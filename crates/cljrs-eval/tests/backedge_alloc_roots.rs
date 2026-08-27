//! IR loop back-edges must not pin per-iteration garbage via ALLOC_ROOTS.
//!
//! Every GC allocation is pushed onto the thread's in-flight alloc-root
//! shadow stack and only truncated when an `AllocRootGuard` drops.  The IR
//! interpreter used to hold a single guard per function invocation, so a
//! long-running `loop` accumulated every iteration's intermediates as GC
//! roots: collections marked them reachable and freed nothing, and a
//! perpetual widget loop grew without bound (observed at 42 GiB RSS).
//!
//! This drives `interpret_ir` on a hot allocating loop with a tiny GC limit
//! so collections fire at the loop's own safepoints, then asserts that the
//! heap object count stays bounded — i.e. mid-loop collections could actually
//! reclaim dead iterations.

#![cfg(not(target_arch = "wasm32"))]
#![cfg(not(feature = "no-gc"))]

use std::sync::Arc;

use cljrs_env::env::GlobalEnv;
use cljrs_eval::{Env, ir_interp::interpret_ir};
use cljrs_ir::lower::lower_fn_body;
use cljrs_reader::{Form, Parser};
use cljrs_value::Value;

fn parse_body(src: &str) -> Vec<Form> {
    let mut p = Parser::new(src.to_string(), "<test>".to_string());
    p.parse_all().expect("parse")
}

fn make_globals() -> Arc<GlobalEnv> {
    cljrs_interp::standard_env_with_paths(None, None, None, Vec::new())
}

/// Lower `body_src` as a zero-arg fn body, then run it through the IR
/// interpreter (Tier 1) — the tier whose loop back-edge is under test.
fn run_ir(globals: &Arc<GlobalEnv>, body_src: &str) -> Value {
    let _mutator = cljrs_gc::register_mutator();
    let body = parse_body(body_src);
    let ir = lower_fn_body(Some("test"), "user", &[], &body, false).expect("lower");

    let mut env = Env::new(globals.clone(), "user");
    let ns_arc: Arc<str> = Arc::from("user");
    cljrs_env::callback::push_eval_context(&env);
    let result = interpret_ir(&ir, vec![], globals, &ns_arc, &mut env);
    cljrs_env::callback::pop_eval_context();
    result.unwrap_or_else(|e| panic!("IR interpret of {body_src:?} failed: {e:?}"))
}

#[test]
fn loop_backedge_releases_iteration_garbage() {
    let globals = make_globals();

    // Small limits so the loop's own gc_safepoints actually collect: soft
    // 4 MB (~40k objects), hard 32 MB.
    cljrs_gc::HEAP.set_config(Arc::new(cljrs_gc::GcConfig::with_limits(
        4 * 1024 * 1024,
        32 * 1024 * 1024,
    )));

    let count_before = cljrs_gc::HEAP.count();

    // 20k iterations, each allocating a vector + boxed ints: with back-edge
    // truncation these die within a few collections; when in-flight roots
    // pin them the heap ends ~400k objects above the baseline.
    let result = run_ir(
        &globals,
        "(loop [i 0]
           (if (< i 20000)
             (let [v [i (+ i 1) (+ i 2)]]
               (recur (+ i 1)))
             i))",
    );
    assert_eq!(result, Value::Long(20000));

    let count_after = cljrs_gc::HEAP.count();
    let growth = count_after.saturating_sub(count_before);
    assert!(
        growth < 150_000,
        "IR loop retained {growth} heap objects past its back-edges \
         (before={count_before}, after={count_after}); in-flight alloc \
         roots are pinning per-iteration garbage"
    );
}

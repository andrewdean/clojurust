//! Async loop back-edges must release per-iteration alloc roots.
//!
//! Every GC allocation is pushed onto the thread's in-flight alloc-root
//! shadow stack (`ALLOC_ROOTS`) and stays a GC root until truncated.  The
//! async evaluator's `loop*` used to run without any per-iteration
//! truncation, so a long-running top-level loop pinned every iteration's
//! intermediates: collections marked them reachable and freed nothing, and
//! a perpetual 5-second polling widget grew to 42 GiB RSS in ~2 hours.
//!
//! This evaluates a hot allocating loop through `eval_async` — the same path
//! `cljrs run` drives for top-level forms — with a tiny GC limit so
//! collections fire mid-loop, then asserts the heap object count stays
//! bounded.

#![cfg(not(feature = "no-gc"))]

use std::sync::Arc;

use cljrs_value::Value;

fn block_on_local<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future)
}

#[test]
fn async_loop_backedge_releases_iteration_garbage() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = cljrs_interp::standard_env_with_paths(None, None, None, Vec::new());
    let mut env = cljrs_env::env::Env::new(globals, "user");

    // Small limits so the loop's own safepoints actually collect: soft 4 MB
    // (~40k objects), hard 32 MB.
    cljrs_gc::HEAP.set_config(Arc::new(cljrs_gc::GcConfig::with_limits(
        4 * 1024 * 1024,
        32 * 1024 * 1024,
    )));

    // `when` is a macro: each iteration re-expands it, allocating symbol /
    // list / metadata-map form nodes — the allocation profile of the widget
    // loop that leaked in production.
    let src = "(loop [i 0]
                 (when (< i 40000)
                   (let [v [i (+ i 1) (+ i 2)]]
                     (recur (+ i 1)))))";
    let mut parser = cljrs_reader::Parser::new(src.to_string(), "<test>".to_string());
    let form = parser.parse_all().expect("parse").pop().expect("one form");

    let count_before = cljrs_gc::HEAP.count();

    let result = block_on_local(async {
        cljrs_async::eval_async::eval_async(&form, &mut env).await
    })
    .expect("loop evaluates");
    assert_eq!(result, Value::Nil);

    // 20k iterations allocate ~100k+ objects; with back-edge truncation they
    // die within a few collections, while pinned in-flight roots leave the
    // heap hundreds of thousands of objects above the baseline.
    let count_after = cljrs_gc::HEAP.count();
    let growth = count_after.saturating_sub(count_before);
    eprintln!("heap growth across loop: {growth} objects (before={count_before}, after={count_after})");
    assert!(
        growth < 150_000,
        "async loop retained {growth} heap objects past its back-edges \
         (before={count_before}, after={count_after}); in-flight alloc \
         roots are pinning per-iteration garbage"
    );
}

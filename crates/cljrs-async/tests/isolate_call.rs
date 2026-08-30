//! Clojure-level tests for the isolate surface (Phase C2):
//! `isolate`, `isolate?`, `isolate-call`, `isolate-close!`, `default-isolate`,
//! and the `pfuture` macro.

use std::sync::Arc;
use std::time::Instant;

use cljrs_async::eval_async::eval_async;
use cljrs_env::env::{Env, GlobalEnv};
use cljrs_env::error::EvalError;
use cljrs_reader::Parser;
use cljrs_value::Value;

const REQUIRE: &str = "(require '[clojure.core.async :refer \
     [isolate isolate? isolate-call isolate-close! default-isolate pfuture join-all]])";

fn async_env() -> Arc<GlobalEnv> {
    let globals = cljrs_interp::standard_env(None, None, None);
    cljrs_async::init(&globals);
    globals
}

fn user_env(globals: Arc<GlobalEnv>) -> Env {
    let mut env = Env::new(globals, "user");
    eval_sync(REQUIRE, &mut env);
    env
}

fn parse_one(src: &str) -> cljrs_reader::Form {
    let mut p = Parser::new(src.to_string(), "<test>".to_string());
    p.parse_all()
        .expect("parse error")
        .into_iter()
        .next()
        .expect("no form")
}

fn eval_sync(src: &str, env: &mut Env) -> Value {
    let mut p = Parser::new(src.to_string(), "<test>".to_string());
    let mut result = Value::Nil;
    for form in p.parse_all().expect("parse error") {
        result = cljrs_interp::eval::eval(&form, env).expect("eval error");
    }
    result
}

#[allow(clippy::result_large_err)] // mirrors cljrs_interp::eval::eval's own signature
fn try_eval(src: &str, env: &mut Env) -> Result<Value, EvalError> {
    cljrs_interp::eval::eval(&parse_one(src), env)
}

async fn eval_await(src: &str, env: &mut Env) -> Result<Value, EvalError> {
    eval_async(&parse_one(src), env).await
}

fn block_on_local<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, f)
}

#[test]
fn isolate_call_runs_qualified_fn() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        assert_eq!(eval_sync("(isolate? iso)", &mut env), Value::Bool(true));
        assert_eq!(eval_sync("(isolate? 42)", &mut env), Value::Bool(false));
        let r = eval_await("(await (isolate-call iso 'clojure.core/+ 20 22))", &mut env)
            .await
            .expect("isolate-call resolves");
        assert_eq!(r, Value::Long(42));
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn isolate_call_roundtrips_composite_data() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        let r = eval_await(
            "(await (isolate-call iso 'clojure.core/assoc {:a 1} :b [2 \"three\"]))",
            &mut env,
        )
        .await
        .expect("isolate-call resolves");
        let expected = eval_sync("{:a 1 :b [2 \"three\"]}", &mut env);
        assert_eq!(r, expected);
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn unqualified_symbol_is_error_at_call_site() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        let err = try_eval("(isolate-call iso 'inc 1)", &mut env)
            .expect_err("unqualified symbol must be rejected");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("fully qualified"),
            "error should demand a qualified symbol, got: {msg}"
        );
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn non_shareable_arg_is_located_error_at_call_site() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        let err = try_eval(
            "(isolate-call iso 'clojure.core/identity (atom 1))",
            &mut env,
        )
        .expect_err("an atom argument must be rejected at the send site");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("isolate-call") && msg.contains("cannot cross"),
            "error should be located at the call site, got: {msg}"
        );
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn closed_isolate_rejects_new_calls() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate)) (isolate-close! iso)", &mut env);
        let err = try_eval("(isolate-call iso 'clojure.core/+ 1 2)", &mut env)
            .expect_err("closed isolate must reject calls");
        assert!(format!("{err:?}").contains("closed"));
    });
}

#[test]
fn worker_error_settles_reply_future_as_error() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        let err = eval_await(
            "(await (isolate-call iso 'clojure.core/definitely-not-a-fn 1))",
            &mut env,
        )
        .await
        .expect_err("unresolvable symbol inside the worker must error the future");
        assert!(
            format!("{err:?}").contains("isolate"),
            "error should name the isolate"
        );
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn non_shareable_result_is_located_error() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso (isolate))", &mut env);
        let err = eval_await("(await (isolate-call iso 'clojure.core/atom 5))", &mut env)
            .await
            .expect_err("an atom result must be rejected at the boundary");
        assert!(
            format!("{err:?}").contains("cannot cross"),
            "error should explain the result cannot cross"
        );
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

#[test]
fn pfuture_runs_on_the_default_pool() {
    // SAFETY: process-global env var; every test that touches the pool size
    // writes the same value, so racing writers are benign.
    unsafe { std::env::set_var("CLJRS_ISOLATE_POOL_SIZE", "1") };
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        let r = eval_await("(await (pfuture (clojure.core/+ 20 22)))", &mut env)
            .await
            .expect("pfuture resolves");
        assert_eq!(r, Value::Long(42));
    });
}

#[test]
fn pfuture_rejects_non_call_and_unqualified_forms() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        for bad in ["(pfuture 42)", "(pfuture (inc 1))", "(pfuture ((fn [] 1)))"] {
            let err = try_eval(bad, &mut env).expect_err("pfuture must reject the form");
            assert!(
                format!("{err:?}").contains("call form"),
                "{bad} should fail at expansion with the call-form message"
            );
        }
    });
}

/// C2 + C4 together: a `shared-atom` holding a map crosses by Arc-clone,
/// is written inside the isolate, and the write is observed by the caller —
/// genuine cross-isolate coordination through the shared tier.
#[test]
fn shared_atom_map_coordinates_across_isolates() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync(
            "(def sa (shared-atom {:done false}))
             (def iso (isolate))",
            &mut env,
        );
        let r = eval_await(
            "(await (isolate-call iso 'clojure.core/reset! sa {:done true :n 42}))",
            &mut env,
        )
        .await
        .expect("cross-isolate reset! resolves");
        let expected = eval_sync("{:done true :n 42}", &mut env);
        assert_eq!(r, expected);
        // The write happened in the worker's heap; the caller sees it through
        // the shared cell, not through the reply copy.
        assert_eq!(eval_sync("(:n @sa)", &mut env), Value::Long(42));
        assert_eq!(eval_sync("(:done @sa)", &mut env), Value::Bool(true));
        eval_sync("(isolate-close! iso)", &mut env);
    });
}

/// A namespace on the caller's source paths is requirable inside the worker,
/// and two isolates genuinely run on separate cores: two concurrent calls take
/// roughly as long as one, not twice as long.
#[test]
fn isolates_require_user_namespaces_and_run_in_parallel() {
    let _mutator = cljrs_gc::register_mutator();

    // A CPU-bound fn in a namespace file the workers must `require`.
    let dir = std::env::temp_dir().join(format!("cljrs-isolate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp source dir");
    std::fs::write(
        dir.join("crunch.cljrs"),
        "(ns crunch)\n\
         (defn burn [n]\n\
           (loop [i 0 acc 0]\n\
             (if (< i n) (recur (+ i 1) (+ acc i)) acc)))\n",
    )
    .expect("write crunch.cljrs");

    let globals = cljrs_interp::standard_env_with_paths(None, None, None, vec![dir.clone()]);
    cljrs_async::init(&globals);

    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def iso-a (isolate)) (def iso-b (isolate))", &mut env);

        // Warm both workers (env build + require) before timing anything.
        for iso in ["iso-a", "iso-b"] {
            let r = eval_await(&format!("(await (isolate-call {iso} 'crunch/burn 10))"), &mut env)
                .await
                .expect("warm-up call resolves");
            assert_eq!(r, Value::Long(45));
        }

        const N: u64 = 400_000;
        let expected = Value::Long((N as i64 - 1) * N as i64 / 2);

        let t = Instant::now();
        let r = eval_await(&format!("(await (isolate-call iso-a 'crunch/burn {N}))"), &mut env)
            .await
            .expect("baseline call resolves");
        let baseline = t.elapsed();
        assert_eq!(r, expected);

        let t = Instant::now();
        let r = eval_await(
            &format!(
                "(await (join-all [(isolate-call iso-a 'crunch/burn {N}) \
                                   (isolate-call iso-b 'crunch/burn {N})]))"
            ),
            &mut env,
        )
        .await
        .expect("parallel calls resolve");
        let parallel = t.elapsed();
        let expected_pair = eval_sync(&format!("[{0} {0}]", (N as i64 - 1) * N as i64 / 2), &mut env);
        assert_eq!(r, expected_pair);

        // Two concurrent runs on two isolates ≈ one run, not two. The 1.6×
        // bound leaves generous room for scheduler noise while still failing
        // hard if the calls serialize (2.0×).
        assert!(
            parallel < baseline * 8 / 5,
            "two concurrent isolate-calls took {parallel:?} vs {baseline:?} \
             for one — the isolates are not running in parallel"
        );

        eval_sync("(isolate-close! iso-a) (isolate-close! iso-b)", &mut env);
        let _ = std::fs::remove_dir_all(&dir);
    });
}

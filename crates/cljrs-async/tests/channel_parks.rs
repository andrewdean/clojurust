//! Channel background tasks must park, not spin.
//!
//! `docs/async-await-spin-bug.md` ("Residual spin loops") listed the
//! `yield_now` loops that survived the `await_value` fix: `mult`'s
//! forwarding task, `onto-chan!`/`to-chan!`'s put loops, `thread-call`'s
//! result put, and `isolate-take!` polling an empty channel.  These tests
//! guard their conversion onto the channel `Notify` / mpsc waker paths the
//! same way `await_parks.rs` guards `await_value`: stall each operation for
//! ~300 ms and require the executor thread to sleep through the stall.  The
//! spin implementations burn the whole interval.

use std::sync::Arc;
use std::time::Duration;

use cljrs_async::eval_async::eval_async;
use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

/// This thread's consumed CPU (utime + stime) in USER_HZ ticks (10 ms).
#[cfg(target_os = "linux")]
fn thread_cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").expect("read thread stat");
    let rest = stat.rsplit_once(')').expect("comm delimiter").1;
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next().expect("utime").parse().expect("utime");
    let stime: u64 = fields.next().expect("stime").parse().expect("stime");
    utime + stime
}

/// Non-Linux: no cheap per-thread CPU clock — the CPU assertions degrade to
/// checking functional completion only.
#[cfg(not(target_os = "linux"))]
fn thread_cpu_ticks() -> u64 {
    0
}

/// 300 ms stall ≈ 30 ticks when spinning; a parked executor stays under 10.
const STALL: Duration = Duration::from_millis(300);
const MAX_STALL_TICKS: u64 = 10;

const REQUIRE: &str = "(require '[clojure.core.async :refer \
     [chan put! poll! onto-chan! mult tap! thread-call \
      isolate-chan isolate-put! isolate-take!]])";

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

fn block_on_local<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, f)
}

/// Poll `src` (a form producing a value or nil) until it yields non-nil.
async fn drain_one(src: &str, env: &mut Env) -> Value {
    loop {
        let v = eval_sync(src, env);
        if v != Value::Nil {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// `onto-chan!` blocked on a full destination must park its put task.
#[test]
fn onto_chan_parks_while_destination_full() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def ch (chan 1))", &mut env);
        // Fills the single buffer slot with 1, then parks trying to put 2.
        eval_sync("(onto-chan! ch [1 2 3])", &mut env);

        let before = thread_cpu_ticks();
        tokio::time::sleep(STALL).await;
        let stall_ticks = thread_cpu_ticks() - before;

        // Draining lets the parked task finish; values arrive in order.
        for expected in 1..=3 {
            assert_eq!(
                drain_one("(poll! ch)", &mut env).await,
                Value::Long(expected)
            );
        }
        assert!(
            stall_ticks < MAX_STALL_TICKS,
            "onto-chan! burned {} ms of CPU stalled on a full channel — \
             the put loop is spinning instead of parking",
            stall_ticks * 10
        );
    });
}

/// `mult`'s forwarding task blocked on an unconsumed rendezvous tap must park.
#[test]
fn mult_parks_while_tap_unconsumed() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync(
            "(def src (chan 1))
             (def m (mult src))
             (def tap-ch (chan))
             (tap! m tap-ch)
             (put! src 42)",
            &mut env,
        );
        // Let the forwarding task pick up 42 and offer it to the rendezvous
        // tap, where it stalls: nobody takes for the whole interval.
        tokio::task::yield_now().await;
        let before = thread_cpu_ticks();
        tokio::time::sleep(STALL).await;
        let stall_ticks = thread_cpu_ticks() - before;

        assert_eq!(drain_one("(poll! tap-ch)", &mut env).await, Value::Long(42));
        assert!(
            stall_ticks < MAX_STALL_TICKS,
            "mult burned {} ms of CPU stalled on an unconsumed tap — \
             the forwarding task is spinning instead of parking",
            stall_ticks * 10
        );
    });
}

/// `(await (isolate-take! rx))` on an empty isolate channel must park until
/// the put arrives.
#[test]
fn isolate_take_parks_until_put() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals.clone());
        eval_sync("(def ich (isolate-chan))", &mut env);

        // Deliver the value 300 ms later from a sibling task (same isolate,
        // fresh Env over the same globals — the var is in `user`).
        let putter_globals = globals.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(STALL).await;
            let mut env = Env::new(putter_globals, "user");
            eval_sync("(isolate-put! (first ich) 7)", &mut env);
        });

        let before = thread_cpu_ticks();
        let r = eval_async(&parse_one("(await (isolate-take! (second ich)))"), &mut env)
            .await
            .expect("isolate-take! resolves");
        let stall_ticks = thread_cpu_ticks() - before;

        assert_eq!(r, Value::Long(7));
        assert!(
            stall_ticks < MAX_STALL_TICKS,
            "isolate-take! burned {} ms of CPU waiting on an empty channel — \
             it is polling instead of parking on the mpsc waker",
            stall_ticks * 10
        );
    });
}

/// `thread-call` still delivers its result through the 1-buffer channel.
#[test]
fn thread_call_delivers_result() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = user_env(globals);
        eval_sync("(def rc (thread-call (fn [] (+ 1 2))))", &mut env);
        assert_eq!(drain_one("(poll! rc)", &mut env).await, Value::Long(3));
    });
}

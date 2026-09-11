//! The memory-pressure coordinator seen from async code and from Clojure
//! (isolates plan C5): the watch mirror follows GC-side transitions, a
//! parked waiter wakes when the level drops, and the Clojure builtins
//! report and await the level.
//!
//! These tests drive the process-global level through `set_budget` and
//! `add_live`/`sub_live`, so they run under one mutex to keep them from
//! interleaving with each other; every test restores a zero budget (the
//! coordinator off) before it returns.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cljrs_async::eval_async::eval_async;
use cljrs_async::pressure::{self, PressureLevel};
use cljrs_env::env::{Env, GlobalEnv};
use cljrs_gc::pressure as gc_pressure;
use cljrs_reader::Parser;
use cljrs_value::Value;

static SERIAL: Mutex<()> = Mutex::new(());

/// A budget no test binary's own allocations can approach, so only the
/// explicit `add_live`/`sub_live` calls below move the level.
const BUDGET: usize = 1 << 40;

struct BudgetGuard;
impl Drop for BudgetGuard {
    fn drop(&mut self) {
        gc_pressure::sub_live(usize::MAX);
        gc_pressure::set_budget(0);
    }
}

fn serial() -> (std::sync::MutexGuard<'static, ()>, BudgetGuard) {
    let lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    gc_pressure::sub_live(usize::MAX);
    gc_pressure::set_budget(BUDGET);
    (lock, BudgetGuard)
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
fn watch_mirror_follows_gc_transitions() {
    let (_lock, _budget) = serial();
    let mut rx = pressure::subscribe();
    assert_eq!(*rx.borrow_and_update(), PressureLevel::Green);
    gc_pressure::add_live(BUDGET / 100 * 92);
    assert_eq!(pressure::level(), PressureLevel::Red);
    assert_eq!(*rx.borrow_and_update(), PressureLevel::Red);
    gc_pressure::sub_live(BUDGET);
    assert_eq!(*rx.borrow_and_update(), PressureLevel::Green);
}

#[test]
fn waiter_parks_under_red_and_wakes_when_it_clears() {
    let (_lock, _budget) = serial();
    gc_pressure::add_live(BUDGET / 100 * 92);
    assert_eq!(pressure::level(), PressureLevel::Red);

    let released = Arc::new(Mutex::new(false));
    let released2 = released.clone();
    block_on_local(async move {
        let waiter = tokio::task::spawn_local(async move {
            pressure::wait_until_below(PressureLevel::Red).await;
            *released2.lock().unwrap() = true;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !*released.lock().unwrap(),
            "waiter must stay parked while the level is Red"
        );
        // Drop below the 85% Red exit but stay above the 70% Yellow exit.
        gc_pressure::sub_live(BUDGET / 100 * 12);
        assert_eq!(pressure::level(), PressureLevel::Yellow);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter woke after the level dropped below Red")
            .expect("waiter task completed");
        assert!(*released.lock().unwrap());
    });
}

fn async_env() -> Arc<GlobalEnv> {
    let globals = cljrs_interp::standard_env(None, None, None);
    cljrs_async::init(&globals);
    globals
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
    cljrs_interp::eval::eval(&parse_one(src), env).expect("eval error")
}

#[test]
fn clojure_builtins_report_and_await_the_level() {
    let _mutator = cljrs_gc::register_mutator();
    let (_lock, _budget) = serial();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync(
            "(require '[clojure.core.async :refer [memory-pressure memory-pressure-below]])",
            &mut env,
        );
        assert_eq!(
            eval_sync("(memory-pressure)", &mut env).to_string(),
            ":green"
        );

        gc_pressure::add_live(BUDGET / 100 * 80);
        assert_eq!(
            eval_sync("(memory-pressure)", &mut env).to_string(),
            ":yellow"
        );
        // Already below Red: resolves without parking, with the current level.
        let v = eval_async(&parse_one("(await (memory-pressure-below :red))"), &mut env)
            .await
            .expect("await");
        assert_eq!(v.to_string(), ":yellow");

        let err = cljrs_interp::eval::eval(&parse_one("(memory-pressure-below :purple)"), &mut env)
            .expect_err("unknown level must be rejected");
        assert!(err.to_string().contains(":green, :yellow, :red"), "{err}");
    });
}

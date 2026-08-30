//! Clojure-level tests for agents (Phase C3): `agent`, `send`, `send-off`,
//! `(await agent)`, `agent-error`, `restart-agent`, and agent watches.
//!
//! Agents are serial async mailboxes on the isolate's LocalSet — cooperative
//! like `future`, no OS threads (docs/user-reachable-isolates-plan.md, D3).

use std::sync::Arc;

use cljrs_async::eval_async::eval_async;
use cljrs_env::env::{Env, GlobalEnv};
use cljrs_env::error::EvalError;
use cljrs_reader::Parser;
use cljrs_value::Value;

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
fn send_applies_actions_serially_in_order() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync("(def a (agent []))", &mut env);
        // Enqueued before any drain runs; FIFO order must be preserved.
        let ret = eval_sync(
            "(do (send a conj :first) (send-off a conj :second) (send a conj :third))",
            &mut env,
        );
        assert!(matches!(ret, Value::Agent(_)), "send returns the agent");
        let awaited = eval_await("(await a)", &mut env).await.expect("await");
        assert_eq!(awaited, Value::Nil, "await returns nil");
        let state = eval_sync("@a", &mut env);
        let expected = eval_sync("[:first :second :third]", &mut env);
        assert_eq!(state, expected);
        assert_eq!(eval_sync("(agent-error a)", &mut env), Value::Nil);
    });
}

#[test]
fn failed_agent_reports_error_rejects_sends_and_restarts() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync("(def a (agent 0))", &mut env);
        // The failing action and a pending one behind it, queued together.
        eval_sync(
            "(do (send a (fn [s] (throw (ex-info \"boom\" {:s s})))) (send a + 5))",
            &mut env,
        );
        eval_await("(await a)", &mut env)
            .await
            .expect("await on failing agent");

        // Failed: error observable, state unchanged, new sends rejected.
        assert_ne!(eval_sync("(agent-error a)", &mut env), Value::Nil);
        assert_eq!(eval_sync("@a", &mut env), Value::Long(0));
        let err = try_eval("(send a inc)", &mut env).expect_err("send to failed agent");
        assert!(format!("{err:?}").contains("restart-agent"));

        // Restart resumes the still-queued (+ 5) action on the new state.
        eval_sync("(restart-agent a 10)", &mut env);
        eval_await("(await a)", &mut env)
            .await
            .expect("await after restart");
        assert_eq!(eval_sync("@a", &mut env), Value::Long(15));
        assert_eq!(eval_sync("(agent-error a)", &mut env), Value::Nil);
    });
}

#[test]
fn restart_agent_clear_actions_drops_the_queue() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync("(def a (agent 0))", &mut env);
        eval_sync(
            "(do (send a (fn [s] (throw (ex-info \"boom\" {}))))
                 (send a + 5))",
            &mut env,
        );
        eval_await("(await a)", &mut env).await.expect("await");
        eval_sync("(restart-agent a 10 :clear-actions true)", &mut env);
        eval_await("(await a)", &mut env)
            .await
            .expect("await after restart");
        assert_eq!(eval_sync("@a", &mut env), Value::Long(10));
    });
}

#[test]
fn restart_of_healthy_agent_errors() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync("(def a (agent 0))", &mut env);
        let err = try_eval("(restart-agent a 1)", &mut env).expect_err("healthy agent");
        assert!(format!("{err:?}").contains("does not need a restart"));
    });
}

#[test]
fn agent_watches_fire_on_state_change() {
    let _mutator = cljrs_gc::register_mutator();
    let globals = async_env();
    block_on_local(async move {
        let mut env = Env::new(globals, "user");
        eval_sync(
            "(def seen (atom []))
             (def a (agent 0))
             (add-watch a :w (fn [k r old new] (swap! seen conj [old new])))
             (send a + 1)
             (send a + 10)",
            &mut env,
        );
        eval_await("(await a)", &mut env).await.expect("await");
        let seen = eval_sync("@seen", &mut env);
        let expected = eval_sync("[[0 1] [1 11]]", &mut env);
        assert_eq!(seen, expected);
    });
}

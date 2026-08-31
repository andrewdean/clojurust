//! Agents without the async runtime (AOT binaries, embedders that never call
//! `cljrs_async::init`): `send` drains the mailbox synchronously at the send
//! site, so state is settled by the time it returns. This is the environment
//! the compiled clojure-test-suite binary runs in — `add-watch`/`remove-watch`
//! core tests exercise agents there with no executor installed.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_src(src: &str, env: &mut Env) -> Value {
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, env).expect("eval error");
    }
    result
}

#[test]
fn send_without_async_runtime_applies_synchronously() {
    let mut env = make_env().1;
    eval_src("(def a (agent 0))", &mut env);
    let ret = eval_src("(send a + 5)", &mut env);
    assert!(matches!(ret, Value::Agent(_)), "send returns the agent");
    // No executor: the action has already been applied when send returns.
    assert_eq!(eval_src("@a", &mut env), Value::Long(5));
    eval_src("(send-off a + 10)", &mut env);
    assert_eq!(eval_src("@a", &mut env), Value::Long(15));
    assert_eq!(eval_src("(agent-error a)", &mut env), Value::Nil);
}

#[test]
fn agent_watches_fire_synchronously_without_runtime() {
    // The clojure.core-test.add-watch / remove-watch shape.
    let mut env = make_env().1;
    eval_src(
        "(def seen (atom []))
         (def a (agent 0))
         (add-watch a :w (fn [k r old new] (swap! seen conj [old new])))
         (send a + 1)
         (send a + 10)",
        &mut env,
    );
    let seen = eval_src("@seen", &mut env);
    let expected = eval_src("[[0 1] [1 11]]", &mut env);
    assert_eq!(seen, expected);
    eval_src("(remove-watch a :w) (send a + 100)", &mut env);
    assert_eq!(eval_src("(count @seen)", &mut env), Value::Long(2));
    assert_eq!(eval_src("@a", &mut env), Value::Long(111));
}

#[test]
fn failed_agent_without_runtime_keeps_queue_and_restarts() {
    let mut env = make_env().1;
    eval_src("(def a (agent 0))", &mut env);
    eval_src("(send a (fn [s] (throw (ex-info \"boom\" {}))))", &mut env);
    assert_ne!(eval_src("(agent-error a)", &mut env), Value::Nil);
    assert_eq!(eval_src("@a", &mut env), Value::Long(0));
    eval_src("(restart-agent a 10) (send a + 5)", &mut env);
    assert_eq!(eval_src("@a", &mut env), Value::Long(15));
}

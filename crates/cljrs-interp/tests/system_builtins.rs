//! `System/*` host builtins and `*command-line-args*` (cljrs-builtins
//! `system` module): env vars, JVM-style properties, and the uncatchable
//! `System/exit` control signal.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_env::error::EvalError;
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_in(env: &mut Env, src: &str) -> Result<Value, EvalError> {
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, env)?;
    }
    Ok(result)
}

#[test]
fn getenv_reads_a_variable() {
    // Set our own variable so the test controls the value.
    unsafe { std::env::set_var("CLJRS_SYSTEM_TEST_VAR", "hello") };
    let (_, mut env) = make_env();
    let v = eval_in(&mut env, "(System/getenv \"CLJRS_SYSTEM_TEST_VAR\")").unwrap();
    assert_eq!(v, Value::string("hello".to_string()));
    let missing = eval_in(&mut env, "(System/getenv \"CLJRS_SYSTEM_TEST_NOPE\")").unwrap();
    assert_eq!(missing, Value::Nil);
}

#[test]
fn getenv_zero_arity_returns_map() {
    unsafe { std::env::set_var("CLJRS_SYSTEM_TEST_MAP", "42") };
    let (_, mut env) = make_env();
    let v = eval_in(&mut env, "(get (System/getenv) \"CLJRS_SYSTEM_TEST_MAP\")").unwrap();
    assert_eq!(v, Value::string("42".to_string()));
}

#[test]
fn properties_defaults_and_overrides() {
    let (_, mut env) = make_env();
    if let Ok(home) = std::env::var("HOME") {
        let v = eval_in(&mut env, "(System/getProperty \"user.home\")").unwrap();
        assert_eq!(v, Value::string(home));
    }
    let v = eval_in(&mut env, "(System/getProperty \"no.such.prop\" \"dflt\")").unwrap();
    assert_eq!(v, Value::string("dflt".to_string()));
    let v = eval_in(
        &mut env,
        "(System/setProperty \"cljrs.test.prop\" \"x\") (System/getProperty \"cljrs.test.prop\")",
    )
    .unwrap();
    assert_eq!(v, Value::string("x".to_string()));
}

#[test]
fn exit_is_not_catchable() {
    let (_, mut env) = make_env();
    let err = eval_in(
        &mut env,
        "(try (System/exit 3) (catch :default e :caught))",
    )
    .expect_err("System/exit must unwind past catch");
    match err {
        EvalError::Exit(code) => assert_eq!(code, 3),
        other => panic!("expected EvalError::Exit(3), got {other:?}"),
    }
}

#[test]
fn command_line_args_bound_by_host() {
    let (globals, mut env) = make_env();
    assert_eq!(eval_in(&mut env, "*command-line-args*").unwrap(), Value::Nil);
    cljrs_builtins::system::set_command_line_args(
        &globals,
        &["a".to_string(), "b".to_string()],
    );
    let v = eval_in(&mut env, "(vec *command-line-args*)").unwrap();
    let expected = eval_in(&mut env, "[\"a\" \"b\"]").unwrap();
    assert_eq!(v, expected);
}

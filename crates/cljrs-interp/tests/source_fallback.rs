//! The embedder-installed source fallback (`GlobalEnv::set_source_fallback`)
//! must serve `require`d namespaces that are neither builtin sources nor files
//! on the source path — the seam cljrsh uses for dependency caches and
//! pod-backed namespaces.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_in(env: &mut Env, src: &str) -> Value {
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, env).expect("eval error");
    }
    result
}

#[test]
fn require_resolves_through_source_fallback() {
    let (globals, mut env) = make_env();
    globals.set_source_fallback(Arc::new(|ns| {
        (ns == "fallback.lib").then(|| {
            (
                "(ns fallback.lib) (defn answer [] 42)".to_string(),
                "<fallback:fallback.lib>".to_string(),
            )
        })
    }));
    let result = eval_in(
        &mut env,
        "(require '[fallback.lib :as fl]) (fl/answer)",
    );
    assert_eq!(result, Value::Long(42));
}

#[test]
fn fallback_miss_still_errors() {
    let (globals, mut env) = make_env();
    globals.set_source_fallback(Arc::new(|_| None));
    let mut parser = Parser::new("(require 'no.such.ns)".to_string(), "<test>".to_string());
    let form = parser.parse_one().expect("parse").expect("form");
    let err = cljrs_interp::eval::eval(&form, &mut env).expect_err("require should fail");
    assert!(
        err.to_string().contains("no.such.ns"),
        "error should name the namespace: {err}"
    );
}

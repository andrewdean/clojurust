//! `#inst` / `Value::Instant` (cljrsh milestone A6): reading, printing,
//! equality, ordering, and the constructor builtins.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_src(src: &str) -> Value {
    let (_, mut env) = make_env();
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, &mut env).expect("eval error");
    }
    result
}

#[test]
fn inst_literal_reads_to_instant() {
    assert_eq!(
        eval_src("#inst \"1970-01-01T00:00:01Z\""),
        Value::Instant(1000)
    );
    // Offset applies.
    assert_eq!(
        eval_src("#inst \"1970-01-01T02:00:00+02:00\""),
        Value::Instant(0)
    );
}

#[test]
fn inst_prints_readably_and_roundtrips() {
    let printed = eval_src("(pr-str #inst \"2026-08-21T12:34:56.789Z\")");
    assert_eq!(
        printed,
        Value::string("#inst \"2026-08-21T12:34:56.789-00:00\"".to_string())
    );
    assert_eq!(
        eval_src("(= #inst \"2026-08-21T12:34:56.789Z\" (read-string (pr-str #inst \"2026-08-21T12:34:56.789Z\")))"),
        Value::Bool(true)
    );
}

#[test]
fn instant_builtins() {
    assert_eq!(
        eval_src("(instant-ms (instant \"2020-01-01T00:00:00Z\"))"),
        Value::Long(1_577_836_800_000)
    );
    assert_eq!(eval_src("(instant? (instant-now))"), Value::Bool(true));
    assert_eq!(eval_src("(instant? 5)"), Value::Bool(false));
    assert_eq!(eval_src("(instant 42)"), Value::Instant(42));
}

#[test]
fn instants_sort_chronologically() {
    assert_eq!(
        eval_src("(vec (sort [#inst \"2026-01-02\" #inst \"2025-06-01\" #inst \"2026-01-01\"]))"),
        eval_src("[#inst \"2025-06-01\" #inst \"2026-01-01\" #inst \"2026-01-02\"]")
    );
    assert_eq!(
        eval_src("(compare #inst \"2025-01-01\" #inst \"2026-01-01\")"),
        Value::Long(-1)
    );
}

#[test]
fn instant_crosses_isolate_serialization() {
    let v = Value::Instant(123_456);
    let wire = cljrs_value::clone::serialize(&v).expect("serialize");
    assert_eq!(cljrs_value::clone::deserialize(wire), v);
}

#[test]
fn bad_inst_is_an_error() {
    let (_, mut env) = make_env();
    let mut parser = Parser::new("#inst \"not-a-date\"".to_string(), "<test>".to_string());
    let form = parser.parse_one().unwrap().unwrap();
    assert!(cljrs_interp::eval::eval(&form, &mut env).is_err());
}

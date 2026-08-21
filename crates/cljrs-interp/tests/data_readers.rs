//! `*data-readers*` / `*default-data-reader-fn*` and `clojure.edn`
//! (cljrsh milestone A5).

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_src(src: &str) -> Result<Value, cljrs_env::error::EvalError> {
    let (_, mut env) = make_env();
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, &mut env)?;
    }
    Ok(result)
}

#[test]
fn data_readers_binding_applies() {
    let v = eval_src(
        "(binding [*data-readers* {'my/tag (fn [v] [:tagged v])}] #my/tag 42)",
    )
    .unwrap();
    assert_eq!(v, eval_src("[:tagged 42]").unwrap());
}

#[test]
fn default_data_reader_fn_catches_unknown() {
    let v = eval_src(
        "(binding [*default-data-reader-fn* (fn [t v] [:unknown t v])] #any/x 1)",
    )
    .unwrap();
    assert_eq!(v, eval_src("[:unknown 'any/x 1]").unwrap());
}

#[test]
fn unknown_tag_without_readers_errors() {
    let err = eval_src("#nope/nope 1").expect_err("should error");
    assert!(err.to_string().contains("No reader function for tag"));
}

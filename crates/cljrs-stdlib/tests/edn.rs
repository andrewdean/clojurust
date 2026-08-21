//! `clojure.edn/read-string` with :readers/:default/:eof (cljrsh milestone A5).

use cljrs_env::env::Env;
use cljrs_reader::Parser;
use cljrs_value::Value;

fn eval_src(src: &str) -> Value {
    let globals = cljrs_stdlib::standard_env();
    let mut env = Env::new(globals, "user");
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, &mut env).expect("eval error");
    }
    result
}

#[test]
fn edn_read_string_basics_and_readers() {
    let v = eval_src(
        "(require '[clojure.edn :as edn])
         [(edn/read-string \"{:a [1 2]}\")
          (edn/read-string {:readers {'my/t (fn [v] (inc v))}} \"#my/t 41\")
          (edn/read-string {:default (fn [t v] [t v])} \"#u/k 7\")
          (edn/read-string {:eof :done} \"\")
          (edn/read-string \"#inst \\\"1970-01-01T00:00:01Z\\\"\")]",
    );
    let expected = eval_src("[{:a [1 2]} 42 ['u/k 7] :done #inst \"1970-01-01T00:00:01Z\"]");
    assert_eq!(v, expected);
}

#[test]
fn edn_read_string_never_evaluates() {
    let v = eval_src("(require '[clojure.edn :as edn]) (edn/read-string \"(+ 1 2)\")");
    assert_eq!(v, eval_src("'(+ 1 2)"));
}

#[test]
fn edn_unknown_tag_throws() {
    let globals = cljrs_stdlib::standard_env();
    let mut env = Env::new(globals, "user");
    let src = "(require '[clojure.edn :as edn]) (edn/read-string \"#no/reader 1\")";
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().unwrap();
    let mut result = Ok(Value::Nil);
    for form in forms {
        result = cljrs_interp::eval::eval(&form, &mut env);
    }
    assert!(result.is_err());
}

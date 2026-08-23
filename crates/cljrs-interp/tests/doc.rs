//! `doc` / `doc-data`: docstrings on `def`, `defn`, `defmacro`, and native
//! builtins are captured as `:doc` (and `:arglists`) var metadata. `doc`
//! prints the Clojure-style block (name, arglists, docstring) and returns
//! nil, so these tests capture it with `with-out-str`.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_all(env: &mut Env, src: &str) -> Value {
    let mut parser = Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, env).expect("eval error");
    }
    result
}

#[test]
fn defn_docstring_is_readable_via_doc() {
    let (_, mut env) = make_env();
    let result = eval_all(
        &mut env,
        r#"(defn add1 "Adds one to x." [x] (+ x 1)) (with-out-str (doc add1))"#,
    );
    let Value::Str(s) = result else {
        panic!("expected printed doc, got {result:?}");
    };
    let printed = s.get();
    assert!(printed.contains("user/add1"), "missing name: {printed}");
    assert!(printed.contains("[x]"), "missing arglists: {printed}");
    assert!(printed.contains("Adds one to x."), "missing doc: {printed}");
}

#[test]
fn def_docstring_is_readable_via_doc() {
    let (_, mut env) = make_env();
    let result = eval_all(
        &mut env,
        r#"(def answer "The answer." 42) (with-out-str (doc answer))"#,
    );
    let Value::Str(s) = result else {
        panic!("expected printed doc, got {result:?}");
    };
    assert!(s.get().contains("The answer."), "missing doc: {}", s.get());
}

#[test]
fn defmacro_docstring_is_readable_via_doc() {
    let (_, mut env) = make_env();
    let result = eval_all(
        &mut env,
        r#"(defmacro my-when "Like when." [test & body] (list 'if test (cons 'do body) nil))
           (with-out-str (doc my-when))"#,
    );
    let Value::Str(s) = result else {
        panic!("expected printed doc, got {result:?}");
    };
    let printed = s.get();
    assert!(printed.contains("Like when."), "missing doc: {printed}");
    assert!(printed.contains("Macro"), "missing Macro marker: {printed}");
}

#[test]
fn doc_returns_nil_for_unresolvable_symbol() {
    let (_, mut env) = make_env();
    let result = eval_all(&mut env, "(doc definitely-not-a-thing-xyz)");
    assert_eq!(result, Value::Nil);
}

#[test]
fn doc_works_on_native_builtins() {
    let (_, mut env) = make_env();
    let result = eval_all(&mut env, "(with-out-str (doc +))");
    let Value::Str(s) = result else {
        panic!("expected printed doc, got {result:?}");
    };
    assert!(s.get().contains("sum"));
}

#[test]
fn defn_without_docstring_has_no_doc() {
    let (_, mut env) = make_env();
    let result = eval_all(&mut env, "(defn plain [x] x) (doc plain)");
    assert_eq!(result, Value::Nil);
}

#[test]
fn doc_data_reports_doc_and_arities_for_defn() {
    let (_, mut env) = make_env();
    let result = eval_all(
        &mut env,
        r#"(defn add
             "Adds two numbers."
             ([x] x)
             ([x y] (+ x y)))
           (doc-data (var add))"#,
    );
    let Value::Map(m) = result else {
        panic!("expected a map, got {result:?}");
    };
    let doc = m
        .get(&Value::keyword(cljrs_value::Keyword::parse("doc")))
        .expect("expected :doc key");
    assert_eq!(doc, Value::string("Adds two numbers.".to_string()));

    let arities = m
        .get(&Value::keyword(cljrs_value::Keyword::parse("arities")))
        .expect("expected :arities key");
    let Value::Vector(v) = arities else {
        panic!("expected a vector of arities, got {arities:?}");
    };
    assert_eq!(v.get().count(), 2);
}

#[test]
fn doc_data_on_macro_hides_implicit_form_and_env_params() {
    let (_, mut env) = make_env();
    let result = eval_all(
        &mut env,
        r#"(defmacro my-macro "does a thing" [x] x)
           (doc-data (var my-macro))"#,
    );
    let Value::Map(m) = result else {
        panic!("expected a map, got {result:?}");
    };
    let arities = m
        .get(&Value::keyword(cljrs_value::Keyword::parse("arities")))
        .expect("expected :arities key");
    let Value::Vector(v) = arities else {
        panic!("expected a vector of arities, got {arities:?}");
    };
    let first = v
        .get()
        .nth(0)
        .cloned()
        .expect("expected at least one arity");
    let Value::Vector(first_arity) = first else {
        panic!("expected first arity to be a vector");
    };
    // Only `x`, not the implicit `&form`/`&env`.
    assert_eq!(first_arity.get().count(), 1);
}

#[test]
fn doc_data_on_native_fn_reports_synthetic_arities() {
    let (_, mut env) = make_env();
    let result = eval_all(&mut env, "(doc-data (var cons))");
    let Value::Map(m) = result else {
        panic!("expected a map, got {result:?}");
    };
    let arities = m
        .get(&Value::keyword(cljrs_value::Keyword::parse("arities")))
        .expect("expected :arities key");
    assert_ne!(arities, Value::Nil);
}

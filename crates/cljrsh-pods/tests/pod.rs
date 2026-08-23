//! End-to-end pod protocol tests against the bundled cljrsh-test-pod binary.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_value::Value;

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_stdlib::standard_env();
    cljrsh_pods::init(&globals);
    let env = Env::new(globals.clone(), "user");
    (globals, env)
}

fn eval_in(env: &mut Env, src: &str) -> Result<Value, cljrs_env::error::EvalError> {
    let mut parser = cljrs_reader::Parser::new(src.to_string(), "<test>".to_string());
    let forms = parser.parse_all().expect("parse error");
    let mut result = Value::Nil;
    for form in forms {
        result = cljrs_interp::eval::eval(&form, env)?;
    }
    Ok(result)
}

fn load_pod_form() -> String {
    format!(
        "(def pod (cljrsh.pods/load-pod \"{}\"))",
        env!("CARGO_BIN_EXE_cljrsh-test-pod")
    )
}

#[test]
fn sync_invoke_and_code_vars() {
    let (_, mut env) = make_env();
    eval_in(&mut env, &load_pod_form()).unwrap();
    assert_eq!(
        eval_in(&mut env, "(pod.test-pod/add-sync 1 2 39)").unwrap(),
        Value::Long(42)
    );
    // A "code" var evaluated client-side in the pod namespace.
    assert_eq!(
        eval_in(&mut env, "(pod.test-pod/from-code)").unwrap(),
        eval_in(&mut env, ":evaluated-client-side").unwrap()
    );
    // require of a pod namespace is a no-op (already loaded).
    assert_eq!(
        eval_in(
            &mut env,
            "(require '[pod.test-pod :as tp]) (tp/add-sync 20 22)"
        )
        .unwrap(),
        Value::Long(42)
    );
}

#[test]
fn pod_error_becomes_ex_info() {
    let (_, mut env) = make_env();
    eval_in(&mut env, &load_pod_form()).unwrap();
    let v = eval_in(
        &mut env,
        "(try (pod.test-pod/error-fn)
              (catch Exception e [(ex-message e) (:pod-var (ex-data e))]))",
    )
    .unwrap();
    assert_eq!(
        v,
        eval_in(&mut env, "[\"pod exploded\" :error-fn]").unwrap()
    );
}

#[test]
fn out_forwarding_and_unload() {
    let (_, mut env) = make_env();
    eval_in(&mut env, &load_pod_form()).unwrap();
    // print-fn emits an out reply then a value; out goes to our stdout
    // (visible with --nocapture), the value comes back.
    assert_eq!(
        eval_in(&mut env, "(pod.test-pod/print-fn)").unwrap(),
        eval_in(&mut env, ":printed").unwrap()
    );
    assert_eq!(
        eval_in(&mut env, "(cljrsh.pods/unload-pod pod)").unwrap(),
        Value::Nil
    );
    // After shutdown, invoking errors instead of hanging.
    assert!(eval_in(&mut env, "(pod.test-pod/add-sync 1)").is_err());
}

#[test]
fn babashka_pods_veneer() {
    let (_, mut env) = make_env();
    let v = eval_in(
        &mut env,
        &format!(
            "(require '[babashka.pods :as pods])
             (pods/load-pod \"{}\")
             (pod.test-pod/add-sync 2 40)",
            env!("CARGO_BIN_EXE_cljrsh-test-pod")
        ),
    )
    .unwrap();
    assert_eq!(v, Value::Long(42));
}

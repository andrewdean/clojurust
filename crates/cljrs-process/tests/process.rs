//! End-to-end tests for the `cljrs.process` namespace through the interpreter.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::{Keyword, Value};

fn make_env() -> (Arc<GlobalEnv>, Env) {
    let globals = cljrs_interp::standard_env(None, None, None);
    cljrs_process::init(&globals);
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

fn get_kw(map: &Value, name: &str) -> Value {
    let Value::Map(m) = map else {
        panic!("expected map, got {}", map.type_name());
    };
    m.get(&Value::keyword(Keyword::simple(name)))
        .unwrap_or(Value::Nil)
}

#[test]
fn sh_captures_output_and_exit() {
    let (_, mut env) = make_env();
    let r = eval_in(&mut env, "(cljrs.process/sh \"echo\" \"hello\")");
    assert_eq!(get_kw(&r, "exit"), Value::Long(0));
    assert_eq!(get_kw(&r, "out"), Value::string("hello\n".to_string()));
}

#[test]
fn sh_pipes_stdin_and_reports_failure_exit() {
    let (_, mut env) = make_env();
    let r = eval_in(&mut env, "(cljrs.process/sh \"cat\" :in \"piped\")");
    assert_eq!(get_kw(&r, "out"), Value::string("piped".to_string()));
    let r = eval_in(&mut env, "(cljrs.process/sh \"false\")");
    assert_eq!(get_kw(&r, "exit"), Value::Long(1));
}

#[test]
fn sh_honors_dir_and_env() {
    let (_, mut env) = make_env();
    let r = eval_in(&mut env, "(cljrs.process/sh \"pwd\" :dir \"/tmp\")");
    let Value::Str(s) = get_kw(&r, "out") else {
        panic!("expected string out")
    };
    assert_eq!(s.get().trim(), "/tmp");
    let r = eval_in(
        &mut env,
        "(cljrs.process/sh \"sh\" \"-c\" \"printf %s $CLJRS_PROC_TEST\" :extra-env {\"CLJRS_PROC_TEST\" \"yes\"})",
    );
    assert_eq!(get_kw(&r, "out"), Value::string("yes".to_string()));
}

#[test]
fn spawn_wait_alive_lifecycle() {
    let (_, mut env) = make_env();
    let r = eval_in(
        &mut env,
        "(let [p (cljrs.process/spawn [\"sh\" \"-c\" \"sleep 0.2; echo done\"])]
           [(cljrs.process/alive? p)
            (:out (cljrs.process/wait p))
            (cljrs.process/alive? p)
            (cljrs.process/exit-code p)])",
    );
    let expected = eval_in(&mut env, "[true \"done\\n\" false 0]");
    assert_eq!(r, expected);
}

#[test]
fn destroy_kills_the_child() {
    let (_, mut env) = make_env();
    let r = eval_in(
        &mut env,
        "(let [p (cljrs.process/spawn [\"sleep\" \"30\"])]
           (cljrs.process/destroy p)
           (:exit (cljrs.process/wait p)))",
    );
    // Killed by signal → no exit code; wait reports -1.
    assert_eq!(r, Value::Long(-1));
}

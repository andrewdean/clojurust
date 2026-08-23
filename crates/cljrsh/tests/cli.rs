//! End-to-end CLI tests for the cljrsh binary (M0 surface): programs, args,
//! exit-code conventions, shebang, preloads, stdin, reader conditionals.

use std::io::Write;
use std::process::{Command, Stdio};

fn cljrsh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cljrsh"))
}

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Outcome {
    let mut cmd = cljrsh();
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn cljrsh");
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("wait cljrsh");
    Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

#[test]
fn eval_prints_non_nil_result() {
    let r = run(&["-e", "(+ 1 2)"], None, &[]);
    assert_eq!(r.stdout, "3\n");
    assert_eq!(r.code, 0);
}

#[test]
fn eval_nil_prints_nothing() {
    let r = run(&["-e", "nil"], None, &[]);
    assert_eq!(r.stdout, "");
    assert_eq!(r.code, 0);
}

#[test]
fn command_line_args_after_expr() {
    let r = run(
        &["-e", "(pr (vec *command-line-args*))", "a", "b"],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[\"a\" \"b\"]");
}

#[test]
fn double_dash_separates_args() {
    let r = run(&["-e", "(pr (vec *command-line-args*))", "--", "-e"], None, &[]);
    assert_eq!(r.stdout, "[\"-e\"]");
}

#[test]
fn system_exit_code_propagates_silently() {
    let r = run(&["-e", "(System/exit 7)"], None, &[]);
    assert_eq!(r.code, 7);
    assert_eq!(r.stderr, "");
}

#[test]
fn babashka_exit_convention() {
    let r = run(
        &["-e", "(throw (ex-info \"clean death\" {:babashka/exit 5}))"],
        None,
        &[],
    );
    assert_eq!(r.code, 5);
    assert_eq!(r.stderr, "clean death\n");
    // No stack trace / unhandled banner.
    assert!(!r.stderr.contains("unhandled"));
}

#[test]
fn uncaught_throw_exits_one_with_report() {
    let r = run(&["-e", "(throw (ex-info \"boom\" {}))"], None, &[]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("boom"), "stderr: {}", r.stderr);
}

#[test]
fn script_file_with_shebang_and_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.clj");
    std::fs::write(
        &path,
        "#!/usr/bin/env cljrsh\n(println \"got\" (count *command-line-args*))\n",
    )
    .unwrap();
    let r = run(&[path.to_str().unwrap(), "x", "y"], None, &[]);
    assert_eq!(r.stdout, "got 2\n");
    assert_eq!(r.code, 0);
}

#[test]
fn shebang_preserves_line_numbers() {
    // An error on line 2 of a shebang script must report line 2.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("err.clj");
    std::fs::write(&path, "#!/usr/bin/env cljrsh\n(this-var-is-unbound)\n").unwrap();
    let r = run(&[path.to_str().unwrap()], None, &[]);
    assert_eq!(r.code, 1);
    assert!(
        r.stderr.contains("this-var-is-unbound"),
        "stderr: {}",
        r.stderr
    );
}

#[test]
fn missing_file_is_a_clean_error() {
    let r = run(&["no-such-file.clj"], None, &[]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("file not found"));
}

#[test]
fn stdin_program_runs_when_not_a_tty() {
    let r = run(&[], Some("(println (* 6 7))"), &[]);
    assert_eq!(r.stdout, "42\n");
}

#[test]
fn preloads_run_before_program() {
    let r = run(
        &["-e", "(pre-defined)"],
        None,
        &[("CLJRSH_PRELOADS", "(defn pre-defined [] :from-preloads)")],
    );
    assert_eq!(r.stdout, ":from-preloads\n");
}

#[test]
fn babashka_preloads_honored_but_cljrsh_wins() {
    let r = run(
        &["-e", "(which)"],
        None,
        &[
            ("BABASHKA_PRELOADS", "(defn which [] :bb)"),
            ("CLJRSH_PRELOADS", "(defn which [] :cljrsh)"),
        ],
    );
    assert_eq!(r.stdout, ":cljrsh\n");
}

#[test]
fn bb_reader_conditional_branch_selected() {
    let r = run(&["-e", "#?(:bb :on-bb :clj :on-jvm)"], None, &[]);
    assert_eq!(r.stdout, ":on-bb\n");
}

#[test]
fn version_and_help() {
    let r = run(&["--version"], None, &[]);
    assert!(r.stdout.starts_with("cljrsh "));
    let r = run(&["--help"], None, &[]);
    assert!(r.stdout.contains("Usage: cljrsh"));
}

#[test]
fn subprocess_namespace_available() {
    let r = run(&["-e", "(:exit (cljrs.process/sh \"true\"))"], None, &[]);
    assert_eq!(r.stdout, "0\n");
}

#[test]
fn classpath_adds_source_dir() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("my");
    std::fs::create_dir_all(&lib_dir).unwrap();
    // .clj extension exercises the babashka-family probe order.
    std::fs::write(lib_dir.join("lib.clj"), "(ns my.lib) (def answer 42)").unwrap();
    let r = run(
        &[
            "-cp",
            dir.path().to_str().unwrap(),
            "-e",
            "(require (quote my.lib)) my.lib/answer",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "42\n", "stderr: {}", r.stderr);
}

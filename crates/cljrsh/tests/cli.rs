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

// ── Host-namespace / compat-layer coverage (cljrsh-host) ─────────────────────

#[test]
fn babashka_fs_compat() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let expr = format!(
        "(require '[babashka.fs :as fs])
         (fs/create-dirs (fs/path \"{d}\" \"sub\"))
         (spit (str \"{d}\" \"/sub/x.clj\") \"1\")
         [(fs/exists? (str \"{d}\" \"/sub/x.clj\"))
          (count (fs/glob \"{d}\" \"**/*.clj\"))
          (fs/file-name (str \"{d}\" \"/sub/x.clj\"))]"
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(r.stdout, "[true 1 \"x.clj\"]\n", "stderr: {}", r.stderr);
}

#[test]
fn cheshire_compat_roundtrip() {
    let r = run(
        &[
            "-e",
            "(require '[cheshire.core :as json])
             (json/parse-string (json/generate-string {:a [1 2.5 nil] :b \"x\"}) true)",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "{:a [1 2.5 nil], :b \"x\"}\n", "stderr: {}", r.stderr);
}

#[test]
fn clojure_java_shell_compat() {
    let r = run(
        &[
            "-e",
            "(require '[clojure.java.shell :refer [sh]]) (:out (sh \"echo\" \"hi\"))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "\"hi\\n\"\n");
}

#[test]
fn babashka_process_shell_throws_on_failure() {
    let r = run(
        &[
            "-e",
            "(require '[babashka.process :as p]) (p/shell \"false\")",
        ],
        None,
        &[],
    );
    assert_eq!(r.code, 1, "stderr: {}", r.stderr);
}

// ── Streaming I/O flags (-i/-I/-o/-O/--stream) ───────────────────────────────

#[test]
fn input_lines_lazy_seq() {
    let r = run(
        &["-i", "-e", "(map clojure.string/upper-case *input*)", "-o"],
        Some("apple\nbanana\n"),
        &[],
    );
    assert_eq!(r.stdout, "APPLE\nBANANA\n", "stderr: {}", r.stderr);
}

#[test]
fn input_edn_values() {
    let r = run(
        &["-I", "-e", "(reduce + (map :n *input*))"],
        Some("{:n 1}\n{:n 2}\n{:n 5}\n"),
        &[],
    );
    assert_eq!(r.stdout, "8\n");
}

#[test]
fn output_prn_is_readable() {
    let r = run(&["-e", "[\"a\" \"b\"]", "-O"], None, &[]);
    assert_eq!(r.stdout, "\"a\"\n\"b\"\n");
}

#[test]
fn stream_edn_per_value() {
    let r = run(
        &["-I", "--stream", "-e", "(* *input* 10)"],
        Some("1\n2\n3\n"),
        &[],
    );
    assert_eq!(r.stdout, "10\n20\n30\n");
}

#[test]
fn stream_lines_with_output_mode() {
    let r = run(
        &["-i", "--stream", "-e", "(count *input*)", "-o"],
        Some("a\nbb\nccc\n"),
        &[],
    );
    assert_eq!(r.stdout, "1\n2\n3\n");
}

#[test]
fn combined_io_flag() {
    let r = run(
        &["-io", "-e", "(map clojure.string/reverse *input*)"],
        Some("ab\ncd\n"),
        &[],
    );
    assert_eq!(r.stdout, "ba\ndc\n");
}

#[test]
fn http_client_compat() {
    use std::io::{Read as _, Write as _};
    // One-shot HTTP server on an ephemeral port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        sock.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )
        .unwrap();
    });
    let expr = format!(
        "(require '[babashka.http-client :as http])
         (let [r (http/get \"http://127.0.0.1:{port}/\")]
           [(:status r) (:body r)])"
    );
    let r = run(&["-e", &expr], None, &[]);
    server.join().unwrap();
    assert_eq!(r.stdout, "[200 \"hello\"]\n", "stderr: {}", r.stderr);
}

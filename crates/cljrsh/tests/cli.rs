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

#[test]
fn yaml_compat_roundtrip() {
    let r = run(
        &[
            "-e",
            "(require '[clj-yaml.core :as yaml])
             (yaml/parse-string (yaml/generate-string {:name \"x\" :tags [\"a\"]}))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "{:name \"x\", :tags [\"a\"]}\n", "stderr: {}", r.stderr);
}

#[test]
fn csv_compat_roundtrip() {
    let r = run(
        &[
            "-e",
            "(require '[clojure.data.csv :as csv])
             (csv/read-csv (csv/write-csv-string [[\"a\" \"b\"] [\"1\" \"2,x\"]]))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[[\"a\" \"b\"] [\"1\" \"2,x\"]]\n", "stderr: {}", r.stderr);
}

// ── Embedded nushell (cljrsh-nu, feature "nu") ───────────────────────────────

#[test]
fn nu_eval_basic_and_tables() {
    let r = run(&["-e", "(nu/eval \"2 + 2\")"], None, &[]);
    assert_eq!(r.stdout, "4\n", "stderr: {}", r.stderr);
    let r = run(
        &[
            "-e",
            "(nu/eval \"[[name size]; [a 10] [b 2000]] | where size > 100 | get name | first\")",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "\"b\"\n");
}

#[test]
fn nu_eval_pipeline_input_from_clojure() {
    let r = run(
        &[
            "-e",
            "(nu/eval \"$in | where price > 10 | get name\"
                      {:in [{:name \"a\" :price 5} {:name \"b\" :price 20}]})",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[\"b\"]\n", "stderr: {}", r.stderr);
}

#[test]
fn nu_session_persists_defs() {
    let r = run(
        &[
            "-e",
            "(let [s (nu/session)]
               (nu/eval \"def greet [n] { $\\\"hi ($n)\\\" }\" {:session s})
               (nu/eval \"greet world\" {:session s}))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "\"hi world\"\n", "stderr: {}", r.stderr);
}

#[test]
fn nu_parse_error_is_catchable() {
    let r = run(
        &[
            "-e",
            "(try (nu/eval \"definitely | | broken\") (catch Exception e :caught))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, ":caught\n");
}

#[test]
fn nu_ls_returns_keyword_maps() {
    let r = run(
        &["-e", "(pos? (:size (first (nu/eval \"ls Cargo.toml\"))))"],
        None,
        &[],
    );
    assert_eq!(r.stdout, "true\n", "stderr: {}", r.stderr);
}

// ── bb.edn tasks (cljrsh-project + tasks.rs) ─────────────────────────────────

fn run_in(dir: &std::path::Path, args: &[&str]) -> Outcome {
    let mut cmd = cljrsh();
    cmd.current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.spawn().unwrap().wait_with_output().unwrap();
    Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

fn task_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("bb.edn"),
        r#"{:paths ["src"]
            :tasks {:init (def base 40)
                    clean (println "cleaning")
                    compile-it {:doc "Compile" :task (do (println "compiling") (+ base 2))}
                    hidden {:task 1 :private true}
                    build {:doc "Build" :depends [clean compile-it]
                           :task (println "result" compile-it)}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/my")).unwrap();
    std::fs::write(
        dir.path().join("src/my/lib.clj"),
        "(ns my.lib) (def marker :from-project-paths)",
    )
    .unwrap();
    dir
}

#[test]
fn tasks_listing_hides_private_and_shows_docs() {
    let dir = task_project();
    let r = run_in(dir.path(), &["tasks"]);
    assert!(r.stdout.contains("compile-it Compile"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("build"));
    assert!(!r.stdout.contains("hidden"));
}

#[test]
fn task_depends_order_and_result_binding() {
    let dir = task_project();
    let r = run_in(dir.path(), &["build"]);
    assert_eq!(r.stdout, "cleaning\ncompiling\nresult 42\n", "stderr: {}", r.stderr);
}

#[test]
fn run_subcommand_invokes_task() {
    let dir = task_project();
    let r = run_in(dir.path(), &["run", "compile-it"]);
    assert_eq!(r.stdout, "compiling\n");
}

#[test]
fn project_paths_apply_to_eval() {
    let dir = task_project();
    let r = run_in(dir.path(), &["-e", "(require 'my.lib) my.lib/marker"]);
    assert_eq!(r.stdout, ":from-project-paths\n", "stderr: {}", r.stderr);
}

#[test]
fn unknown_task_reports_cleanly() {
    let dir = task_project();
    let r = run_in(dir.path(), &["no-such-thing"]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("neither a file nor a task"));
}

// ── Futures / pmap / promise (milestone A7) ──────────────────────────────────

#[test]
fn future_deref_top_level() {
    let r = run(&["-e", "@(future (+ 40 2))"], None, &[]);
    assert_eq!(r.stdout, "42\n", "stderr: {}", r.stderr);
}

#[test]
fn mapv_deref_futures_no_deadlock() {
    let r = run(
        &["-e", "(mapv deref (mapv (fn [i] (future (* i 10))) [1 2 3]))"],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[10 20 30]\n", "stderr: {}", r.stderr);
}

#[test]
fn future_error_propagates_with_ex_data() {
    let r = run(
        &[
            "-e",
            "(try @(future (throw (ex-info \"boom\" {:k 1})))
                  (catch Exception e [(ex-message e) (:k (ex-data e))]))",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[\"boom\" 1]\n", "stderr: {}", r.stderr);
}

#[test]
fn future_predicates_and_single_run() {
    let r = run(
        &[
            "-e",
            "(let [f (future (println \"ran\") :done)]
               [(future? f) @f @f (future-done? f)])",
        ],
        None,
        &[],
    );
    // Body printed exactly once even with two derefs.
    assert_eq!(r.stdout, "ran\n[true :done :done true]\n", "stderr: {}", r.stderr);
}

#[test]
fn pmap_and_promise() {
    let r = run(
        &[
            "-e",
            "[(vec (pmap inc [1 2 3])) (let [p (promise)] (deliver p :d) @p)]",
        ],
        None,
        &[],
    );
    assert_eq!(r.stdout, "[[2 3 4] :d]\n", "stderr: {}", r.stderr);
}

// ── Pods (cljrsh-pods, bundled test pod) ─────────────────────────────────────

#[test]
fn pods_end_to_end_through_binary() {
    // The test pod binary lives in the same target dir as cljrsh.
    let pod = std::path::Path::new(env!("CARGO_BIN_EXE_cljrsh"))
        .with_file_name("cljrsh-test-pod");
    if !pod.exists() {
        eprintln!("skipping: cljrsh-test-pod not built");
        return;
    }
    let expr = format!(
        "(require '[babashka.pods :as pods])
         (pods/load-pod \"{}\")
         [(pod.test-pod/add-sync 40 2)
          (pod.test-pod/from-code)
          (try (pod.test-pod/error-fn)
               (catch Exception e (:pod-var (ex-data e))))]",
        pod.display()
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(
        r.stdout, "[42 :evaluated-client-side :error-fn]\n",
        "stderr: {}",
        r.stderr
    );
}

// ── nREPL server (milestone B-M5) ────────────────────────────────────────────

#[test]
fn nrepl_server_clone_and_eval() {
    use cljrs_bencode::{Bencode, decode, encode_to_vec};
    use std::io::{Read as _, Write as _};

    // Port 0 → OS-assigned; read the actual port from stdout.
    let dir = tempfile::tempdir().unwrap();
    let mut child = cljrsh()
        .args(["nrepl-server", "0"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    {
        use std::io::BufRead as _;
        let mut reader = std::io::BufReader::new(child.stdout.as_mut().unwrap());
        reader.read_line(&mut line).unwrap();
    }
    let port: u16 = line
        .split("port ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .expect("port in banner");

    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut buf: Vec<u8> = Vec::new();
    let mut read_msg = |sock: &mut std::net::TcpStream, buf: &mut Vec<u8>| -> Bencode {
        loop {
            if let Ok(Some((msg, used))) = decode(buf) {
                buf.drain(..used);
                return msg;
            }
            let mut chunk = [0u8; 4096];
            let n = sock.read(&mut chunk).expect("nrepl read");
            assert!(n > 0, "nrepl closed");
            buf.extend_from_slice(&chunk[..n]);
        }
    };
    let dict = |entries: Vec<(&str, &str)>| {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in entries {
            m.insert(k.as_bytes().to_vec(), Bencode::str(v));
        }
        Bencode::Dict(m)
    };
    let get = |m: &Bencode, k: &str| -> Option<String> {
        m.as_dict()?
            .get(k.as_bytes())
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    sock.write_all(&encode_to_vec(&dict(vec![("op", "clone"), ("id", "1")])))
        .unwrap();
    let session = loop {
        let m = read_msg(&mut sock, &mut buf);
        if let Some(s) = get(&m, "new-session") {
            break s;
        }
    };

    sock.write_all(&encode_to_vec(&dict(vec![
        ("op", "eval"),
        ("code", "(reduce + (range 101))"),
        ("id", "2"),
        ("session", &session),
    ])))
    .unwrap();
    let mut value = None;
    loop {
        let m = read_msg(&mut sock, &mut buf);
        if let Some(v) = get(&m, "value") {
            value = Some(v);
        }
        if let Some(Bencode::List(status)) = m.as_dict().and_then(|d| d.get(b"status".as_ref()))
            && status.iter().any(|s| s.as_str() == Some("done"))
        {
            break;
        }
    }
    assert_eq!(value.as_deref(), Some("5050"));

    let _ = child.kill();
    let _ = child.wait();
}

// ── Error reports (milestone A8) ─────────────────────────────────────────────

#[test]
fn error_report_has_type_data_location_and_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boom.clj");
    std::fs::write(
        &path,
        "(defn inner [x]\n  (throw (ex-info \"kaboom\" {:k x})))\n(defn outer []\n  (inner 7))\n(outer)\n",
    )
    .unwrap();
    let r = run(&[path.to_str().unwrap()], None, &[]);
    assert_eq!(r.code, 1);
    let err = &r.stderr;
    assert!(err.contains("Type:     clojure.lang.ExceptionInfo"), "{err}");
    assert!(err.contains("Message:  kaboom"), "{err}");
    assert!(err.contains("Data:     {:k 7}"), "{err}");
    assert!(err.contains("boom.clj:4:4"), "location: {err}");
    assert!(err.contains("^--- kaboom"), "caret: {err}");
    // Innermost first.
    let inner_pos = err.find("user/inner").expect("inner frame");
    let outer_pos = err.find("user/outer").expect("outer frame");
    assert!(inner_pos < outer_pos, "{err}");
}

#[test]
fn reader_error_report_has_location_and_caret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.clj");
    std::fs::write(&path, "(println :ok)\n(def x {:a 1)\n").unwrap();
    let r = run(&[path.to_str().unwrap()], None, &[]);
    assert_eq!(r.code, 1);
    let err = &r.stderr;
    assert!(err.contains("Type:     Reader error"), "{err}");
    assert!(err.contains("Message:  unexpected closing delimiter"), "{err}");
    assert!(err.contains("bad.clj:2:13"), "location: {err}");
    assert!(err.contains("^--- unexpected closing delimiter"), "caret: {err}");

    // Unclosed-at-EOF points at the opening delimiter.
    let path2 = dir.path().join("unclosed.clj");
    std::fs::write(&path2, "(defn f [x]\n  (+ x 1)\n").unwrap();
    let r2 = run(&[path2.to_str().unwrap()], None, &[]);
    assert_eq!(r2.code, 1);
    assert!(r2.stderr.contains("unclosed list"), "{}", r2.stderr);
    assert!(r2.stderr.contains("unclosed.clj:1:1"), "{}", r2.stderr);
}

#[test]
fn caught_error_does_not_pollute_later_trace() {
    let r = run(
        &[
            "-e",
            "(defn safe [] (try (throw (ex-info \"handled\" {})) (catch Exception e :ok)))
             (safe)
             (defn fails [] (nth [1 2] 9))
             (fails)",
        ],
        None,
        &[],
    );
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("user/fails"), "{}", r.stderr);
    assert!(!r.stderr.contains("user/safe"), "{}", r.stderr);
    // The context snippet may show source containing "handled"; the reported
    // error itself must not be the caught one.
    assert!(!r.stderr.contains("Message:  handled"), "{}", r.stderr);
}

#[test]
fn unbound_symbol_report() {
    let r = run(&["-e", "(defn f [] (no-such-fn 1)) (f)"], None, &[]);
    assert!(
        r.stderr.contains("Unable to resolve symbol: no-such-fn"),
        "{}",
        r.stderr
    );
    assert!(r.stderr.contains("user/f"), "{}", r.stderr);
}

// ── deps (:deps in bb.edn) + babashka.cli + -x (milestone B-M3) ──────────────

#[test]
fn local_root_dep_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("mylib/src/coollib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("core.clj"), "(ns coollib.core) (def marker :dep-loaded)").unwrap();
    std::fs::write(
        dir.path().join("bb.edn"),
        r#"{:deps {coollib/coollib {:local/root "mylib"}}}"#,
    )
    .unwrap();
    let r = run_in(
        dir.path(),
        &["-e", "(require 'coollib.core) coollib.core/marker"],
    );
    assert_eq!(r.stdout, ":dep-loaded\n", "stderr: {}", r.stderr);
}

#[test]
fn babashka_cli_is_builtin() {
    let r = run(
        &[
            "-e",
            "(require '[babashka.cli :as cli])
             (cli/parse-opts [\"--port\" \"8080\" \"--who\" \":admin\" \"-v\"]
                             {:alias {:v :verbose}})",
        ],
        None,
        &[],
    );
    assert_eq!(
        r.stdout, "{:port 8080, :who :admin, :verbose true}\n",
        "stderr: {}",
        r.stderr
    );
}

#[test]
fn exec_flag_calls_fn_with_parsed_opts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/my")).unwrap();
    std::fs::write(dir.path().join("bb.edn"), r#"{:paths ["src"]}"#).unwrap();
    std::fs::write(
        dir.path().join("src/my/tool.clj"),
        "(ns my.tool)\n(defn hello [{:keys [name times] :or {times 1}}]\n  (dotimes [_ times] (println \"hi\" name)))",
    )
    .unwrap();
    let r = run_in(
        dir.path(),
        &["-x", "my.tool/hello", "--name", "x", "--times", "2"],
    );
    assert_eq!(r.stdout, "hi x\nhi x\n", "stderr: {}", r.stderr);
}

// ── Built-in AWS client (cljrsh-aws, feature "aws") ──────────────────────────

#[test]
fn aws_s3_signing_and_list_against_mock() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || -> String {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 16384];
        let n = sock.read(&mut buf).unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let xml = r#"<?xml version="1.0"?>
<ListBucketResult><Name>b</Name><KeyCount>2</KeyCount><IsTruncated>false</IsTruncated>
<Contents><Key>a/one.txt</Key><Size>3</Size><ETag>"e1"</ETag><LastModified>2026-01-01T00:00:00.000Z</LastModified></Contents>
<Contents><Key>a/two.txt</Key><Size>7</Size><ETag>"e2"</ETag><LastModified>2026-01-02T00:00:00.000Z</LastModified></Contents>
</ListBucketResult>"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            xml.len(),
            xml
        );
        sock.write_all(resp.as_bytes()).unwrap();
        req
    });
    let expr = format!(
        "(def c (aws/client {{:api :s3 :region \"us-east-1\"
                             :endpoint \"http://127.0.0.1:{port}\"
                             :access-key-id \"AKIATEST\" :secret-access-key \"secret\"}}))
         (let [r (aws/invoke c {{:op :ListObjectsV2 :request {{:Bucket \"b\" :Prefix \"a/\"}}}})]
           [(:KeyCount r) (mapv :Key (:Contents r)) (:Size (first (:Contents r)))
            (instant? (:LastModified (first (:Contents r))))])"
    );
    let r = run(&["-e", &expr], None, &[]);
    let req = server.join().unwrap();
    assert_eq!(
        r.stdout, "[2 [\"a/one.txt\" \"a/two.txt\"] 3 true]\n",
        "stderr: {}",
        r.stderr
    );
    // Path-style URL against custom endpoint + SigV4 headers.
    assert!(req.starts_with("GET /b/?list-type=2&prefix=a%2F"), "req: {req}");
    assert!(req.contains("authorization: AWS4-HMAC-SHA256"), "req: {req}");
    assert!(req.contains("Credential=AKIATEST/"), "req: {req}");
    assert!(req.contains("x-amz-date:"), "req: {req}");
    assert!(req.contains("x-amz-content-sha256:"), "req: {req}");
}

#[test]
fn aws_presign_and_anomaly() {
    let expr = "(def c (aws/client {:api :s3 :region \"us-east-1\"
                                    :access-key-id \"AKIATEST\" :secret-access-key \"secret\"}))
         (let [url (aws/presign c {:op :GetObject :request {:Bucket \"b\" :Key \"k/x.txt\"} :expires 600})]
           [(clojure.string/includes? url \"X-Amz-Signature=\")
            (clojure.string/includes? url \"X-Amz-Expires=600\")
            (clojure.string/starts-with? url \"https://b.s3.us-east-1.amazonaws.com/k/x.txt\")])";
    let r = run(&["-e", expr], None, &[]);
    assert_eq!(r.stdout, "[true true true]\n", "stderr: {}", r.stderr);
}

// ── Built-in Kubernetes client (cljrsh-k8s, feature "k8s") ───────────────────

/// Full e2e against a real cluster; set CLJRSH_K8S_TEST_CONTEXT (e.g. a k3d
/// context) to enable. Skipped silently otherwise so CI without a cluster
/// stays green.
#[test]
fn k8s_end_to_end_when_cluster_available() {
    let Ok(context) = std::env::var("CLJRSH_K8S_TEST_CONTEXT") else {
        eprintln!("skipping: CLJRSH_K8S_TEST_CONTEXT not set");
        return;
    };
    let expr = format!(
        "(def c (k8s/client {{:context \"{context}\"}}))
         (k8s/apply c {{:apiVersion \"v1\" :kind \"ConfigMap\"
                        :metadata {{:name \"cljrsh-test-cm\" :namespace \"default\"}}
                        :data {{:k \"v\"}}}})
         (let [got (get-in (k8s/get c :ConfigMap \"cljrsh-test-cm\" {{:namespace \"default\"}})
                           [:data :k])
               n (count (k8s/list c :namespaces))]
           (k8s/delete c :ConfigMap \"cljrsh-test-cm\" {{:namespace \"default\"}})
           [got (pos? n)])"
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(r.stdout, "[\"v\" true]\n", "stderr: {}", r.stderr);
}

// ── Infrastructure DSLs: tf + kustomize (cljrsh-host) ────────────────────────

#[test]
fn tf_stack_emission_and_conflicts() {
    let r = run(
        &[
            "-e",
            "(require '[tf])
             (let [s (tf/stack (tf/provider :aws {:region \"us-east-1\"})
                               (tf/resource :aws_s3_bucket :content {:bucket \"b\"})
                               (tf/output :arn {:value (tf/ref :aws_s3_bucket.content.arn)}))]
               [(get-in s [:resource :aws_s3_bucket :content :bucket])
                (get-in s [:output :arn :value])
                (tf/var-ref :region)
                (try (tf/stack (tf/resource :a :x {:v 1}) (tf/resource :a :x {:v 2}))
                     (catch Exception e :duplicate-detected))])",
        ],
        None,
        &[],
    );
    assert_eq!(
        r.stdout,
        "[\"b\" \"${aws_s3_bucket.content.arn}\" \"${var.region}\" :duplicate-detected]\n",
        "stderr: {}",
        r.stderr
    );
}

#[test]
fn tf_engine_loop_when_tofu_available() {
    if std::process::Command::new("tofu")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: tofu not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let expr = format!(
        "(require '[tf])
         (tf/write! \"{d}\" (tf/stack
           (tf/terraform {{:required_providers {{:local {{:source \"hashicorp/local\"}}}}}})
           (tf/resource :local_file :f {{:filename \"${{path.module}}/out.txt\"
                                        :content \"from-cljrsh\"}})))
         (tf/init! \"{d}\") (tf/validate! \"{d}\") (tf/apply! \"{d}\")
         (slurp \"{d}/out.txt\")"
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(r.stdout, "\"from-cljrsh\"\n", "stderr: {}", r.stderr);
}

#[test]
fn kustomize_overlay_and_tree() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let expr = format!(
        "(require '[kustomize :as k])
         (let [base (k/manifest \"apps/v1\" :Deployment \"web\" {{}} {{:replicas 1 :extra {{:keep 1 :drop 2}}}})
               prod (k/overlay base {{:spec {{:replicas 3 :extra {{:drop nil}}}}}})]
           (k/write! \"{d}\" {{:kustomization {{:namespace \"x\"}}
                              :resources {{\"deploy.yaml\" prod}}}})
           [(get-in prod [:spec :replicas])
            (get-in prod [:spec :extra])
            (cljrsh.fs/exists? \"{d}/kustomization.yaml\")
            (some? (clojure.string/index-of (slurp \"{d}/kustomization.yaml\") \"deploy.yaml\"))])"
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(
        r.stdout, "[3 {:keep 1} true true]\n",
        "stderr: {}",
        r.stderr
    );
}

#[test]
fn kustomize_build_when_kubectl_available() {
    if std::process::Command::new("kubectl")
        .arg("version")
        .arg("--client")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: kubectl not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let expr = format!(
        "(require '[kustomize :as k])
         (k/write! \"{d}\" {{:kustomization {{:namespace \"ns1\"}}
                            :resources {{\"cm.yaml\" (k/manifest \"v1\" :ConfigMap \"c\" {{}})}}}})
         (let [rendered (k/build \"{d}\")]
           [(mapv :kind rendered) (get-in (first rendered) [:metadata :namespace])])"
    );
    let r = run(&["-e", &expr], None, &[]);
    assert_eq!(r.stdout, "[[\"ConfigMap\"] \"ns1\"]\n", "stderr: {}", r.stderr);
}

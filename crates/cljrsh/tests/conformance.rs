//! Data-driven babashka-conformance corpus.
//!
//! Each case in `tests/bb-conformance/` is `<name>.clj` plus sidecars:
//!
//! - `<name>.out`  — expected stdout, matched exactly (required)
//! - `<name>.args` — full argv template, whitespace-split, `{file}` expands
//!                   to the script path (default: `{file}`)
//! - `<name>.in`   — stdin
//! - `<name>.exit` — expected exit code (default 0)
//! - `<name>.err`  — substring that must appear in stderr
//!
//! One #[test] per behavior area would hide coverage; instead this walks the
//! whole corpus and reports every failing case at once.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bb-conformance")
}

struct Failure {
    name: String,
    detail: String,
}

fn run_case(script: &Path) -> Result<(), String> {
    let sidecar = |ext: &str| -> Option<String> {
        std::fs::read_to_string(script.with_extension(ext)).ok()
    };
    let expected_out = sidecar("out").ok_or("missing .out sidecar")?;
    let args_template = sidecar("args").unwrap_or_else(|| "{file}".to_string());
    let stdin = sidecar("in");
    let expected_exit: i32 = sidecar("exit")
        .map(|s| s.trim().parse().expect("bad .exit"))
        .unwrap_or(0);
    let expected_err = sidecar("err");

    let argv: Vec<String> = args_template
        .split_whitespace()
        .map(|a| a.replace("{file}", script.to_str().unwrap()))
        .collect();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cljrsh"));
    cmd.args(&argv)
        .current_dir(script.parent().unwrap())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    if let Some(input) = &stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .map_err(|e| format!("stdin: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().unwrap_or(-1);

    let mut problems = Vec::new();
    if stdout != expected_out {
        problems.push(format!(
            "stdout mismatch\n  expected: {expected_out:?}\n  actual:   {stdout:?}"
        ));
    }
    if code != expected_exit {
        problems.push(format!("exit {code}, expected {expected_exit}"));
    }
    if let Some(needle) = &expected_err
        && !stderr.contains(needle.trim())
    {
        problems.push(format!(
            "stderr missing {:?}\n  actual: {stderr:?}",
            needle.trim()
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[test]
fn bb_conformance_corpus() {
    let dir = corpus_dir();
    let mut scripts: Vec<_> = std::fs::read_dir(&dir)
        .expect("bb-conformance dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "clj"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "empty corpus at {}", dir.display());

    let mut failures: Vec<Failure> = Vec::new();
    for script in &scripts {
        if let Err(detail) = run_case(script) {
            failures.push(Failure {
                name: script.file_stem().unwrap().to_string_lossy().into_owned(),
                detail,
            });
        }
    }

    if !failures.is_empty() {
        let mut report = format!(
            "{}/{} conformance cases failed:\n",
            failures.len(),
            scripts.len()
        );
        for f in &failures {
            report.push_str(&format!("\n── {} ──\n{}\n", f.name, f.detail));
        }
        panic!("{report}");
    }
    println!("all {} conformance cases passed", scripts.len());
}

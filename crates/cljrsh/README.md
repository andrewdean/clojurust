# cljrsh

**Purpose:** cljrsh — Clojure Rust Shell: a babashka-style scripting binary built on clojurust. File-first CLI, shebang scripts, preloads, babashka exit-code conventions, rustyline REPL. Zero JVM.

**Status:** Milestone M0 (skeleton): `-e` / `-f` / bare-file / stdin programs, `*command-line-args*`, `-cp`, shebang stripping (line numbers preserved), `CLJRSH_PRELOADS`/`BABASHKA_PRELOADS`, `System/exit` + `{:cljrsh/exit N}`/`{:babashka/exit N}` exit conventions, SIGPIPE → 141, reader features `#{:bb :cljrsh :rust}`, REPL with multi-line input and history (`~/.cache/cljrsh/history`). Startup ~20 ms debug. Upcoming milestones add host namespaces (fs/http/json/...), bb.edn tasks, pods, and nREPL — see the cljrsh plan.

## File layout

- `src/main.rs` — entry: SIGPIPE reset, arg dispatch, big-stack interpreter thread, Tokio current-thread runtime + LocalSet (`with_driver`), GC mutator registration.
- `src/opts.rs` — hand-rolled file-first argument grammar (`Program`, `Opts`, `parse`, `usage`).
- `src/exec.rs` — `setup_globals` (stdlib + async/io/charset + cljrs-process + reader features + command-line args), `run_program` (shebang, preloads, exit-code mapping), `eval_str`/`eval_form` (per-form LocalSet drive).
- `src/repl.rs` — rustyline REPL, delimiter-balance multi-line accumulation, history.
- `tests/cli.rs` — end-to-end binary tests of every M0 behavior.

## Public API

A binary crate; no library surface. Behaviors:

- `cljrsh [opts] (-e EXPR | -f FILE | FILE) [args...]` — remaining args → `*command-line-args*`; `--` ends option parsing.
- Exit codes: `System/exit N` and thrown ex-info with `:cljrsh/exit`/`:babashka/exit` in ex-data (checked one ex-cause deep) exit N — the ex-info form prints only the message; other errors report and exit 1; usage errors exit 2; SIGPIPE 141.
- No program: REPL on a tty, otherwise the script is read from stdin.
- Reader conditional features: `:bb`, `:cljrsh`, `:rust` (+ `:default`), top-level conditionals expanded before evaluation.

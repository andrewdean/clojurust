# cljrs-process

**Purpose:** Subprocess support for clojurust scripting — the `cljrs.process` namespace: blocking `sh` and spawned child-process handles. The host substrate for cljrsh's `cljrsh.process` / `babashka.process` / `clojure.java.shell` surface.

**Status:** Implemented (v1: blocking sh, spawn/wait/alive?/exit-code/destroy). Part of the cljrsh scripting-binary work. Trusted full-power surface — never registered in the restricted transaction environment (cljrs-tx boots only cljrs-builtins natives). `destroy` kills the child process only, not its descendants (process-group kill is future work).

## File layout

- `src/lib.rs` — the whole crate: `ChildProcess` NativeObject, option parsing, spawning, and namespace registration.
- `tests/process.rs` — end-to-end interpreter tests (sh capture/stdin/env/dir, spawn lifecycle, destroy).

## Public API

- `NS: &str = "cljrs.process"`.
- `fn init(globals: &Arc<GlobalEnv>)` — idempotent: create + register the namespace (the pattern from `cljrs-base64`).
- `fn register(registry: &mut Registry)` — raw registration for embedders composing their own registry.
- `struct ChildProcess` — `NativeObject` (type tag `"ChildProcess"`) holding the `std::process::Child` and a cached wait result.

Clojure surface (all in `cljrs.process`):

- `(sh "cmd" "arg" ... & kwopts)` — run to completion, `{:exit N :out String :err String}`. Options after the string args (clojure.java.shell style): `:in` (string piped to stdin), `:dir`, `:env` (replace), `:extra-env` (merge).
- `(spawn ["cmd" "arg" ...] opts?)` — start a child, return a `ChildProcess`. Opts map: `:dir`, `:env`, `:extra-env`, `:in` (string or `:inherit`; default null), `:out`/`:err` (`:inherit` or piped default).
- `(wait proc)` — block until exit, `{:exit N :out ... :err ...}`; idempotent (result cached). Exit is `-1` when the child was killed by a signal.
- `(alive? proc)` / `(exit-code proc)` (nil while running) / `(destroy proc)`.

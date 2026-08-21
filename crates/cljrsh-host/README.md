# cljrsh-host

**Purpose:** cljrsh's host library surface: native `cljrsh.*` namespaces implemented in Rust, plus the babashka-compatibility layer shipped as embedded portable-Clojure veneers that load lazily on `require`.

**Status:** Milestone B-M1, first slice — `cljrsh.fs`, `cljrsh.json`, and compat veneers `babashka.fs`, `babashka.process` (sh/shell/process/check subset), `cheshire.core`, `clojure.java.shell`. Later slices add http, yaml, csv, cli, io streaming, term, signal, template. Documented divergences: paths are strings (not `java.nio.file.Path`), `babashka.process` handles are ChildProcess native objects with `(wait p)` instead of deref, `tokenize` is whitespace-naive, `destroy-tree` kills only the child.

## File layout

- `src/lib.rs` — `init(globals)`: registers native namespaces via `Registry` and the compat veneers via `register_builtin_source` (the `cljrs-stdlib` embedding pattern).
- `src/fs.rs` — `cljrsh.fs`: exists?/directory?/regular-file?/sym-link?/readable?/size/modified-time-millis/list-dir/create-dirs/delete/delete-tree/copy/copy-tree/move/absolutize/canonicalize/file-name/parent/extension/which/temp-dir/create-temp-dir/cwd/home/glob/walk (walkdir + globset; paths are strings).
- `src/json.rs` — `cljrsh.json`: parse-string (truthy second arg → keyword keys), generate-string (`{:pretty true}`); public `json_to_value`/`value_to_json` converters (serde_json).
- `src/clj/babashka/fs.cljrs`, `src/clj/babashka/process.cljrs`, `src/clj/cheshire/core.cljrs`, `src/clj/clojure/java/shell.cljrs` — the compat veneers.

## Public API

- `fn init(globals: &Arc<GlobalEnv>)` — idempotent full registration; called by the cljrsh binary at startup.
- `mod fs::register(&mut Registry)`, `mod json::register(&mut Registry)` — per-namespace registration for embedders.
- `json::json_to_value(&serde_json::Value, keywordize: bool) -> Value`, `json::value_to_json(&Value) -> Result<serde_json::Value, String>` — reusable JSON ↔ Clojure conversion (maps/vectors/keywords/uuid; non-finite doubles and exotic types error).

# cljrsh-nu

**Purpose:** Embedded nushell engine for cljrsh — the `nu` Clojure namespace. `(nu/eval "ls | where size > 1kb")` parses and evaluates a nu pipeline in-process (no external `nu` binary, full standard command set) and returns Clojure data.

**Status:** Implemented (v1: collect-to-value; no streaming eval, no Clojure-callable-from-nu, no plugin protocol — recorded extension points). nu crates pinned `=0.115.0` (`nu-protocol`, `nu-parser`, `nu-engine`, `nu-cmd-lang`, `nu-command` with `default-features = false, features = ["os", "network", "rustls-tls"]` — no sqlite/plugin, no reedline). Gated behind the cljrsh binary's default-on `nu` cargo feature. Licensing: nushell is MIT (compatible with this EPL-1.0 workspace); reproduce its notice in any distributed third-party-notices file.

## File layout

- `src/lib.rs` — `NuSession` NativeObject (Mutex<(EngineState, Stack)>), default-session lifecycle, parse→merge_delta→eval_block→merge_env→into_value flow, and the `nu/eval`, `nu/session`, `nu/parse` registrations.
- `src/convert.rs` — bidirectional value mapping.

## Semantics

- `(nu/eval code)` / `(nu/eval code {:session s :in data :keywordize? bool})` — `:in` becomes the pipeline's `$in`; records → keyword-keyed maps (`:keywordize? false` → string keys); tables → vectors of maps.
- `(nu/session)` / `(nu/session {:cwd "..." :env {...}})` — explicit sessions are sticky (keep creation-time cwd/env) and persist `def`s/aliases/`let`s/env (incl. nu-side `cd`) across evals. The implicit default session persists state too but re-syncs cwd/env **from the process** at each eval; nu-side `cd` never changes the process cwd.
- `(nu/parse code)` — syntax check: nil or a thrown parse error.
- Value mapping: Filesize → bytes (Long), Duration → nanoseconds (Long), Date ↔ `#inst` (`Value::Instant`, epoch millis), Binary ↔ byte array, bounded ranges realize to vectors. Not convertible (throws): closures, custom values, unbounded ranges. Clojure keywords/symbols/uuids stringify going in.
- Externals (`^ls`, bare externals) run in-engine; a trailing external's output lands in the returned value. Stderr inherits the process's stderr.
- Errors (parse and ShellError) surface as catchable exceptions with nu's rendered message.
- Config files (`env.nu`/`config.nu`) are never loaded. Evaluation is synchronous on the calling thread.

## Public API

- `fn init(globals: &Arc<GlobalEnv>)` / `fn register(&mut Registry)` — namespace registration.
- `struct NuSession` — NativeObject (type tag `"NuSession"`).
- `fn default_interrupt_flag() -> Arc<AtomicBool>` — shared `Signals` source; the hosting binary's SIGINT handler flips it to stop a running pipeline.
- `convert::nu_to_clj(&nu::Value, keywordize) -> Result<Value, String>`, `convert::clj_to_nu(&Value) -> Result<nu::Value, String>`.

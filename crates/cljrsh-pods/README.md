# cljrsh-pods

**Purpose:** babashka pod protocol client — `(babashka.pods/load-pod "...")` unlocks the existing pod ecosystem (sqlite, postgres, ...) with zero porting.

**Status:** Milestone B-M4a — path/command `load-pod`, bencode framing (`cljrs-bencode`), all three payload formats (**edn**, **json**, **transit+json** via the built-in minimal transit codec in `src/transit.rs`), pod namespaces registered as native sync-invoke stubs (errors surface as real ex-info with parsed ex-data), `"code"` vars evaluated client-side, `out`/`err` forwarding, `unload-pod`, `shutdown_all()` for clean process exit, and a bundled test pod. Verified against the real `pod-babashka-go-sqlite3`. Later (M4b): the pod registry (`:pods` in bb.edn + download/cache), streaming `:handlers`, pod-declared data readers.

## File layout

- `src/lib.rs` — wire messages, the `Pod` handle (reader thread → mpsc, `GcPtr`s never cross threads; the `cljrs-nrepl` split), spawn/describe/invoke/shutdown, namespace registration, `cljrsh.pods` natives + `babashka.pods` veneer.
- `src/transit.rs` — minimal transit+json codec: `~:`/`~$`/`~i`/`~d`/`~t`/`~u`/`~~` strings, `["^ ", ...]` maps, `~#list`/`~#set`/`~#cmap`/`~#'` tags, and the exact **read cache** (`^0`… codes) pod writers emit; the encoder never emits cache codes. Invoke args are sent as a transit **list** (babashka sends a seq).
- `src/bin/test_pod.rs` — `cljrsh-test-pod`: a reference pod (EDN format) used by `tests/pod.rs`.
- `tests/pod.rs` — end-to-end: sync invoke, code vars, ex-info errors, out forwarding, unload, the babashka.pods veneer.

## Public API

- `fn init(globals: &Arc<GlobalEnv>)` — register `cljrsh.pods/load-pod` + `unload-pod` and the `babashka.pods` veneer. `load-pod` takes a path string or command vector.
- `fn shutdown_all()` — send `shutdown` to every live pod (called by the binary before `process::exit`, which skips Drop).
- `transit::{decode, encode}` — the transit+json codec (`serde_json::Value` ↔ Clojure `Value`).
- `struct Pod` / `struct PodHandle` — NativeObject (type tag `"Pod"`); dropping the handle shuts the pod down.

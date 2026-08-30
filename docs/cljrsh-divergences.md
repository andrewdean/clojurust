# cljrsh: intentional divergences

Where cljrsh deliberately differs from JVM Clojure and/or babashka.
Everything here is a decision, not a gap — gaps live in the issue
tracker. Parity items that USED to diverge and were fixed (decoder
keywordization via `Keyword::parse`, `with-redefs` on registry natives,
exit codes through `try/finally`) are not listed.

## Runtime model

- **Single-threaded per isolate; parallelism is explicit.** One
  interpreter thread with a current-thread Tokio runtime. `future`,
  `pmap`, and `promise` exist and are API-compatible, but execute
  cooperatively on that thread — `pmap` is `map` with a different cost
  model, and `with-redefs`' "visible in all threads" is true per
  isolate. Real parallelism is opt-in and visibly so: `(isolate)` /
  `(isolate-call iso 'ns/fn args…)` / `(pfuture (ns/fn args…))` run on
  separate OS threads with share-nothing heaps; work ships as a fully
  qualified symbol plus deep-copied arguments — never a closure — and
  results cross the same metered copy boundary as `isolate-chan`
  (docs/user-reachable-isolates-plan.md). OS processes
  (`cljrsh.process`) remain an option for process-level isolation.
- **Agents are serial async mailboxes, not thread-pool actors.**
  `send` and `send-off` are the same operation (no pools to choose
  between); actions run cooperatively, in order, on the owning
  isolate's executor. `(await agent)` is the async special form —
  it parks until the mailbox drains (or the agent fails) and returns
  nil; there is no blocking await. Constructor options (`:meta`,
  `:validator`, `:error-handler`, `:error-mode`) are accepted and
  ignored; a failed agent keeps its queue for `restart-agent`, and a
  watch error fails the agent like an action error.
- **`locking` is a no-op — by argument, not omission.** Share-nothing
  isolates mean no two threads can ever reference the same GC object;
  the only cross-isolate mutables are `shared-atom` (lock-free CAS)
  and channels (already synchronized). There is nothing a monitor
  could guard, so the body just runs.
- **`System/exit` is an uncatchable control signal** — `catch` never
  sees it — but **`finally` blocks still run** on the way out. JVM
  `System.exit` skips `finally` (the process dies inside the call);
  cljrsh scripts routinely guard temp-dir cleanup with `try/finally`,
  so unwinding runs it. The exit code is preserved.
- **Reader conditionals** answer to `:cljrsh`, `:bb`, `:clj`, and
  `:rust` (embedders can extend the set). `:bb` is honored so babashka
  scripts take their bb branch.

## I/O

- **`*out*` / `*err*` are IO sentinel keywords** (`:cljrs.io/stdout`,
  `:cljrs.io/stderr`), not `java.io.Writer`s. `(binding [*out* *err*]
  …)` routes prints to stderr for its extent; binding `*out*` to
  anything other than the two sentinels leaves the print target
  unchanged (there is no Writer protocol to target).
- **`with-out-str` captures both streams.** Output redirected to stderr
  inside the captured extent lands in the capture, where JVM Clojure
  would let it escape to the real stderr. Scripting harnesses assert
  over a script's combined output; capturing both is the more useful
  contract.

## Processes (`cljrsh.process`)

- `sh`/`shell`/`process` take a **leading opts map** (babashka.process
  style): `(p/sh {:dir d :in s} "cmd" "arg")`.
- `destroy-tree` kills **the child only** (no process-group walk);
  wrap the child in `exec` from a shell when the child must be the
  direct kill target.

## Built-in clients

- **k8s**: `list` returns a bare vector of objects (not `{:items […]}`);
  `patch` is JSON **merge**-patch only (arrays replace wholesale — carry
  `metadata.resourceVersion` for optimistic concurrency); `apply` is
  server-side apply with force (field manager `cljrsh`, merges
  containers by name); `raw` is GET-only. Strategic-merge patches,
  subresource writes, `auth can-i`, and admission-warning capture stay
  with `kubectl`.
- Decoded API objects keywordize keys with `Keyword::parse`, so
  slash-bearing keys (`app.kubernetes.io/name`) are namespaced keywords
  and match `:a/b` literals — same as cheshire/clj-yaml on the JVM.

## Tasks and tooling

- `run --parallel` warns and runs **sequentially** (single-threaded by
  design).
- `uberscript` is a **textual carve** like babashka's: reachable source
  namespaces are concatenated dependencies-first; built-ins, pods, and
  native deps remain as plain `require`s for the target runtime. An
  inline `(ns …)` marks its namespace loaded (except while the loader
  owns it), so the bundle's own requires are satisfied.
- `socket-repl` serves **one connection at a time**; evaluation prints
  go to the process stdout, only results and errors travel the socket.

## clojure.pprint and stdin

- **`clojure.pprint` is a pragmatic subset**: `pprint` is width-aware
  (fits-on-one-line, else break with indentation) rather than the XP
  pretty-printing algorithm; `cl-format` supports only the common
  directives (`~a ~s ~d ~x ~o ~b ~f ~% ~& ~~`) and throws on the rest;
  `write` always prints or returns via `pprint`/`pr` (no `:stream`
  Writer plumbing). `print-table` matches Clojure's output.
- **`line-seq` takes `*in*` (the stdin sentinel) or a file path string**
  — there is no Reader object to wrap. Path-string input is read
  eagerly.
- **`subseq`/`rsubseq` are linear filters** over the sorted collection's
  seq using `compare` (not a tree descent honoring a custom
  comparator): same results for default-ordered collections, O(n)
  instead of O(log n + k), and custom `sorted-map-by` comparators are
  not consulted for the bounds.

## Regex

- Patterns run on **fancy-regex**: full lookaround (`(?=…)`, `(?<=…)`)
  and backreferences work, matching JVM/JS expectations. The
  backtracking limit surfaces at match time; a pattern that exceeds it
  behaves as **no match** rather than throwing (only pathological
  patterns hit it).

## Interop shims

- `(. target member)` / `..` dispatch through the same method table as
  the `.method` sugar; `.toString` works on every value.
- `StringBuilder.` / `HashMap.` / `ArrayDeque.` are native emulations
  for portable `.cljc` `:clj` branches; `Locale/US`-style constants are
  opaque keywords and locale-parameterized string methods ignore them
  (Rust's Unicode case mapping is unconditional).
- `extend` registers method maps by dispatch tag; JVM/cljs class
  designators normalize (`clojure.lang.IPersistentVector` → `Vector`,
  `Number` fans out to every numeric tag).

## Semantics

- `map?` returns false for `reify` instances (they are native objects,
  not maps, even when they implement lookup).
- `pr`/`prn` of an `#inst` prints the RFC3339 form with a `-00:00`
  offset and milliseconds.

## clojure.repl

- `source`/`source-fn` extract source **textually**: the defining
  namespace records its source file, and the def form is found by a
  regex on the var name plus balanced-paren extraction. Vars defined at
  the REPL, in builtins, or via macros that generate defs with computed
  names have no source to show (Clojure has the same limits, but keys
  off `:file`/`:line` var metadata instead).
- `pst` prints the exception message and `ex-data` only; there are no
  JVM stack frames to list.

## Privacy

- `defn-` marks the var `{:private true}` and `ns-publics`/`dir`/
  `apropos` exclude it, but privacy is **not enforced at resolution**:
  a fully-qualified reference to another namespace's private var still
  resolves (Clojure throws unless you go through `#'`/`var`).

## babashka.fs

- Time functions (`last-modified-time`, `creation-time`) return **epoch
  milliseconds**, not `java.nio.file.attribute.FileTime`; the
  FileTime/instant conversion helpers and the attribute-object API
  (`get-attribute`, `read-attributes`, `set-attribute`, `owner`,
  `set-creation-time`, `set-last-modified-time`) do not exist.
- `walk-file-tree` visitors receive a plain attrs **map**
  (`:directory?` `:regular-file?` `:size`), not `BasicFileAttributes`;
  `:skip-siblings` is not supported (treated as `:continue`).
- `match` glob patterns go through globset, which requires `**` to be a
  full path component; the bb idiom `glob:**.ext` is rewritten to
  `**/*.ext` before matching.
- `zip` stores entry names relative to `:root` (default: cwd) and errors
  on paths outside it; babashka stores paths as given. `unzip`
  extraction is sanitized (zip-slip entries are rejected, not written).

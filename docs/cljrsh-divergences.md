# cljrsh: intentional divergences

Where cljrsh deliberately differs from JVM Clojure and/or babashka.
Everything here is a decision, not a gap — gaps live in the issue
tracker. Parity items that USED to diverge and were fixed (decoder
keywordization via `Keyword::parse`, `with-redefs` on registry natives,
exit codes through `try/finally`) are not listed.

## Runtime model

- **Single-threaded.** One interpreter thread with a current-thread
  Tokio runtime. `future`, `pmap`, and `promise` exist and are
  API-compatible, but execute cooperatively on that thread — `pmap` is
  `map` with a different cost model, and `with-redefs`' "visible in all
  threads" is trivially true. Use OS processes (`cljrsh.process`) for
  real parallelism.
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

## Semantics

- `map?` returns false for `reify` instances (they are native objects,
  not maps, even when they implement lookup).
- `pr`/`prn` of an `#inst` prints the RFC3339 form with a `-00:00`
  offset and milliseconds.

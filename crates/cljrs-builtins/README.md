# cljrs-builtins

Built-in functions for clojurust (the `clojure.core`-equivalent runtime
implemented in Rust, registered into a name → fn dispatch table).

## Cargo profiles

The default `host-dependencies` feature propagates
`cljrs-env/host-dependencies`. Disable default features when the embedding
must compile builtins without Clojurust's dependency and Git resolution
subsystems. Other builtin behavior does not change.

## Map entries

Map entries are a dedicated type, not plain 2-element vectors: seq'ing a map,
`find`, and the `map-entry` constructor produce vectors tagged as entries
(`PersistentVector::map_entry` in `cljrs-value`).

- `(map-entry k v)` / `(map-entry coll)` — build an entry from a key and
  value, or from any seqable of exactly two elements.
- `(map-entry? x)` — true only for real entries; `(map-entry? [:a 1])` is
  false.
- `key` / `val` (bootstrap) — accept only real map entries and throw
  otherwise.

Entries otherwise behave exactly like 2-element vectors (equality, hashing,
printing, `nth`, destructuring), and, as in Clojure, any vector derived from
an entry (`conj`, `assoc`, `pop`, `subvec`, ...) is a plain vector again.

## Unchecked arithmetic

Includes the `unchecked-*` integer arithmetic family — `unchecked-add`,
`unchecked-subtract`, `unchecked-multiply`, `unchecked-inc`, `unchecked-dec`,
`unchecked-negate` (and their `-int` aliases) — which wrap on overflow, in
contrast to the checked `+`/`-`/`*` (which throw on overflow at the IR/compiled
tiers and promote to BigInt in the tree-walk tier).

## Docstrings (`doc` / `doc-data`)

`register_all` attaches `:doc` var metadata to native builtins from the
`BUILTIN_DOCS: &[(&str, &str)]` table (in `builtins.rs`), keyed by the name
the builtin is interned under. Not every builtin has an entry — special-form
stub vars and rarely-used internals are skipped, and a builtin later
redefined in `bootstrap.cljrs` (e.g. `swap!`, `partition`, `range`) carries
its docstring there instead, since the Clojure-level `defn`/`defmacro`
re-interns the var (see `cljrs-interp`'s README for how `def`/`defn`/
`defmacro` capture docstrings into var meta). Any builtin *may* carry a
docstring simply by adding a `BUILTIN_DOCS` entry; `#[cfg(test)] mod
doc_tests` in `builtins.rs` asserts every entry names something actually
registered, and that there are no duplicate names.

`doc-data` (`builtin_doc_data`, registered as a native fn) takes a `Var`
(`#'foo`), a value carrying attached metadata (`with-meta`), or a bare
function value, and returns `{:doc <string-or-nil> :arities <vector-or-nil>}`.
`:arities` prefers `:arglists` var metadata when present (real parameter
names, from `def`/`defn`/`defmacro`); otherwise it synthesizes placeholder
parameter names (`arg1`, `arg2`, ...) from a native fn's `Arity` shape, since
native fns don't carry real parameter names.

`clojure.core/doc` (a macro, defined in `bootstrap.cljrs`) wraps `(var sym)` +
`doc-data` in a `try`/`catch` so `(doc some-unbound-symbol)` returns `nil`
instead of throwing, and returns just the `:doc` string.

## Phase B3 — `shared-atom` (cross-isolate, two-tier atom ADR)

`shared-atom` is the cross-isolate tier of the two-tier atom design in
`docs/async-worker-pool-plan.md`.  Unlike `atom` (isolate-local, GC-backed),
its contents are promoted to a `Send + Sync` `SharedValue`
(`cljrs_value::shared`) behind a lock-free `ArcSwap`, so the reference can cross
the isolate boundary and be mutated concurrently:

- `(shared-atom x)` — construct, promoting `x` (non-promotable values such as
  closures and native resources are rejected here).
- `(shared-atom? x)` — predicate.
- `deref` / `reset!` / `swap!` / `compare-and-set!` — dispatch on
  `Value::SharedAtom` alongside the local `atom` path; writes promote, reads
  demote, and `swap!`/`compare-and-set!` use a single lock-free CAS with retry.

# Plan: cljrs-polars (Clojure bindings for the Polars dataframe engine)

## Overview

clojurust has two data-shaped capabilities today: `cljrs.num` (bulk numeric
kernels over `double-array`/`long-array` — the "numpy layer") and the runtime's
persistent collections. What it does not have is a **dataframe engine**: joins,
group-bys, window functions, `join_asof`, lazy query optimization, and
first-class Parquet/Arrow/CSV/IPC IO. cljrs-num is deliberately a kernel
library and should never grow those.

**The proposal:** bind [Polars](https://pola.rs) — the Rust-native dataframe
engine — as a `cljrs.polars` namespace. Polars ships Python/R/JS/Ruby
bindings; a Clojure-native binding does not exist anywhere (JVM Clojure wraps
it awkwardly through Java interop layers), and a Lisp is an unusually good
host for its expression DSL: `(-> (pl/scan-parquet "x.parquet")
(pl/filter (pl/> (pl/col "qty") 0)) ...)` is data all the way down.

**Non-goal:** replacing DuckDB in fibo-gen-clj. The constraint-enforcing load
and dialect-faithful derivation SQL stay where they are (see fibo-gen-clj's
`docs/QUERY-ENGINE.md`). This is a platform capability, justified by wanting
clojurust to have a real data story.

### Locked design decisions

- **Ship as a `cljrs-dylib` native package, not a workspace crate.** Polars
  pulls hundreds of crates and minutes of compile time; that does not belong
  in the core CLI build. The `:rust/load :dylib` mechanism exists for exactly
  this. (The workspace tolerates heavy in-tree crates — cljrs-qt, cljrs-lmdb —
  so this could be revisited, but the dylib route keeps `cargo build -p cljrs`
  fast and is the better citizen.)
- **Lazy API first.** `LazyFrame` is where Polars' optimizer lives and the
  API surface is smaller than eager `DataFrame`. Eager operations arrive
  later as conveniences over `collect`.
- **Expressions are Clojure data compiled to `polars::Expr`**, not strings.
  A small, printable, testable representation — no macros required in v0
  (functions building expr values), macro sugar optional later.
- **Handles are `NativeObject`s; columns bridge through cljrs.num arrays.**
  `Series ↔ double-array/long-array` conversion is the zero-friction interop
  seam (both are contiguous buffers; memcpy, zero-copy later if profitable).

---

## Architecture

```
crates-ext/cljrs-polars/          (or its own repo; loaded via cljrs-dylib)
  src/lib.rs        init/register into cljrs.polars (Registry pattern,
                    mark_loaded — same skeleton as cljrs-num/cljrs-base64)
  src/handles.rs    LazyFrameHandle, DataFrameHandle, ExprHandle as
                    NativeObjects (Trace = no children; Polars objects are
                    plain Rust data outside the GC heap)
  src/expr.rs       Clojure-data → polars::Expr compiler
  src/marshal.rs    Series ↔ primitive arrays / vectors; AnyValue ↔ Value
  src/io.rs         scan/read/write: parquet, csv, ipc, ndjson
  src/ops.rs        LazyFrame combinators
```

### Threading and GC

- Polars parallelizes internally with rayon. Every namespace call is a
  **blocking native call**: enter Rust, run Polars (which may fan out to
  rayon), return. No `Value`s cross into rayon workers; marshalling happens
  on the calling thread before/after. This sidesteps the GC entirely during
  compute.
- Handles hold `Arc<LazyFrame>`/`DataFrame` outside the GC heap. `Trace` is
  empty (the primitive-array trace bug taught us: empty trace is only
  correct because the NativeObjectBox itself is visited — add a regression
  test mirroring `gc_trace_primitive_arrays.rs` for a DataFrame handle held
  inside a collection).
- `collect` on a big frame can allocate GBs inside Polars; that memory is
  invisible to the GC's soft limit. Document it; optionally report
  `estimated_size` through a stats fn.

### The expression representation

Expressions are vectors/values built by `cljrs.polars` functions — no reader
magic:

```clojure
(pl/col "close")                        ; column ref
(pl/lit 2.5)                           ; literal
(pl/* (pl/col "qty") (pl/col "price")) ; arithmetic (variadic where Polars is)
(pl/> (pl/col "qty") 0)
(pl/agg-sum (pl/col "net"))            ; aggregations
(pl/alias expr "notional")
(pl/over expr [(pl/col "fund_id")])    ; window
```

Internally each returns an `ExprHandle` (NativeObject wrapping
`polars::Expr`) immediately — composing handles, not interpreting trees at
collect time. This keeps the Rust side a set of small constructors instead of
a tree-walker, and errors surface at construction with a source-shaped
message. A `pl/expr` data-literal form (`[:> [:col "qty"] 0]` → Expr) can be
layered on later for programmatic query construction.

---

## Phases

### Phase 1 — skeleton + IO + collect (the "it works" milestone) — DONE 2026-09-03

Implemented at `~/dev/cljrs-polars` (git, pinned Polars 0.46.0); loaded
end-to-end through the real `:rust/load :dylib` path and exercised against
fibo-gen-clj's parquet (flat files, Hive-partitioned globs, DECIMAL columns).
9 clojure.test tests / 27 assertions via an embedded-interpreter harness.
Learnings folded back into clojurust: the dylib wrapper now seeds its
Cargo.lock from the host workspace (an archery patch-version skew between
host and wrapper corrupted every rpds collection crossing the boundary), and
Polars *panics* abort the process across the dylib boundary (PolarsErrors
propagate cleanly). Decimal → f64 marshalling added beyond the original v0
dtype list.

- Crate skeleton, dylib packaging, `cljrs.polars` registration, version pin.
- `scan-parquet`, `scan-csv`, `read-parquet`, `read-csv` → LazyFrame/DataFrame
  handles; `collect`, `fetch` (row-limited collect).
- `write-parquet!`, `write-csv!`, `write-ipc!` on DataFrames.
- Introspection: `schema`, `shape`, `head` (returns Clojure vectors of maps
  for small results), `explain` (optimizer plan as string).
- Marshalling v0: `Series → double-array/long-array/vector-of-strings`,
  `column` accessor; `DataFrame → vector of row maps` (small frames only).
- Exit criteria: round-trip fibo-gen-clj's parquet output — scan, schema,
  head, collect, write — from the REPL.

### Phase 2 — the lazy relational core — DONE 2026-09-03

All listed ops and expressions implemented (18 clojure.test tests / 48
assertions). Exit criterion met: the position_eod LazyFrame program matches
DuckDB's 1,201,941-row output exactly on keys and to float epsilon / the
DECIMAL(20,4) quantum on values (gated: CLJRS_POLARS_FIBO=1 cargo test in
the package). Findings: polars' `AsOfOptions` derives `allow_eq: false` —
equal keys silently don't match unless set (we default to true, matching
DuckDB ASOF and python-polars); off-spine trade dates need an explicit
forward-asof alignment step where SQL's `trade_date <= as_of_date` filter
is implicit; the interpreter special-cases `/` in call position, so the
binding also registers `div`.

- `select`, `with-columns`, `filter`, `sort`, `limit`, `rename`, `drop`.
- `group-by` + `agg`; the aggregation expr family (sum/mean/min/max/count/
  n-unique/first/last/std/var).
- Joins: `join` (inner/left/outer/semi/anti, `:on`/`:left-on`/`:right-on`)
  and **`join-asof`** (the fibo forward-fill use case; strategy + `:by`).
- Expression library: arithmetic, comparison, boolean, null handling
  (`is-null`, `fill-null`), string ops (contains/replace/len), temporal ops
  (year/month/dt truncation), `when/then/otherwise`, `cast`, `over` windows.
- Exit criteria: reproduce fibo's `position_eod` derivation (trade cumsum ×
  date spine, asof-priced) as a LazyFrame program and match DuckDB's output.

### Phase 3 — ergonomics and integration

- Zero-copy or single-copy `Series ↔ cljrs.num` arrays both directions;
  `pl/from-arrays {"col" double-array ...}` frame constructor.
- Eager conveniences mirroring the lazy ops on DataFrame handles.
- Streaming collect (`collect {:streaming true}`) for larger-than-memory.
- `pl/sql` (Polars' SQL context) as a bonus query surface — subset, clearly
  documented as such.
- Docs: crate README per repo convention, a tutorial in `docs/tutorials`,
  and a worked fibo example.

### Phase 4 (optional, later) — sugar and depth

- `pl/expr` data-literal compiler; threading-macro-friendly aliases.
- Categorical/struct/list dtypes beyond passthrough.
- Arrow IPC interop with external processes (hand frames to DuckDB via IPC
  instead of CSV).

---

## Testing

- Rust unit tests per module (expr compilation golden tests: Clojure form →
  `Expr::to_string`).
- A clojure.test suite run via `cljrs test` covering the Phase-2 relational
  core against small hand-checked frames.
- The GC-handle regression test (handle inside a collection across forced
  collections).
- Determinism check: fibo `position_eod` reproduction vs DuckDB reference.

## Risks

- **Build weight**: Polars + deps ≈ 300–400 crates; dylib packaging isolates
  it but CI needs a dedicated job with caching. Pin a Polars version;
  `features = ["lazy", "parquet", "csv", "asof_join", "strings", "temporal",
  "dtype-date", "sql"]` and nothing else to start.
- **MSRV**: Polars tracks recent Rust; verify against the workspace toolchain
  (currently beta 1.95) before pinning — the kstring 2.0.4 incident says
  check first.
- **AnyValue marshalling breadth**: Polars has ~30 dtypes; v0 supports
  f64/i64/bool/str/date/datetime/null and *errors loudly* on the rest rather
  than guessing.
- **API churn**: Polars minor versions break; the binding surface is
  deliberately small and versioned with the package.

## Effort

Phase 1 ≈ 3–5 days; Phase 2 ≈ 1.5–2 weeks (expr library is the bulk);
Phase 3 ≈ 1 week. A credible, useful v0 (Phases 1–2) in **2–3 weeks**.

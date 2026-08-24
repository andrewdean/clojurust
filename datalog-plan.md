# Native Datalog plan: datalevin on cljrs

**Status:** assessment complete, 2026-08-23. Sized against datalevin
`78b199e8` (2026-08-23, EPL-2.0).

## Decision shape

Port datalevin's datalog engine to cljrs with the fork line at the
storage interface. Rust owns bytes and indexes through a native store
crate; Clojure owns parsing, planning, and execution. Vendor datascript
first as the in-memory tier and the portability proof. Retire the
datalevin pod when the native path lands.

The split follows the measured interop density. Datalevin's query side
is nearly pure Clojure: parser 0%, query_util 0%, interface 0%, the
5,119-line optimizer 4%, resolve/execute clean apart from one mutable
list type. Its storage and serde side is JVM-shaped by design: buffer
42%, hu (Hu-Tucker coding) 31%, bits 19%, txlog 14%, storage 11% with
464 interop lines. Porting the clean half and replacing the JVM half
with Rust plays each runtime to its strength.

## Evidence

- **cljrs gates are green.** `defprotocol`, `defrecord`, and `deftype`
  are implemented (cljrs-interp special forms); `BigInt`, `BigDecimal`,
  and `Ratio` are native Value variants. No language blocker.
- **The core path's Java imports are mostly Clojure-generated types.**
  `datalevin.parser BindColl`, `datalevin.bits Retrieved`,
  `datalevin.datom Datom`, `datalevin.storage Store`,
  `datalevin.interface IStore` are deftype/definterface classes from
  the Clojure sources; on cljrs these become ordinary record and
  protocol references. The mechanical rewrite is the `:import` forms.
- **True Java in the core path is small.** `datalevin.utl` (LRUCache
  93 lines, PriorityQueue 312, LeftistHeap 157, LikeFSM 185) plus the
  pervasive `org.eclipse.collections` `FastList` (and one
  `LongObjectHashMap` in join). Strategy: a native mutable-list shim
  over cljrs ObjectArray, and cljrs rewrites of the utl classes.
- **The remaining ~80% of the 103 Java files is severable**: snowball
  stemmers and SearchUtils (full-text), VectorIndex (vectors), the
  Java client API (Datalevin/Connection/Interop/Forms), HA log
  storage, and UDF plumbing. None are datalog-core.
- **Serde must be redesigned, not ported.** `bits.clj` builds on
  ByteBuffer, java.util, java.math, and nippy; value compression uses
  zstd-jni; integer lists use JavaFastPFOR. Replace with cljrs-native
  encoding (Rust side: zstd and bitpacking crates exist). This breaks
  file-format compatibility with JVM datalevin, which is acceptable:
  both ends are ours.
- **Optimizer-to-storage coupling is narrow.** Sampling flows through
  `step-sample` and the `IStore` surface (`SamplingWork` in
  storage.clj). The optimizer cannot be ported without a store, but
  the store contract is exactly the piece the Rust crate implements.
- **A conformance suite comes with the port**: 5,637 lines of tests,
  including query_optimizer_test and query_resolve_test, the same
  pattern as the vendored jank clojure-test-suite.
- **License**: datalevin is EPL-2.0; this repo is EPL-1.0. Both
  Eclipse licenses; record the version difference in COPYING when
  vendoring.

## Landscape survey (2026-08-23)

A crate survey before starting confirmed the port and altered the
storage substrate:

- **No whole-engine replacement exists.** CozoDB is abandoned (last
  release 2023-12, no maintainer response since 2024), Mozilla Mentat
  is archived with no revived fork, minigraf is a 30-star single-author
  project in a foreign dialect, and oxigraph/terminusdb-store are the
  wrong data model. The logic-engine family (ascent, crepe, datafrog,
  egglog) remains compile-time rules without storage.
- **Plan-altering find: dlmdb and dtlv.** Since 0.10.1 datalevin's
  storage is not stock LMDB but dlmdb
  (github.com/datalevin/dlmdb, BSD-3-Clause, active), huahaiy's C fork
  adding exactly what the store crate needs: O(log n) rank and
  count-range lookups, sparse sampling for optimizer statistics,
  prefix compression, optimized dup iteration, and an in-memory mode.
  The dtlvnative repo layers a dtlv C wrapper implementing datalevin's
  iterator, counter, and sampler surface. Phase 2 therefore evaluates
  binding dlmdb (a forked -sys crate plus a heed-style safe wrapper)
  before falling back to stock LMDB with hand-rolled sampling. Risk:
  dlmdb is single-maintainer; mitigation: it stays LMDB-API-compatible,
  so the fallback is mechanical.
- **No pure-Rust engine supports multi-process readers** as of
  2026-08: redb and fjall exclude it explicitly, canopydb and the
  lmdb-rs-core reimplementation are immature. The small C dependency
  stays, and it is the most boring one available.
- **Helper picks**: roaring (bitmaps), bitpacking (integer batches),
  quick_cache or moka (LRU), zstd bindings only if dlmdb's prefix
  compression proves insufficient. storekey and memcomparable are
  reference reading for the order-preserving key codec, which is
  hand-written to match datalevin's key semantics.
- **Port from the 1.0.x line.** Datalevin reached 1.0.0 on 2026-07-20;
  1.0.2 added late-clause costing, range fusion, and parallel index
  scans. The assessed commit `78b199e8` is post-1.0.2.

## Why LMDB and not redb here

cljrsh's access pattern is many short-lived processes sharing one
durable store. LMDB's multi-process MVCC (readers never block, one
writer through the environment lock) is built for that; redb is
single-process by construction. redb remains correct for daemon-owned
stores (smithy, swarmd). The Rust binding is heed; it compiles vendored
LMDB C, a small, stable, widely audited exception to the pure-Rust
default, and it stays contained inside the store crate.

## Phases

1. **Vendor datascript** as `cljrs` source: in-memory `q` for
   read-model queries, immediate removal of the pod fetch from the
   common path, and the cheap test of how this library family runs.
   **Done 2026-08-23.** datascript 1.8.1 runs on cljrs: the full query
   engine over plain collections (joins, predicates, fn bindings, not,
   rules, aggregates, collection/tuple ins) and the in-memory DB path
   (transact with upserts and unique identity, index queries, pull with
   ref navigation). Port cost: ~30 small `:cljrsh` reader arms in the
   vendored sources plus a pure-Clojure persistent-sorted-set shim, and
   eight genuine cljrs runtime fixes the spike surfaced (reader
   conditionals in require specs and fn params, params shadowing the
   fn's own name, meta-transparent type dispatch, get-in/dissoc/
   destructuring on records, JVM array identity equality, not-empty on
   cons/lazy-seq). Known gaps: the `entity` facade needs ILookup-style
   dispatch (use `pull`), and `slice` is O(n) in the shim. Conformance
   case 079 locks the port in. Verdict for phase 4: the datalevin port
   is viable; the same fix classes will recur and the runtime absorbed
   all of them.
2. **`cljrs-lmdb`**: native crate exposing environments, named DBIs,
   read/write transactions, cursors, and ranges. Substrate decision
   inside this phase: bind dlmdb (preferred: rank, count-range, and
   sampling come free) or stock LMDB via heed (fallback: sampling is
   hand-rolled in phase 3).
   **Done 2026-08-23, substrate: dlmdb.** Evaluation: dlmdb keeps the
   full stock LMDB API, compiles clean as two C files, and its
   extensions are exactly the optimizer's needs (count_all,
   count_range with MDB_COUNT_*_INCL bound flags, get_rank,
   key_rank, prefix compression, in-memory mode). License is the
   OpenLDAP Public License 2.8, not BSD-3 as the survey said. Vendored
   at d79120e2 under crates/cljrs-lmdb/lmdb with a hand-written FFI
   subset and a safe wrapper (zero-copy txn-scoped reads, in-process
   writer mutex, inclusive RangeIter, dupsort cursors). Eight tests
   including snapshot isolation and a re-exec cross-process reader.
   Custom comparators deliberately unexposed: phase 3 uses
   order-preserving key encoding over the default byte comparator.
3. **`cljrs-datalog-store`**: implement datalevin's `IStore`/lmdb
   protocol surface in Rust: EAV/AVE/VAE orderings as sorted keys,
   native serde (replacing bits/nippy/zstd-jni/FastPFOR), sampling
   for the optimizer. With dlmdb underneath, this shrinks toward
   serde, schema, and transaction logic; the dtlv C layer is the
   reference implementation of the iterator and sampler surface.
   **Core done 2026-08-23.** crates/cljrs-datalog-store: eav/ave/vae
   counted indexes with the order-preserving codec (randomized
   byte-order-equals-value-order test), attr interning, cardinality
   semantics, ref reverse index, content-addressed giants, O(log n)
   count/cardinality via rank differences, and rank-strided sample_ave.
   Ten tests. Remaining for phase 4 wiring: the exact IStore-shaped
   surface the ported Clojure calls (thin adapter over this API) and
   giant garbage collection.
4. **Port the query family** (~20k lines: parser, optimizer, resolve,
   plan, execute, rules, built-ins, db, conn, datom, entity, pull)
   against a pinned datalevin tag, with the FastList shim and utl
   rewrites. Vendor with attribution; re-sync deliberately, never
   track.
5. **Port the test suite** as the conformance gate.
6. **Swap the `cljrsh.datalog` veneer** to the native engine (the API
   already matches) and retire the pod.

## Non-goals

Full-text search, vector search, the server/client/remote/HA/MCP
stack, the JSON document API, UDFs, and JVM file-format compatibility.
Any of these can be revisited once the datalog core is proven.

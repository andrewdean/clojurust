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
2. **`cljrs-lmdb`**: native crate over heed exposing environments,
   named DBIs, read/write transactions, cursors, and ranges.
3. **`cljrs-datalog-store`**: implement datalevin's `IStore`/lmdb
   protocol surface in Rust: EAV/AVE/VAE orderings as sorted keys,
   native serde (replacing bits/nippy/zstd-jni/FastPFOR), sampling
   for the optimizer.
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

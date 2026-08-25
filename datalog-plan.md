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
   **Native boundary wired 2026-08-23**: `cljrs.dstore.native` exposes
   the store to Clojure (attr-aware value conversion, refs coerced for
   ref-typed attrs, dual long/ref search merge), and `cljrs.dstore`'s
   DurableDB satisfies datascript's IDB/ISearch/IIndexAccess, so the
   vendored engine runs q and pull against disk unmodified: joins,
   predicates, vae ref-joins, nested pull, O(log n) counts, and reopen
   persistence (conformance case 080). dlmdb's exported symbols are
   compile-time prefixed (dlmdb_*) so it links beside the stock LMDB
   already in cljrsh's tree via heed. The datalevin optimizer port now
   proceeds against a live durable target; -rseek/-index-range and the
   entity facade remain unsupported on DurableDB until then.
   **Foundations rung done 2026-08-23**: datalevin.util,
   datalevin.constants, and datalevin.datom (from the pinned 78b199e8)
   load and work on cljrs — Datom construction and both comparators,
   distinct-by, seeded reservoir sampling (portable xorshift replacing
   java.util.Random; atom-map cache replacing LRUCache), byte/bigint
   sentinels made portable, FastList and defcomp/defrecord-updatable
   given :cljrsh arms (conformance case 082). Runtime gained
   unchecked-byte/short/int and variadic bit-and/or/xor. Reminder: the
   feature set includes :clj, so vendored gates must lead with a
   :cljrsh arm; bare #?(:clj ...) still fires. Parser and interface rungs done
   2026-08-23: datalevin.query-util, datalevin.parser (validation,
   clause typing, rules, find-vars all verified), and
   datalevin.interface (8 protocols) load and work; extend-type
   accepts nil upstream and seqable? has a portable arm. built-ins
   strategy: the portable predicate/aggregate halves vendor next, with
   the contiguous fulltext/vector/idoc block (lines ~210-770) and its
   query-fns entries gated to :clj until the db/storage adapters exist;
   its ns requires on db/storage/idoc/embedding gate the same way.
   Built-ins rung done 2026-08-23: datalevin.built-ins loads with 84
   query-fns; like/not-like (SQL LIKE with escape chars, portable
   regex compiler + atom caches), in/not-in, and the predicate and
   aggregate registry all verified; the storage-dependent block
   (fulltext/vector/idoc, ~580 lines) stubs with raise until the
   db/storage adapters land (conformance case 083). Surfaced and
   fixed an evaluator-family bug: clojure.core-qualified special
   forms ((clojure.core/or ...) etc.) fell through to nil-returning
   stub calls in all three dispatch paths (tree-walker, IR lowering,
   async driver); each now strips the clojure.core/ prefix before the
   special-form check. Relation-substrate rung done
   2026-08-23: datalevin.timeout and datalevin.query.predicate vendored
   verbatim; datalevin.query.tuple with the index/giant decode gated to
   :clj (the cljrs store decodes giants natively, so [:g gt] doc-refs
   never occur); datalevin.relation as a whole-file two-arm split — on
   :cljrsh a relation's :tuples is a persistent vector of object-array
   tuples (the FastList convention from datalevin.util), tuple identity
   is the tuple's vec (replacing HashSet + ArrayWrapper), and mutable
   seen-sets are volatiles of sets created with the additive
   new-seen-set constructor (both arms define it; callers gate their
   HashSet. allocations to it when vendored). Full algebra verified:
   sum/sum-dedupe/difference/renumbering/prod/project-distinct/
   many-tuples/seen-set iteration/timeout abort (conformance case 084).
   Runtime gained System/arraycopy (all array families, overlap-safe),
   so join-tuples/conj-tuple run upstream-original. Pipes + range-scan
   natives rung done 2026-08-23: datalevin.inline and datalevin.pipe
   vendored (the :cljrsh pipe keeps the public surface over volatile
   state, single-threaded, honoring the :datalevin/end-scan sentinel;
   additive new-sink/sink-seq/add-one helpers in both arms);
   cljrs-datalog-store gained slice/rslice/count-range with
   partial-datom open/closed bounds over all three indexes, exposed as
   cljrs.dstore.native slice/rslice/count-range. Storage adapter rung
   done 2026-08-23: datalevin.storage REPLACED (not vendored) with a
   cljrsh-only implementation over cljrs.dstore.native — schema with
   synthesized stable aids, the slice/size/head/filter families with
   sentinel translation, the optimizer's physical operators
   (ave-tuples, sampled variants, eav-scan-v with :pred/:fidx/:skip?
   and card-many products, val-eq-scan-e/-filter-e), and the
   search-tuples case-tree helpers; cardinality is exact under 16k
   datoms, else the datom count (statistic only). Db rung done
   2026-08-23 (21d76141): datalevin.db replaced with the query-path
   surface — upstream protocols verbatim, DB record over
   datalevin.storage with the upstream case trees, helpers via a
   vendored datalevin.db.tx.common (pending-tx probes gated off);
   datalevin.validate replaced with the eight query-path validators;
   storage gained a datalevin.interface IStore bridge (ns-qualified
   bodies avoid method-name self-capture). Upstream fixes: protocol
   method impls accept the multi-arity clause form and repeated
   same-name forms merge arities (defrecord/deftype/reify/extend-type/
   extend-protocol); record field injection reaches clause bodies.
   Conformance case 085. Not ported: datalevin tx pipeline (writes go
   through storage/load-datoms), pending-tx caches, result caches,
   remote, UDF. Join rung done
   2026-08-23 (59b08dc2): datalevin.join with a persistent-map
   :cljrsh arm (vec join keys, vector buckets, pipe/add-one sinks);
   u/intersection gained a portable arm; the runtime gained mutable
   java.util collection shims — JavaArrayList (ArrayList./FastList.),
   JavaHashSet, LongObjectHashMap. alias — integrated with the seq
   family and ValueIter, plus a fix for (get transient-map k)
   returning nil. Rules rung done 2026-08-23 (d1e736d7):
   datalevin.rules vendors near-verbatim on the shims (only imports
   gated + portable fill-rule-output-and-hash); parse/SCC/dependency/
   stratification verified, solve-stratified awaits the resolve
   rung's query context; runtime gained locking, while, (Object.).
   Conformance case 086. Resolve rung done
   2026-08-23 (4d2193c2): datalevin.query.resolve + query.aggregate
   near-verbatim; verified against the durable store: resolve-ins,
   lookup-pattern, patterns/predicates/fn-bindings/or/not/not-join,
   and RECURSIVE RULES end-to-end (semi-naive stratified transitive
   closure, unbound and bound), grouped and ungrouped aggregates.
   relation seen-sets are polymorphic (HashSet shim or volatile).
   Upstream: defprotocol return-type hints, :keys type hints,
   callable vectors, nth on arrays, read-only List interop on
   vectors, HashMap views, Object class token, IdentityHashMap
   alias. Conformance case 087. Note: query.cache requires
   query.execute, so it lands with the execute rung. Planner rung done
   2026-08-23 (fa77f143): query.access, optimizer.graph (portable
   idx/value-type inline), optimizer.range (portable max-string,
   like validation via built-ins compiler), query.plan, and
   query-optimizer (5.1k lines) all load; graph building with
   predicate pushdown verified on the durable store. Portable arms:
   SIP bitmaps = volatile sets; plan/writing? = true so every
   parallel pipeline collapses onto the upstream sequential fallback
   branches; DPK record replaces DPKey; plan cache = datascript LRU
   in a volatile; FnSink replaces the AbstractCollection proxy.
   Runtime feature: interop method calls on deftype/defrecord
   dispatch to protocol methods ((.-field x) stays field access);
   CompletableFuture/ConcurrentHashMap/putIfAbsent shims; pipes
   answer .add/.addAll. Conformance case 088. Engine COMPLETE
   2026-08-23 (ca864edc): execute, access.ave, access.function,
   cache, pull-parser, pull-api, and the datalevin.query facade all
   vendor; (q '[:find ...] db) runs the complete pipeline — parse,
   cost-based planning, step execution — natively against the Rust
   LMDB store. Twenty-one query shapes verified (conformance 089):
   joins, predicates, :in, scalar/coll/tuple finds, aggregates,
   not/or/not-join, recursive rules, fn bindings, :order-by/:limit/
   :offset, pull attr-lists/nested-refs/wildcard/find-specs,
   collection sources. Load-bearing: the storage adapter now uses
   NATIVE aids (attrs_with_aids) so schema aid order matches eav
   iteration order — pull's merge join depends on it. Deferred:
   fulltext/vector/idoc access (:clj-gated), result caching (needs
   db invalidation hooks), remote paths. Phase 6 done
   2026-08-24 (49bccc30): cljrsh.datalog runs everything natively —
   collections/dstore on datascript, durable conn/db/transact!/q/pull
   on the datalevin engine; transact! translates Datomic tx-data with
   two-pass tempid resolution (entity + ref-value positions); the pod
   is retired (pod-era databases need re-import; noted in ns doc).
   Conformance case 090. Pod-era migration shipped 2026-08-24
   (7da4ff10): cljrsh.datalog/migrate-pod-db! loads the retired pod
   one last time (cached dtlv), reads schema + eav datoms, and writes
   them natively with entity ids preserved; verified end to end (no
   live pod databases existed on this machine — the helper is for
   anyone who has one). Phase 5 in progress 2026-08-24 (4fe28544):
   conformance case 091 vendors datalevin's own test suites (pinned
   78b199e8) and runs them under clojure.test against the native
   engine — datalevin.core facade vendored (query family re-exports +
   conn/tx over the minimal translator, moved from the veneer);
   query-resolve and index suites fully green and gating; query-not
   green pending final verification; query-optimizer vendored, first
   run pending. Engine gaps the suites surfaced and fixed: nil-as-
   empty-set in clojure.set, populated? nil contract, end-scan gated
   to pipes, :db/unique flags in the native store + lookup-ref
   resolution, ordered tuple (vector) values in the codec, conj onto
   records (clojure.walk/explain), sequential partitioned execution
   (:cljrsh arms), use-fixtures in clojure.test. Remaining: the
   deferred features (result caching with invalidation hooks, the
   datalevin tx pipeline with uniqueness/component semantics,
   fulltext/vector/idoc, entity facade, giant GC, perf promotion of
   hot scan loops into Rust).
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

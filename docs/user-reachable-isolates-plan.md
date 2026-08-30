# User-Reachable Isolates — Phase C

## Why this document exists

`async-worker-pool-plan.md` (the governing ADR) and `isolate-boundary-plan.md`
are decided and — as of v0.2.27 — mostly **built**: `GcPtr` is honestly
`!Send`, every OS thread owns an independent heap that collects in parallel
(`ISOLATE_HEAP`), and the metered structured-clone boundary exists and rejects
non-shareable values with located errors.

What is missing is the **door**: no Clojure program can start an isolate.
`Isolate::spawn` is test-only, so `future`/`pmap`/`go` all share one core and
`isolate-chan` typically has both ends on the same heap. Phase C is the
user-facing layer on the landed substrate, plus the honesty debt that becomes
visible the day two isolates actually run.

Status survey that produced this plan (2026-08-30):

| Component | Status |
|---|---|
| Honest `!Send` `GcPtr` (A1) | landed — `cljrs-gc/src/lib.rs` (`NonNull`, no unsafe impl) |
| `Send`-only worker pool (A2) | landed — `worker_pool.rs`; used by net/tls/quic/h2/h3 |
| Per-isolate heaps, parallel GC (B1) | landed — `ISOLATE_HEAP` thread-local |
| Copy boundary + metering (B2) | landed — `clone.rs`; crossings metered in `GC_STATS` |
| Static arena: keyword/symbol identity (B3) | landed |
| `shared-atom` / hybrid var roots | partial — contents stop at scalars/strings/interned kw+sym/byte blobs |
| Waker-based parking | **done** — core paths 2026-08-27; residual spins converted in C1 |
| Clojure-level isolate spawn | missing |
| Agents | missing — `send` errors "not yet implemented" |
| Memory-pressure coordinator | designed, not built |
| `ref`/STM | absent (declared a non-goal below) |

## Decisions

### D1 — Work ships as a qualified symbol, never a closure

Closures are non-shareable by construction (`clone.rs` rejects every fn type —
they capture isolate-local GC state), and that stays the rule. The unit of
cross-isolate work is **a fully qualified symbol plus arguments**: the target
isolate requires the namespace, resolves the symbol, and applies it to the
deep-copied args. Underneath, the primitive is "evaluate this form" — forms
are lists/symbols/keywords, all already shareable. Lexical capture is replaced
by explicit arguments, which keeps every crossing visible at the call site
(the boundary plan's source-visibility rule). Arena-resident compiled fns
(zero-copy fn crossing, the var-hybrid's deferred option (a)) remain deferred;
they slot in later as an optimization of the same surface.

### D2 — Surface API: isolate handles + `pfuture`

```clojure
(def iso (isolate))                     ; spawn: own OS thread, heap, runtime
(isolate-call iso 'my.ns/crunch data)   ; → reply future; args deep-copied
(isolate-close! iso)                    ; graceful shutdown, drains in-flight
(pfuture (my.ns/crunch data))           ; sugar: default pool; macro requires
                                        ; a (sym args…) call form — no closures
```

- `isolate` returns a handle — a `Resource`, therefore itself non-shareable,
  so handles cannot leak across boundaries. Options map for stack size, GC
  config, preloaded namespaces.
- `isolate-call` returns a `CljxFuture` settled from a reply `isolate-chan`.
  Isolate panic or death settles the future with a located error carrying the
  isolate name. This is the distinct parallel primitive the boundary plan
  requires — `future` stays loop-async, untouched.
- `pfuture` targets a lazily-created default pool sized
  `available_parallelism()`, size 1 under test for determinism. It accepts a
  call form, not an arbitrary body, so the no-closure rule is syntactic and
  the error fires at expansion, not at runtime mid-scheduling.
- Isolate warm-up (namespace loading) is real; mitigations are pool reuse and
  the preload option. Metered like everything else at the seam.

### D3 — Agents become loop-async mailboxes, isolate-local

Implement `agent`/`send`/`send-off`/`await` as an isolate-local mailbox: a
channel plus one serial consumer task on the LocalSet, state in the existing
(currently dead) `Agent` struct. Same cost model as `future` — cooperative,
no threads — which matches the documented divergence stance and closes the
false "agents complete" claim cheaply. An agent that must live elsewhere is
composition, not a primitive: run it inside an isolate and reach it via
`isolate-call`.

### D4 — `SharedValue` grows Arc-backed persistent collections

Today a `shared-atom` cannot hold a map. Implement the ADR's already-decided
`Arc<…>` arm: acyclic, immutable, refcounted vectors/maps/sets/lists as
`SharedValue` variants, produced by promote-on-publish (deep copy into Arc
form), read through a shared `Value` variant without copying. This is also the
representation the deferred `shared-vec` fast path needs, so it is built once
and reused when boundary telemetry justifies zero-copy sends. Cycles remain
impossible by the acyclic-contents restriction; no shared-tier collector.

### D5 — Honesty debt is C1, not cleanup

- **Residual spin loops first** *(done — see `async-await-spin-bug.md`)*:
  `isolate-take!` sat directly under `isolate-call`'s reply path, so D2 built
  on it would have resurrected the mised pegged-core bug at the first idle
  worker. All four sites (`mult`, `onto-chan!`/`to-chan!`, `thread-call`,
  `isolate-take!`) now park; guarded by `tests/channel_parks.rs`.
- **`locking` stays a no-op — by argument, not by accident.** Share-nothing
  means no two isolates can ever reference the same GC object; the only
  cross-isolate mutables are `shared-atom` (lock-free CAS) and channels
  (synchronized). A no-op monitor is semantically sound even with N isolates.
  Record that argument in `cljrsh-divergences.md`; do not build a lock nobody
  can contend.
- **`Thread/sleep` gets an async-aware warning** in docs (it blocks the whole
  executor); the go-to is `(<! (timeout ms))`.

### D6 — Documentation tells the truth; STM is a declared non-goal

README ("agent (with send/await)" complete), TODO Phase 7, and CLAUDE.md
("ref/STM") all claim things the code contradicts. Correct them in the same
change that lands D3. Declare `ref`/`dosync` a **non-goal**: STM exists to
coordinate shared mutable state across threads, and the model's answer to that
is isolates + `shared-atom`; a same-thread STM would be ceremony. The
memory-pressure coordinator stays designed-not-built until a multi-isolate
workload exists to signal — it is sequenced after D2, not before.

## Phase plan

| Phase | Deliverable | Crates |
|---|---|---|
| C1 | Waker-complete channels: the four residual spin loops, regression-tested — **done 2026-08-30** | `cljrs-async` |
| C2 | Isolate handles, `isolate-call`, reply futures, `pfuture` + default pool (D1, D2) — **done 2026-08-30** (`isolate_call.rs`; two concurrent calls verified ≈1× wall-clock of one) | `cljrs-async` |
| C3 | Agents as loop-async mailboxes; docs truth pass rides along (D3, D6) — **done 2026-08-30** (`schedule_agent_drain` on the AsyncRuntime hook; `(await agent)`; README/TODO/CLAUDE.md/divergences corrected) | `cljrs-async`, `cljrs-value`, `cljrs-builtins`, docs |
| C4 | Arc-backed `SharedValue` collections; `shared-atom` holds maps (D4) — **done 2026-08-30** (flat `Arc<[SharedValue]>` slices, promote-on-publish, demote-on-read; the zero-copy read view stays deferred with `shared-vec`) | `cljrs-value` |
| C5 | Pressure coordinator (`watch<PressureLevel>` per the existing design), boundary-telemetry review, `shared-vec` go/no-go | `cljrs-gc`, `cljrs-async` |

C1 is a prerequisite for C2. C3 and C4 are independent of each other and can
interleave. C5 is deliberately last — it consumes telemetry the earlier phases
produce.

## Risks and open questions

- **Binding conveyance.** JVM Clojure conveys dynamic bindings into `future`
  bodies. Cross-isolate, conveying means copying the promotable frame per
  call. Proposal: do *not* convey through `isolate-call`/`pfuture` in C2;
  document it; revisit if real code misses it.
- **Keyword intern contention.** The global intern table is one mutex; N
  isolates interning at startup will contend. Measure during C2; the sharded
  table is the known answer if it shows up.
- **Isolate warm-up cost** could make `pfuture` disappointing for small tasks.
  The pool amortizes it, but the honest framing is Erlang's: isolates are for
  workers, not for expression-level parallelism.
- **ARM.** Multi-isolate execution multiplies exposure to the unaudited
  `Relaxed` orderings and the per-arch stack scan. A multi-isolate stress test
  on `ubuntu-24.04-arm` belongs in C2's definition of done.

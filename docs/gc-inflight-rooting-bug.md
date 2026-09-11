# GC: in-flight closure freed during builtin callback re-entry

Status: CLOSED. The class fix landed in three steps and the exit
criterion at the bottom is met:

- 580eec7d (2026-08-25): spot fixes — callback::invoke roots its callee
  and args on the Fn fast path; into/reduce/sort/sort-by/merge_sort root
  the Values they hold in Rust locals across re-entry.
- 2d6eb25e (2026-08-25): layer 1 — the conservative stack scan
  (`crates/cljrs-gc/src/stack_scan.rs`) runs on every production
  collection path, so a Value held only in a Rust local can no longer be
  freed. Layer 2 was replaced by a rescue counter (see "What landed").
- 00e4ed37 (2026-08-27): the audit the scan made cheap found five more
  use-after-free root causes (ValueRootGuard LIFO pop, PersistentHashMap
  index tracing, primitive-array Trace arms, a batch of missing Trace
  edges, and rooted-slice lifetime holes) and fixed them; ValueIter
  self-roots (layer 3).
- 2026-08-28: the datalevin optimizer suite is fully green and gates as
  `datalevin_optimizer_suite` (conformance case 091), with zero
  conservative rescues throughout.

Found 2026-08-25 by the datalevin query-optimizer test suite. The
repro below runs clean. Kept for the record.

## Symptom

Running the same allocation-heavy query twice in one process crashes on
the second run:

- debug build: `assertion failed: GcPtr::get() on freed object!` at
  `crates/cljrs-gc/src/lib.rs:379`, on a `GcPtr<CljxFn>`
- earlier full-suite run: `SIGSEGV` with `si_code: SEGV_ACCERR`
  (4G truncated core, `coredumpctl` 2026-08-24 20:39)
- when the recycled memory still parses, silent misbehavior instead of
  a crash (observed as a spurious "Insufficient bindings" query error)

## Repro

    target/debug/cljrsh docs/gc-inflight-rooting-repro.cljrs

(needs no test corpus — only the vendored datalevin engine in the
binary). The script transacts 2000 items and runs an
or-join + order-by/limit query twice. The first `q` succeeds; the
second panics.

## Backtrace shape (RUST_BACKTRACE=full, debug build)

    GcPtr<CljxFn>::get                     <- freed closure
    cljrs_env::apply::dispatch_if_async
    cljrs_env::callback::invoke
    cljrs_builtins::builtins::builtin_into <- builtin re-enters eval
    cljrs_env::apply::apply_value
    cljrs_interp::apply::eval_call_inner
    ... ordinary interpreter frames ...

## What it is NOT

All of these were tested and ruled out as the cause:

- the datalevin plan cache (`qo/*plan-cache*`): disabling lookup and
  store entirely still crashes
- the parsed-query cache (`qcache/parsed-q` memoization): disabling it
  still crashes
- def'd-volatile escapes, plain-closure captures, reify captures: all
  survive GC pressure in isolation probes

## Working theory

The crash needs *heap pressure carried over from the first query*: the
second query triggers collection at different points, and one of those
points lands inside `builtin_into`'s callback re-entry while a live
closure (the transducer/callback fn) is held only in a Rust-side local
of the builtin — not on the shadow stack, not in an alloc frame, not
reachable from any env. The collector frees it; the next
`callback::invoke` reads the freed `CljxFn`.

If that's right, the audit surface is: builtins that keep `Value`s in
Rust locals across a `callback::invoke`/`apply_value` re-entry
(`into`, `reduce`, `map`-family, sort comparators, ...). Either those
locals need shadow-stack registration for the duration of the
re-entry, or callback re-entry needs to pin its callee.

## Impact

- datalevin query-optimizer suite: several tests crash or misbehave
  when run in one process (the sequence matters, not any single test)
- any long-lived cljrsh process that runs repeated allocation-heavy
  queries is exposed
- the phase-5 conformance gate (case 091) currently excludes the
  optimizer suite because of this

The suites that do gate (query-resolve, query-not, index) pass green;
their runs evidently do not hit the vulnerable collection window.

## Open audit question

The fix covers callback::invoke and the builtins that were implicated.
Any OTHER native code that (a) holds a `Value` in a Rust local, then
(b) re-enters evaluation (callback::invoke, apply_value, or realizing
a lazy seq via ValueIter) has the same exposure. A systematic audit —
or a debug-build assertion that flags unrooted GcPtrs reachable from
the C stack at collection time — would close the class for good.

## What landed (class fix proposed 2026-08-25, complete 2026-08-28)

Sizing the exposed surface: ~33 `callback::invoke` call sites (18 of
them in `rt_abi.rs`, so AOT-compiled binaries share the class and got
no spot fixes), 28 `ValueIter::new` sites, plus the comparator paths.
One instance survives inside the fixed builtins themselves: `into`
roots its reducing fn and accumulator but not `ValueIter`'s internals,
and each freshly realized rest of a lazy seq lives only in the
iterator's Rust field, unrooted across the next invoke.

Three layers, in implementation order:

1. **Conservative stack scanning as an additional root source** — DONE
   (2d6eb25e). Sound
   because the heap is non-moving mark-sweep and `GcPtr` is `!Send`: a
   false positive retains garbage for one extra cycle, never corrupts.
   Record each mutator's stack bounds at `register_mutator` and its SP
   at `park_thread`; the collector spills its own callee-saved
   registers through a small shim, then scans every word-aligned slot
   of each `[SP, base]` range against the set of live object addresses
   (built from the allocation list the sweep already walks; sorted vec
   plus binary search). Hits get marked. After this no Rust local can
   be freed under a native frame, builtin authors need zero rooting
   discipline, and `GC_INITIAL_LIVES` can eventually drop to 1,
   recovering the memory the one-cycle grace retains.
   As built: the scan rides `collect_with_stack_scan`, which
   `gc_safepoint`, `force_collect`, and `async_gc_collect` all use;
   plain `collect` stays precise-only so tests asserting exact free
   behavior hold. The stack ceiling is `record_stack_base` (wired into
   `register_mutator`) with a pthread fallback on Linux and macOS —
   NOT `/proc/self/maps`, whose merged VMAs run into neighboring
   thread stacks. `CLJRS_GC_CONSERVATIVE=0` disables it. Verified on
   x86_64 Linux and arm64 macOS; other architectures scan the stack
   but cannot spill callee-saved registers.

2. **The same scanner as a debug-mode detector** — REPLACED by a
   counter. A panicking detector false-positives on stale dead stack
   slots that happen to hold an old object address, so instead every
   rescue after precise marking increments `Conservative rescues` in
   `--gc-stats` and logs at debug level; a nonzero count is the audit
   signal for a remaining unrooted path. The deterministic hunting
   tools are `CLJRS_GC_QUARANTINE=1` (sweep poisons and leaks dead
   boxes so any later access panics with an exact message) and
   `CLJRS_GC_STRESS=N` (force a collection every Nth safepoint), both
   in `crates/cljrs-gc/src/debug_modes.rs`. Original proposal: run
   the scan after precise marking and flag any stack word that points
   at a live-but-unmarked object: panic with the address and the
   object's `trace_fn` type. Every remaining audit miss (`rt_abi.rs`
   first) becomes a deterministic test failure under the optimizer
   suite, and new builtins cannot silently lean on the conservative
   layer.

3. **Root the re-entry choke points themselves** — DONE.
   `callback::invoke` (580eec7d); `ValueIter` holds a `ValueRootGuard`
   for its lifetime (91f856c0), which is what exposed the LIFO-pop bug
   in the guard itself (00e4ed37: guards now remove their own entry by
   identity). Root guards also carry the slice lifetime now, so the
   borrow checker rejects rooting a buffer that later moves or dies.

Exit criterion: the datalevin optimizer suite runs green with zero
conservative rescues, then case 091 admits it to the conformance
gate. Met 2026-08-28 (25 tests, 228 assertions, 0 failures, 0 errors;
`datalevin_optimizer_suite` in `crates/cljrsh/tests/conformance.rs`,
`#[ignore]`d for its ~2h debug-build runtime).

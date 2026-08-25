# GC: in-flight closure freed during builtin callback re-entry

Status: FIXED (580eec7d) for the known paths — callback::invoke now
roots its callee and args on the Fn fast path, and into/reduce/sort/
sort-by/merge_sort root the Values they hold in Rust locals across
re-entry. The repro below runs clean. Kept for the record and for the
open audit question at the bottom. Found 2026-08-25 by the datalevin
query-optimizer test suite (conformance case 091 work).

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

## Proposed class fix (2026-08-25, cljrsh-rust-shell session)

Sizing the exposed surface: ~33 `callback::invoke` call sites (18 of
them in `rt_abi.rs`, so AOT-compiled binaries share the class and got
no spot fixes), 28 `ValueIter::new` sites, plus the comparator paths.
One instance survives inside the fixed builtins themselves: `into`
roots its reducing fn and accumulator but not `ValueIter`'s internals,
and each freshly realized rest of a lazy seq lives only in the
iterator's Rust field, unrooted across the next invoke.

Three layers, in implementation order:

1. **Conservative stack scanning as an additional root source.** Sound
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

2. **The same scanner as a debug-mode detector.** In debug builds run
   the scan after precise marking and flag any stack word that points
   at a live-but-unmarked object: panic with the address and the
   object's `trace_fn` type. Every remaining audit miss (`rt_abi.rs`
   first) becomes a deterministic test failure under the optimizer
   suite, and new builtins cannot silently lean on the conservative
   layer.

3. **Root the re-entry choke points themselves.** `callback::invoke`
   is done (580eec7d). Give `ValueIter` a self-root: an internal boxed
   slot registered on the shadow stack for the iterator's lifetime,
   updated in place as it advances, covering all 28 sites without
   local audits.

Exit criterion: the datalevin optimizer suite runs green under the
debug detector, then case 091 admits it to the conformance gate.

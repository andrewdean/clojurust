# Async: `await` spin-waited, so an idle daemon pinned a full core

Status: FIXED — by the commit this document lands in ("async: park
awaits on wakers instead of yield_now spin loops", branch cljrsh).
`await_value` and the compiled
poll path now park on registered wakers; the GC service task ticks on a
timer instead of a `yield_now` loop. Regression tests in
`crates/cljrs-async/tests/await_parks.rs` are red on the old code.
Found 2026-08-27 when `mised.cljrs` (the resident mise env daemon) sat
at 99.7% CPU while serving zero requests. Kept for the record and for
the residual spin loops listed at the bottom.

## Symptom

Any daemon whose steady state is an async park — the canonical shape:

    (loop []
      (await (a/timeout 60000))
      (recur))

burned a full core while completely idle. Observed on mised.cljrs:
25 minutes of CPU time in 25 minutes of uptime, `%CPU 99.7`, with the
socket server idle. The spin profile from `/proc`:

- main thread parked on a futex (fine)
- the `cljrsh-main` LocalSet thread `R (running)` with ~20 *voluntary*
  context switches total against ~54,000 involuntary ones — i.e. the
  thread never once entered the kernel to wait; pure userspace spin

A 15-second idle run of the loop above under the pre-fix binary
consumed 1495 clock ticks (14.95 s) of CPU. Under the fixed binary: 0.

Notably, `inbox-watch.cljrs` — the other resident daemon — did *not*
show this, which is exactly backwards from what you'd want: its
blocking `Thread/sleep` freezes the whole interpreter thread, executor
included, so nothing can spin. mised's *correctly written* async park
is what exposed the bug. Writing idiomatic async code was penalized;
blocking the executor was rewarded.

## Root cause

The runtime had no way to wake a task when a `Value::Future` or
`Value::Promise` settled, so every wait was implemented as a
poll-and-yield loop. `tokio::task::yield_now()` does not sleep — it
re-wakes its own task immediately — so any task inside such a loop is
*permanently runnable*, and a `LocalSet` executor with a runnable task
never parks in the reactor. One spinner is enough to pin the core;
mised's steady state had three:

1. **`await_value` (crates/cljrs-async/src/eval_async.rs)** checked the
   future/promise state, called `async_gc_collect()`, and
   `yield_now().await`, in a loop. Every cljrs-level `(await ...)` on a
   pending value was a busy-wait: the top-level `(a/timeout 60000)`
   park, and the `start-server` conns-dispatch go block sitting in
   `(await (a/take! conns))`. The bitter part: `CljChannel` already had
   proper async wakeups (`Notify`-based `take().await`/`put().await`,
   used correctly by cljrs-net), so the *channel* taker task parked
   fine — and then the go block *awaiting that taker's future* spun.

2. **The GC service task (crates/cljrs-async/src/runtime.rs)** was
   itself `loop { yield_now().await; async_gc_collect() }` — a second
   permanently-runnable task, keeping the executor hot even when
   everything else was parked. (The wasm32 comment in that function
   already described this exact failure mode — "an endless chain of
   microtasks" — and skipped the service there, but the native path
   kept the loop.)

3. **`CompiledAsyncTask::poll` (crates/cljrs-async/src/state_machine.rs)**,
   the JIT/AOT poll path, returned `Poll::Pending` after
   `cx.waker().wake_by_ref()` — the self-wake idiom, i.e. the same
   spin one abstraction level down.

## Fix

Three pieces, all landing in the same commit as this document.

### Waker registration on `CljxFuture` / `CljxPromise` (cljrs-value)

Both types grow a private `wakers: Mutex<Vec<std::task::Waker>>`.
`std::task::Waker` is std, so cljrs-value stays tokio-free.

- `register_waker(&self, waker)` parks an async waiter. The contract:
  **callers register while holding the state lock** (`CljxFuture::state`
  / `CljxPromise::value`), having just observed the unsettled state.
- `CljxFuture::notify_settled(&self)` wakes both waiter populations:
  `cond.notify_all()` for blocking `deref`, then drains and wakes the
  waker list. **Every site that writes a settled `FutureState` must
  call it** in place of a bare `cond.notify_all()`. There are four:
  `settle_future` (cljrs-async), the two work-stealing deref sites
  (cljrs-builtins `deref` builtin, cljrs-interp `@` form), and
  `future-cancel` (cljrs-builtins).
- `CljxPromise::deliver` drains its wakers after writing the value.

Why this is race-free: writers settle under the same lock the waiter
registers beneath, and drain wakers afterwards. So either the waiter's
locked re-check sees the settled state (no park needed), or its
registration completes before the writer reaches the drain (wake
guaranteed). No lost-wakeup window. Lock order is uniformly
state-then-wakers; nothing acquires them in the other order, so no
deadlock.

### `await_value` parks

The Future and Promise arms keep their outer check loop but replace
`yield_now` with:

    cljrs_env::gc_roots::async_gc_collect();   // safepoint at the suspension boundary
    std::future::poll_fn(|cx| {
        let guard = /* state or value lock */;
        if /* still unsettled */ {
            register_waker(cx.waker());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }).await;

One `async_gc_collect` per genuine suspension replaces one per spin
iteration — which is the model the wasm32 path already ran on
("safepoints at every real async suspension point are sufficient").
Spurious wakes just re-run the outer check.

`CompiledAsyncTask::poll` does the same on `POLL_PENDING`: the awaited
value is left in `sm.pending` by the readiness check, so it registers
`cx.waker()` on that future/promise under its lock. If `pending` is
neither (a suspend on a plain value), it keeps the old self-wake.

### GC service becomes a timer backstop

    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cljrs_env::gc_roots::async_gc_collect();
    }

The service was never the primary collection path — running tasks hit
safepoints inline (loop back-edges, await suspension points, and
`async_gc_collect` is a cheap flag-check when nothing is requested).
Its remaining job is servicing a request raised just before everything
parked. While everything is parked nothing allocates, so the 100 ms
tick bounds only how long already-requested garbage waits, not heap
growth. The executor sleeps in the timer between ticks.

## Verification

Regression tests (`crates/cljrs-async/tests/await_parks.rs`), all
verified red against the pre-fix `eval_async.rs`/`runtime.rs`:

- `await_on_future_parks_until_settled` /
  `await_on_promise_parks_until_delivered`: a `CountPolls` wrapper
  counts polls of the awaiting task while the value settles 50 ms
  later. Spin implementation: tens of thousands of polls. Fixed: ≤ 10
  (observed: a handful).
- `idle_runtime_does_not_burn_cpu` (linux): full `init()` runtime — GC
  service included — awaiting a 300 ms-deferred future must burn
  < 100 ms of thread CPU (`/proc/thread-self/stat` utime+stime).
  Spin implementation burns the whole interval.

End-to-end: the idle-park DUT went from 1495 CPU ticks per 15 s to 0.
Production mised restarted on the fixed binary: 0% CPU at idle,
`ping`/`env`/`stats` all served correctly, cache warming normally
(~10 ms CPU over 16 s *including* three requests and a `mise env`
subprocess).

Full test suites for cljrs-value, cljrs-async, cljrs-builtins,
cljrs-interp, cljrs-gc, cljrs-eval, and cljrs-net pass; clippy clean.

## Deployment note

`~/.local/bin/cljrs` and `~/.local/bin/cljrsh` are symlinks into
`target/release`, so a release build is the whole install — but
processes started before the build keep executing the old image until
restarted. If a cljrs daemon is pinning a core, check its start time
against the binary's mtime before assuming a new bug.

## Residual spin loops (converted 2026-08-30)

The `yield_now` loops that survived the original fix spun only while
their specific operation was outstanding — never in the steady state
of an idle daemon, which is why they didn't gate it. All are now
converted onto the existing waker paths (Phase C1 of
`docs/user-reachable-isolates-plan.md`):

- `builtins.rs`: `mult`'s forwarding task, `onto-chan!`/`to-chan!`'s
  put loops, and `thread-call`'s result put now use `CljChannel`'s
  async `take`/`put` (which park on `async_not_empty`/
  `async_not_full`).
- `isolate_builtins.rs`: `isolate-take!` parks on the underlying mpsc
  waker via `IsolateReceiver::poll_recv`, locking the shared receiver
  only inside each poll so a concurrent `isolate-poll!` can never
  block the LocalSet thread against a parked taker.

Guarded by `tests/channel_parks.rs`, which stalls each path for
~300 ms and requires the executor thread to sleep through the stall
(the spin implementations burn the whole interval and fail the
CPU-tick assertion).

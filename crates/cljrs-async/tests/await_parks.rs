//! `await` must park, not spin.
//!
//! `await_value` used to poll a pending future/promise in a
//! `yield_now` loop, and the GC service task was a permanently-runnable
//! `yield_now` loop of its own.  Either one keeps the `LocalSet` executor
//! from ever sleeping, so an idle daemon parked on `(await (a/timeout ...))`
//! pinned a full core (observed: mised.cljrs at 99.7% CPU while serving no
//! requests).
//!
//! These tests drive `await_value` against a future/promise that settles
//! ~50 ms later and count how many times the awaiting task is polled.  The
//! spin implementation is polled tens of thousands of times; the
//! waker-registered wait is polled once per genuine wake.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use cljrs_gc::GcPtr;
use cljrs_value::{CljxFuture, CljxPromise, FutureState, Value};

fn block_on_local<F: Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future)
}

/// Wraps a future and counts how many times it is polled.
struct CountPolls<F> {
    inner: Pin<Box<F>>,
    polls: Rc<Cell<u64>>,
}

impl<F: Future> Future for CountPolls<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls.set(self.polls.get() + 1);
        self.inner.as_mut().poll(cx)
    }
}

// The awaiting task tolerates a handful of polls (first poll, the settle
// wake, plus scheduler noise); the old spin implementation shows tens of
// thousands over a 50 ms wait.
const MAX_POLLS: u64 = 10;

#[test]
fn await_on_future_parks_until_settled() {
    let _mutator = cljrs_gc::register_mutator();
    let polls = Rc::new(Cell::new(0u64));

    let future = GcPtr::new(CljxFuture::new());
    let val = Value::Future(future.clone());

    let task_polls = polls.clone();
    let result = block_on_local(async move {
        let settler = future.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            *settler.get().state.lock().unwrap() = FutureState::Done(Value::Long(42));
            settler.get().notify_settled();
        });
        let waiter = tokio::task::spawn_local(CountPolls {
            inner: Box::pin(cljrs_async::eval_async::await_value(val)),
            polls: task_polls.clone(),
        });
        waiter.await.expect("waiter task")
    })
    .expect("await succeeds");

    assert_eq!(result, Value::Long(42));
    assert!(
        polls.get() <= MAX_POLLS,
        "awaiting a future polled the task {} times over a 50 ms wait — \
         await_value is spinning instead of parking",
        polls.get()
    );
}

#[test]
fn await_on_promise_parks_until_delivered() {
    let _mutator = cljrs_gc::register_mutator();
    let polls = Rc::new(Cell::new(0u64));

    let promise = GcPtr::new(CljxPromise::new());
    let val = Value::Promise(promise.clone());

    let task_polls = polls.clone();
    let result = block_on_local(async move {
        let deliverer = promise.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            deliverer.get().deliver(Value::Long(7));
        });
        let waiter = tokio::task::spawn_local(CountPolls {
            inner: Box::pin(cljrs_async::eval_async::await_value(val)),
            polls: task_polls.clone(),
        });
        waiter.await.expect("waiter task")
    })
    .expect("await succeeds");

    assert_eq!(result, Value::Long(7));
    assert!(
        polls.get() <= MAX_POLLS,
        "awaiting a promise polled the task {} times over a 50 ms wait — \
         await_value is spinning instead of parking",
        polls.get()
    );
}

/// End-to-end idle-CPU guard: a full `cljrs-async` runtime (GC service
/// included, via `init()`) parked on an `(a/timeout ...)`-shaped wait must
/// leave the executor asleep.  Measures this thread's CPU time across a
/// 300 ms idle await — the pre-fix runtime burns the whole interval
/// (yield_now spin in both the GC service and `await_value`); the fixed one
/// sleeps in the reactor.
#[cfg(target_os = "linux")]
#[test]
fn idle_runtime_does_not_burn_cpu() {
    /// This thread's consumed CPU (utime + stime) in USER_HZ ticks (10 ms).
    fn thread_cpu_ticks() -> u64 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").expect("read thread stat");
        // Parse after the last ')' — comm may contain spaces. The tokens
        // that follow start at field 3 (state); utime/stime are fields
        // 14/15, i.e. indices 11/12 here.
        let rest = stat.rsplit_once(')').expect("comm delimiter").1;
        let mut fields = rest.split_whitespace().skip(11);
        let utime: u64 = fields.next().expect("utime").parse().expect("utime");
        let stime: u64 = fields.next().expect("stime").parse().expect("stime");
        utime + stime
    }

    let _mutator = cljrs_gc::register_mutator();
    let globals = cljrs_interp::standard_env_with_paths(None, None, None, Vec::new());

    let (result, cpu_ticks) = block_on_local(async move {
        // Registers the async runtime and spawns the GC service task on
        // this LocalSet — the full idle profile of a cljrs daemon.
        cljrs_async::init(&globals);
        let timeout_fut = {
            let f = GcPtr::new(CljxFuture::new());
            let settler = f.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                *settler.get().state.lock().unwrap() = FutureState::Done(Value::Nil);
                settler.get().notify_settled();
            });
            Value::Future(f)
        };
        let ticks_before = thread_cpu_ticks();
        let result = cljrs_async::eval_async::await_value(timeout_fut).await;
        (result, thread_cpu_ticks() - ticks_before)
    });

    result.expect("await succeeds");
    // 10 ticks = 100 ms of a 300 ms idle wait; the spin implementation
    // burns the whole interval (~30 ticks).
    assert!(
        cpu_ticks < 10,
        "idle 300 ms await burned {} ms of CPU — the executor is spinning \
         instead of sleeping",
        cpu_ticks * 10
    );
}

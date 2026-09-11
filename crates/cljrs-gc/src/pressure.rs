//! Process-wide memory-pressure coordinator (user-reachable-isolates plan, C5).
//!
//! Every isolate heap collects on its own, so no single heap can tell that
//! the *process* is close to its memory budget.  This module sums the live
//! bytes of every heap into one process-global counter, derives a graduated
//! [`PressureLevel`] from that sum against a process budget, and lets the
//! rest of the runtime react:
//!
//! - `GcHeap::alloc` lowers its own collection threshold under Yellow and
//!   Red (see [`effective_soft_limit`]), so every isolate collects more
//!   eagerly while the process is under pressure.
//! - `cljrs-async` mirrors the level into a `tokio::sync::watch` so
//!   listeners can park until pressure drops; the `cljrs-net` accept loops
//!   use that to stop taking connections under Red, which lets the kernel
//!   backlog push back on peers instead of the process growing into an OOM.
//!
//! Heaps publish their live bytes in chunks of [`PUBLISH_CHUNK`], so the hot
//! allocation path pays one process-global atomic per chunk rather than per
//! object, and isolates do not bounce a shared cache line on every `conj`.
//!
//! The budget comes from `CLJRS_GC_PROCESS_LIMIT_MB`; unset, it is the same
//! RAM-derived default a single heap's hard limit uses.  A budget of zero
//! disables the coordinator (the level stays Green).

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Graduated memory-pressure level for the whole process.
///
/// Ordered: `Green < Yellow < Red`, so `level < PressureLevel::Red` reads as
/// "not yet shedding load".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum PressureLevel {
    /// Live bytes are comfortably below the budget.
    #[default]
    Green = 0,
    /// Live bytes are near the budget: heaps collect more eagerly.
    Yellow = 1,
    /// Live bytes are at the budget: heaps collect aggressively and load
    /// is shed (accept loops stop taking connections).
    Red = 2,
}

impl PressureLevel {
    /// Lower-case name, matching the Clojure-level keyword (`:green`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
        }
    }

    /// Parse the lower-case name; `None` for anything else.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "green" => Some(Self::Green),
            "yellow" => Some(Self::Yellow),
            "red" => Some(Self::Red),
            _ => None,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Green,
            1 => Self::Yellow,
            _ => Self::Red,
        }
    }
}

impl std::fmt::Display for PressureLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Environment variable naming the process memory budget in megabytes.
pub const CLJRS_GC_PROCESS_LIMIT_ENV: &str = "CLJRS_GC_PROCESS_LIMIT_MB";

/// Bytes a heap's live count may drift from its last published value before
/// it publishes again.
///
/// 1 MiB keeps the process counter within a few megabytes of the truth per
/// isolate while costing one shared atomic per megabyte allocated.  Making
/// it larger hides more of a small isolate's growth; making it much smaller
/// reintroduces the cross-core cache-line traffic the isolate model exists
/// to avoid.
pub const PUBLISH_CHUNK: usize = 1 << 20;

/// Percent of the budget at which the level rises from Green to Yellow.
///
/// Matches the 75 % soft-to-hard ratio a single heap already uses, so a
/// one-isolate process reaches Yellow at the same point its own soft limit
/// would have fired.
const YELLOW_ENTER_PCT: u128 = 75;
/// Percent of the budget below which Yellow falls back to Green.  5 points
/// of hysteresis under [`YELLOW_ENTER_PCT`] so a heap oscillating around
/// the threshold does not flap the level (and wake every listener) on each
/// collection.
const YELLOW_LEAVE_PCT: u128 = 70;
/// Percent of the budget at which the level rises to Red and load is shed.
/// Leaves 10 % of the budget for the in-flight work that is already accepted
/// to finish and be collected.
const RED_ENTER_PCT: u128 = 90;
/// Percent of the budget below which Red falls back to Yellow (same 5-point
/// hysteresis as Yellow).
const RED_LEAVE_PCT: u128 = 85;

/// Sum of every heap's published live bytes.
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of [`LIVE_BYTES`] since process start.
static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Current [`PressureLevel`] as its `u8` discriminant.
static LEVEL: AtomicU8 = AtomicU8::new(PressureLevel::Green as u8);
/// Process budget in bytes, initialized lazily from the environment.
static BUDGET: OnceLock<AtomicUsize> = OnceLock::new();

type Listener = Box<dyn Fn(PressureLevel) + Send + Sync>;
/// Callbacks invoked on every level transition, in registration order.
static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

/// The process budget from `CLJRS_GC_PROCESS_LIMIT_MB`, or the RAM-derived
/// default hard limit when unset.  Malformed input warns and uses the
/// default rather than aborting on a misconfiguration.
pub fn budget_from_env() -> usize {
    let default = default_budget();
    match std::env::var(CLJRS_GC_PROCESS_LIMIT_ENV) {
        Ok(s) => match s.trim().parse::<usize>() {
            Ok(mb) => mb.saturating_mul(1024 * 1024),
            Err(_) => {
                eprintln!(
                    "[gc] warning: ignoring invalid {CLJRS_GC_PROCESS_LIMIT_ENV}={s:?} \
                     (expected a number of megabytes)"
                );
                default
            }
        },
        Err(_) => default,
    }
}

#[cfg(not(feature = "no-gc"))]
fn default_budget() -> usize {
    crate::config::default_hard_limit()
}

/// Without a collector there is nothing to coordinate; the budget only
/// matters for embedders that set one explicitly.
#[cfg(feature = "no-gc")]
fn default_budget() -> usize {
    0
}

fn budget_cell() -> &'static AtomicUsize {
    BUDGET.get_or_init(|| AtomicUsize::new(budget_from_env()))
}

/// The process memory budget in bytes; zero means the coordinator is off.
pub fn budget() -> usize {
    budget_cell().load(Ordering::Relaxed)
}

/// Override the process budget (CLI flags, embedders, tests) and re-derive
/// the level against the current live bytes.  Zero disables the coordinator.
pub fn set_budget(bytes: usize) {
    budget_cell().store(bytes, Ordering::Relaxed);
    recompute(LIVE_BYTES.load(Ordering::Relaxed));
}

/// The current process-wide pressure level.  A plain atomic load: cheap
/// enough for the allocation path.
#[inline]
pub fn level() -> PressureLevel {
    PressureLevel::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// Live bytes published by every heap so far (accurate to one
/// [`PUBLISH_CHUNK`] per heap).
pub fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// High-water mark of [`live_bytes`] since process start.
pub fn peak_live_bytes() -> usize {
    PEAK_LIVE_BYTES.load(Ordering::Relaxed)
}

/// Register a callback for level transitions.  Called with the new level,
/// on the thread whose publish caused the transition, while no lock other
/// than the listener list is held.  Listeners must not allocate on the GC
/// heap or block.  Read [`level`] after registering to learn the current
/// value; the callback fires only on changes.
pub fn on_change(f: impl Fn(PressureLevel) + Send + Sync + 'static) {
    LISTENERS
        .lock()
        .expect("pressure listener list poisoned")
        .push(Box::new(f));
}

/// A heap's soft limit scaled by the current pressure level.
///
/// Yellow halves it and Red quarters it, so each isolate collects earlier
/// while the process is under pressure.  The zero-yield suppression in
/// `GcHeap::collect` still applies, so an all-live heap does not spin in a
/// collection storm just because another isolate is large.
#[inline]
pub fn effective_soft_limit(soft_limit: usize) -> usize {
    match level() {
        PressureLevel::Green => soft_limit,
        PressureLevel::Yellow => soft_limit / 2,
        PressureLevel::Red => soft_limit / 4,
    }
}

/// Add `bytes` to the process live count and re-derive the level.  Heaps
/// call this through their chunked publish; embedders with their own
/// allocators may call it directly.
pub fn add_live(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let now = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_LIVE_BYTES.fetch_max(now, Ordering::Relaxed);
    recompute(now);
}

/// Subtract `bytes` from the process live count (saturating, so a heap
/// that under-reported can never drive the counter negative) and re-derive
/// the level.
pub fn sub_live(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let mut cur = LIVE_BYTES.load(Ordering::Relaxed);
    loop {
        let next = cur.saturating_sub(bytes);
        match LIVE_BYTES.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                recompute(next);
                return;
            }
            Err(actual) => cur = actual,
        }
    }
}

/// The level `live` bytes imply given the level we are coming from.  Pure,
/// so the hysteresis is unit-testable without touching the globals.
fn next_level(current: PressureLevel, live: usize, budget: usize) -> PressureLevel {
    if budget == 0 {
        return PressureLevel::Green;
    }
    // u128 so a wasm32 budget of usize::MAX cannot overflow the product.
    let pct = |p: u128| ((budget as u128 * p) / 100) as usize;
    match current {
        PressureLevel::Green => {
            if live >= pct(RED_ENTER_PCT) {
                PressureLevel::Red
            } else if live >= pct(YELLOW_ENTER_PCT) {
                PressureLevel::Yellow
            } else {
                PressureLevel::Green
            }
        }
        PressureLevel::Yellow => {
            if live >= pct(RED_ENTER_PCT) {
                PressureLevel::Red
            } else if live < pct(YELLOW_LEAVE_PCT) {
                PressureLevel::Green
            } else {
                PressureLevel::Yellow
            }
        }
        PressureLevel::Red => {
            if live < pct(YELLOW_LEAVE_PCT) {
                PressureLevel::Green
            } else if live < pct(RED_LEAVE_PCT) {
                PressureLevel::Yellow
            } else {
                PressureLevel::Red
            }
        }
    }
}

fn recompute(live: usize) {
    let current = level();
    let next = next_level(current, live, budget());
    if next == current {
        return;
    }
    // Two heaps publishing at once may each compute a transition; the CAS
    // lets exactly one of them announce it.  The loser's view is stale and
    // the next publish re-derives from the counter anyway.
    if LEVEL
        .compare_exchange(
            current as u8,
            next as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    crate::stats::GC_STATS.record_pressure_transition();
    cljrs_logging::feat_debug!(
        "gc",
        "memory pressure {} -> {} ({} live bytes of {} budget)",
        current,
        next,
        live,
        budget()
    );
    let listeners = LISTENERS.lock().expect("pressure listener list poisoned");
    for listener in listeners.iter() {
        listener(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: usize = 1000;

    #[test]
    fn levels_are_ordered_and_named() {
        assert!(PressureLevel::Green < PressureLevel::Yellow);
        assert!(PressureLevel::Yellow < PressureLevel::Red);
        for l in [
            PressureLevel::Green,
            PressureLevel::Yellow,
            PressureLevel::Red,
        ] {
            assert_eq!(PressureLevel::parse(l.as_str()), Some(l));
            assert_eq!(l.to_string(), l.as_str());
        }
        assert_eq!(PressureLevel::parse("orange"), None);
    }

    #[test]
    fn zero_budget_is_always_green() {
        for from in [
            PressureLevel::Green,
            PressureLevel::Yellow,
            PressureLevel::Red,
        ] {
            assert_eq!(next_level(from, usize::MAX, 0), PressureLevel::Green);
        }
    }

    #[test]
    fn rising_thresholds() {
        use PressureLevel::*;
        assert_eq!(next_level(Green, 749, B), Green);
        assert_eq!(next_level(Green, 750, B), Yellow);
        assert_eq!(next_level(Green, 899, B), Yellow);
        assert_eq!(next_level(Green, 900, B), Red);
        assert_eq!(next_level(Yellow, 900, B), Red);
    }

    #[test]
    fn falling_thresholds_have_hysteresis() {
        use PressureLevel::*;
        // Just under the entry point is not enough to leave a level.
        assert_eq!(next_level(Yellow, 749, B), Yellow);
        assert_eq!(next_level(Yellow, 700, B), Yellow);
        assert_eq!(next_level(Yellow, 699, B), Green);
        assert_eq!(next_level(Red, 899, B), Red);
        assert_eq!(next_level(Red, 850, B), Red);
        assert_eq!(next_level(Red, 849, B), Yellow);
        // A large drop skips Yellow entirely.
        assert_eq!(next_level(Red, 699, B), Green);
    }

    #[test]
    fn budget_does_not_overflow_at_usize_max() {
        assert_eq!(
            next_level(PressureLevel::Green, usize::MAX, usize::MAX),
            PressureLevel::Red
        );
        assert_eq!(
            next_level(PressureLevel::Green, 0, usize::MAX),
            PressureLevel::Green
        );
    }

    /// The one test that drives the process globals.  It uses a budget far
    /// above anything the rest of the test binary allocates, so concurrent
    /// heap tests (which publish only in 1 MiB chunks or on collect) cannot
    /// move it across a threshold.
    #[test]
    fn transitions_notify_listeners() {
        use std::sync::Arc;
        const BUDGET: usize = 1 << 40;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        on_change(move |l| seen2.lock().unwrap().push(l));
        let transitions_before = crate::stats::GC_STATS.snapshot().pressure_transitions;

        set_budget(BUDGET);
        assert_eq!(level(), PressureLevel::Green);
        add_live(BUDGET / 100 * 80);
        assert_eq!(level(), PressureLevel::Yellow);
        assert!(peak_live_bytes() >= BUDGET / 100 * 80);
        add_live(BUDGET / 100 * 15);
        assert_eq!(level(), PressureLevel::Red);
        assert_eq!(effective_soft_limit(400), 100);
        sub_live(BUDGET / 100 * 4);
        assert_eq!(
            level(),
            PressureLevel::Red,
            "91% is above the 85% exit, so Red holds"
        );
        sub_live(BUDGET / 100 * 8);
        assert_eq!(
            level(),
            PressureLevel::Yellow,
            "83% is below the 85% exit but above the 70% Yellow exit"
        );
        sub_live(BUDGET);
        assert_eq!(level(), PressureLevel::Green);
        assert_eq!(live_bytes(), 0, "sub_live saturates at zero");

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[
                PressureLevel::Yellow,
                PressureLevel::Red,
                PressureLevel::Yellow,
                PressureLevel::Green
            ]
        );
        assert_eq!(
            crate::stats::GC_STATS.snapshot().pressure_transitions - transitions_before,
            4
        );
        // Leave the coordinator off for the remaining tests in this binary.
        set_budget(0);
    }
}

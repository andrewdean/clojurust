//! Opt-in GC debugging modes, read once from the environment.
//!
//! Both modes exist to make use-after-free bugs deterministic instead of
//! timing-dependent (see docs/gc-inflight-rooting-bug.md for the class they
//! hunt).  Both default to off and cost nothing when off.
//!
//! - `CLJRS_GC_QUARANTINE=1`: the sweep drops each dead value in place but
//!   never returns the `GcBox` allocation to the allocator, so the
//!   `GC_MAGIC_FREED` poison in the header survives for the life of the
//!   process and any later `GcPtr` access panics with an exact message
//!   instead of racing allocator reuse into a wild read.  Freed-object
//!   memory is never reclaimed while enabled; debugging only.  Detection
//!   needs the debug-assertions magic field, so use a debug build.
//! - `CLJRS_GC_STRESS=1` (or `=N`): request a collection at every safepoint
//!   (every Nth for N > 1), shrinking the window between a value becoming
//!   unrooted and the collection that would free it.  Combine with
//!   quarantine to turn latent rooting holes into immediate panics.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Quarantine switch, initialized from `CLJRS_GC_QUARANTINE` on first use.
static QUARANTINE: OnceLock<AtomicBool> = OnceLock::new();

fn quarantine_cell() -> &'static AtomicBool {
    QUARANTINE.get_or_init(|| {
        let on = match std::env::var("CLJRS_GC_QUARANTINE") {
            Ok(v) => !matches!(v.trim(), "" | "0" | "off" | "false" | "no"),
            Err(_) => false,
        };
        AtomicBool::new(on)
    })
}

/// Whether sweep quarantines dead objects instead of freeing them.
pub fn quarantine_enabled() -> bool {
    quarantine_cell().load(Ordering::Relaxed)
}

/// Override the quarantine switch (tests, embedders).
pub fn set_quarantine(on: bool) {
    quarantine_cell().store(on, Ordering::Relaxed);
}

/// Stress period, initialized from `CLJRS_GC_STRESS` on first use.
/// 0 = off; N >= 1 = request a collection at every Nth safepoint.
static STRESS_PERIOD: OnceLock<AtomicUsize> = OnceLock::new();

fn stress_cell() -> &'static AtomicUsize {
    STRESS_PERIOD.get_or_init(|| {
        let period = match std::env::var("CLJRS_GC_STRESS") {
            Ok(v) => match v.trim() {
                "" | "0" | "off" | "false" | "no" => 0,
                "on" | "true" | "yes" => 1,
                n => n.parse::<usize>().unwrap_or(1),
            },
            Err(_) => 0,
        };
        AtomicUsize::new(period)
    })
}

/// Override the stress period (tests, embedders).  0 disables.
pub fn set_stress_period(period: usize) {
    stress_cell().store(period, Ordering::Relaxed);
}

thread_local! {
    /// Safepoints seen on this thread since the last stress-forced request.
    static STRESS_TICK: Cell<usize> = const { Cell::new(0) };
}

/// Called at each safepoint: true when stress mode wants a collection now.
/// Off (period 0) short-circuits to false without touching the counter.
pub fn stress_due() -> bool {
    let period = stress_cell().load(Ordering::Relaxed);
    if period == 0 {
        return false;
    }
    STRESS_TICK.with(|c| {
        let n = c.get() + 1;
        if n >= period {
            c.set(0);
            true
        } else {
            c.set(n);
            false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stress_due_honors_period() {
        set_stress_period(3);
        // Fresh thread-local counter: due on every 3rd call.
        let hits: Vec<bool> = (0..6).map(|_| stress_due()).collect();
        assert_eq!(hits, [false, false, true, false, false, true]);
        set_stress_period(0);
        assert!(!stress_due());
    }
}

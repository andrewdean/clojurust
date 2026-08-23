//! Process-wide evaluation interrupt (SIGINT → stop the running form).
//!
//! The hosting binary's signal handler calls `request` (async-signal-safe:
//! one atomic store); every evaluation tier checks `pending` at its gas
//! checkpoint and unwinds with `EvalError::Interrupted` — a control signal
//! like `System/exit`: `finally` runs, `catch` never sees it.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Flag the current evaluation for interruption (signal-handler safe).
pub fn request() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn pending() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Consume the flag: true exactly once per request. Checkpoints use this
/// so the unwind fires a single Interrupted — the finally blocks that run
/// during unwinding must not re-trip on the same request.
pub fn take() -> bool {
    INTERRUPTED.load(Ordering::Relaxed) && INTERRUPTED.swap(false, Ordering::SeqCst)
}

/// Clear the flag (the REPL, before handing out a fresh prompt).
pub fn clear() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

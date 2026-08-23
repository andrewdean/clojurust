//! Clojure-level call-stack tracking for error reports.
//!
//! `eval_call` pushes one [`Frame`] per interpreted call (RAII-popped); when
//! a call returns an error and no snapshot exists yet, the live stack is
//! captured into a thread-local **error trace**. A `try` that *catches* the
//! condition clears it, so the trace always describes the error that finally
//! escaped. Hosting binaries render it via [`take_error_trace`].
//!
//! IR/JIT-executed frames don't appear here (they degrade to gaps); the
//! frontier of a script error is virtually always interpreted, which is the
//! cost/benefit the cljrsh plan accepted.

use std::cell::RefCell;
use std::sync::Arc;

use cljrs_types::span::Span;

/// One interpreted call: the callee as written, the namespace evaluating it,
/// and the call site.
#[derive(Debug, Clone)]
pub struct Frame {
    pub name: String,
    pub ns: Arc<str>,
    pub span: Span,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    static ERROR_TRACE: RefCell<Option<Vec<Frame>>> = const { RefCell::new(None) };
}

/// Pops the frame on drop.
pub struct FrameGuard;

impl FrameGuard {
    pub fn push(name: String, ns: Arc<str>, span: Span) -> Self {
        STACK.with(|s| s.borrow_mut().push(Frame { name, ns, span }));
        FrameGuard
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// Snapshot the live stack as the error trace, unless one is already recorded
/// (the deepest failing call wins as errors propagate outward).
pub fn record_error_trace() {
    ERROR_TRACE.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            *t = Some(STACK.with(|s| s.borrow().clone()));
        }
    });
}

/// A `catch` handled the condition: the recorded trace no longer describes an
/// escaping error.
pub fn clear_error_trace() {
    ERROR_TRACE.with(|t| *t.borrow_mut() = None);
}

/// Take the trace of the error that escaped (innermost frame last).
pub fn take_error_trace() -> Option<Vec<Frame>> {
    ERROR_TRACE.with(|t| t.borrow_mut().take())
}

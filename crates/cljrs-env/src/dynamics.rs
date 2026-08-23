//! Thread-local dynamic variable binding stack.
//!
//! `binding` forms push a frame onto `BINDING_STACK` for the duration of their
//! body; the RAII `BindingGuard` pops it on drop (handles both normal return
//! and panics).

use std::cell::RefCell;
use std::collections::HashMap;

use std::sync::atomic::{AtomicUsize, Ordering};

use cljrs_gc::GcPtr;
use cljrs_gc::Trace as _;
use cljrs_value::{Value, Var};

/// Opaque key for a Var in the binding stack (pointer identity).
/// Stable because the GC is non-moving.
pub type VarKey = usize;

pub fn var_key_of(var: &GcPtr<Var>) -> VarKey {
    var.get() as *const Var as usize
}

thread_local! {
    static BINDING_STACK: RefCell<Vec<HashMap<VarKey, Value>>> =
        const { RefCell::new(Vec::new()) };
}

// ── RAII guard ────────────────────────────────────────────────────────────────

/// Pops the innermost binding frame when dropped (plus the output-stream
/// marker its frame pushed, when `*out*` was rebound to an IO sentinel).
pub struct BindingGuard {
    pushed_stream: bool,
}

impl Drop for BindingGuard {
    fn drop(&mut self) {
        if self.pushed_stream {
            crate::io_target::pop_stream();
        }
        pop_frame();
    }
}

// ── *out* stream routing ─────────────────────────────────────────────────────

/// The var key of `clojure.core/*out*`, registered when the var is interned
/// (0 = not yet defined). Lets `push_frame` route prints to stderr for the
/// extent of `(binding [*out* *err*] …)`.
static OUT_VAR_KEY: AtomicUsize = AtomicUsize::new(0);

pub fn register_out_var(key: VarKey) {
    OUT_VAR_KEY.store(key, Ordering::Relaxed);
}

/// `:cljrs.io/stderr` → Some(true), `:cljrs.io/stdout` → Some(false);
/// anything else leaves the print target unchanged.
fn sentinel_stream(val: &Value) -> Option<bool> {
    if let Value::Keyword(k) = val {
        let k = k.get();
        if k.namespace.as_deref() == Some("cljrs.io") {
            match &*k.name {
                "stderr" => return Some(true),
                "stdout" => return Some(false),
                _ => {}
            }
        }
    }
    None
}

// ── Stack manipulation ────────────────────────────────────────────────────────

/// Push a new dynamic binding frame; return a guard that pops it on drop.
/// A frame rebinding `*out*` to an IO sentinel also pushes the matching
/// output-stream marker for its extent.
pub fn push_frame(bindings: HashMap<VarKey, Value>) -> BindingGuard {
    let out_key = OUT_VAR_KEY.load(Ordering::Relaxed);
    let stream = if out_key == 0 {
        None
    } else {
        bindings.get(&out_key).and_then(sentinel_stream)
    };
    BINDING_STACK.with(|s| s.borrow_mut().push(bindings));
    if let Some(stderr) = stream {
        crate::io_target::push_stream(stderr);
    }
    BindingGuard {
        pushed_stream: stream.is_some(),
    }
}

fn pop_frame() {
    BINDING_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

// ── Lookup ────────────────────────────────────────────────────────────────────

/// Check the thread-local stack first (innermost frame wins); fall back to the
/// root binding stored in the `Var` itself.
pub fn deref_var(var: &GcPtr<Var>) -> Option<Value> {
    let key = var_key_of(var);
    let tl = BINDING_STACK.with(|s| {
        s.borrow()
            .iter()
            .rev()
            .find_map(|frame| frame.get(&key).cloned())
    });
    tl.or_else(|| var.get().deref())
}

/// True if `var` has any thread-local binding on this thread.
pub fn is_thread_bound(var: &GcPtr<Var>) -> bool {
    let key = var_key_of(var);
    BINDING_STACK.with(|s| s.borrow().iter().any(|frame| frame.contains_key(&key)))
}

/// Set the innermost thread-local binding for `var`.
/// Returns `false` if no thread-local binding exists (caller should fall back
/// to setting the root).
pub fn set_thread_local(var: &GcPtr<Var>, val: Value) -> bool {
    let key = var_key_of(var);
    BINDING_STACK.with(|s| {
        for frame in s.borrow_mut().iter_mut().rev() {
            if let std::collections::hash_map::Entry::Occupied(mut e) = frame.entry(key) {
                e.insert(val);
                return true;
            }
        }
        false
    })
}

// ── Binding conveyance ────────────────────────────────────────────────────────

/// Snapshot the current thread's entire binding stack (for conveyance into a
/// child thread, e.g. `future`).
pub fn capture_current() -> Vec<HashMap<VarKey, Value>> {
    BINDING_STACK.with(|s| s.borrow().clone())
}

/// Install a previously captured snapshot on the current (new) thread.
pub fn install_frames(frames: Vec<HashMap<VarKey, Value>>) {
    BINDING_STACK.with(|s| *s.borrow_mut() = frames);
}

// ── GC root tracing ───────────────────────────────────────────────────────────

/// Trace all values in the current thread's binding stack as GC roots.
/// Call this during the GC root enumeration phase.
pub fn trace_current(visitor: &mut cljrs_gc::MarkVisitor) {
    BINDING_STACK.with(|s| {
        for frame in s.borrow().iter() {
            for val in frame.values() {
                val.trace(visitor);
            }
        }
    });
}

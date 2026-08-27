#[cfg(not(feature = "no-gc"))]
use crate::dynamics;
use crate::env::Env;
#[cfg(not(feature = "no-gc"))]
use crate::env::GlobalEnv;
use std::cell::RefCell;

// ── Stop-the-world reclaim hooks (JIT code unloading, cold-IR sweep) ────────
//
// Reclamation of execution-engine caches runs only at a stop-the-world
// safepoint, when every mutator thread is parked and active JIT frames can be
// scanned safely.  GC collection is the existing STW point, so interested
// tiers install hooks here that run at the tail of every collection while the
// STW guard is still held.  Current registrants: `cljrs-jit` (superseded
// native modules) and `cljrs-eval`'s lowering worker (idle Tier-1 IR,
// Phase 10.7).

type StwReclaimHook = Box<dyn Fn() + Send + Sync + 'static>;
static STW_RECLAIM_HOOKS: std::sync::RwLock<Vec<StwReclaimHook>> =
    std::sync::RwLock::new(Vec::new());

/// Register a stop-the-world reclaim hook.  Multiple hooks may be registered;
/// each runs at every STW point, in registration order.
///
/// Hooks run inside the STW guard after each collection, so they may assume
/// all other mutator threads are parked.
pub fn set_stw_reclaim_hook(f: impl Fn() + Send + Sync + 'static) {
    STW_RECLAIM_HOOKS.write().unwrap().push(Box::new(f));
}

/// Run the STW reclaim hooks, if any.  Caller must hold the STW guard.
#[cfg(not(feature = "no-gc"))]
fn run_stw_reclaim() {
    for hook in STW_RECLAIM_HOOKS.read().unwrap().iter() {
        hook();
    }
}

// ── Thread-local Env root registry ──────────────────────────────────────────
//
// When the interpreter enters a function call, the caller's Env stays on the
// Rust stack but the callee creates a fresh Env.  If GC triggers inside the
// callee, only the callee's Env is passed to `gc_safepoint`.  To keep the
// caller's local bindings alive we maintain a thread-local stack of pointers
// to all active Envs on this thread's call stack.
//
// SAFETY: the raw pointers are valid during STW collection because:
// - The collecting thread's own Envs are in earlier (still-live) stack frames.
// - Other threads are parked at safepoints; their stacks (and Envs) are frozen.

thread_local! {
    static ENV_ROOTS: RefCell<Vec<*const Env>> = const { RefCell::new(Vec::new()) };
    /// Shadow stack of Value pointers on the Rust call stack that need to
    /// survive GC.  Each entry is a `(ptr, count)` pair pointing to a
    /// contiguous slice of Values (e.g., a Vec's backing storage or a single
    /// Value on the stack).
    static VALUE_ROOTS: RefCell<Vec<(*const cljrs_value::Value, usize)>> =
        const { RefCell::new(Vec::new()) };
    /// Shadow stack for `Option<Value>` slices (e.g., the IR interpreter's
    /// register file).  Each entry is `(ptr, count)` pointing to a fixed-size
    /// heap slice whose address will not change for the lifetime of the entry.
    static OPTION_VALUE_ROOTS: RefCell<Vec<(*const Option<cljrs_value::Value>, usize)>> =
        const { RefCell::new(Vec::new()) };
}

/// RAII guard that pops the Env pointer on drop.
pub struct EnvRootGuard;

impl Drop for EnvRootGuard {
    fn drop(&mut self) {
        ENV_ROOTS.with(|roots| {
            roots.borrow_mut().pop();
        });
    }
}

/// RAII guard that removes its own entry from the value shadow stack on drop.
///
/// Removal is by identity, not LIFO pop: guards stored in structs (ValueIter's
/// cursor root) drop in arbitrary order relative to scoped guards, and a blind
/// pop under out-of-order drops unregisters a still-live root while leaving
/// the dead entry behind — a dangling slot the tracer then reads as a Value.
/// Entry order carries no meaning for tracing, so swap_remove is sound.
pub struct ValueRootGuard<'a> {
    entry: Option<(*const cljrs_value::Value, usize)>,
    /// Ties the guard to the registered slice: the borrow checker rejects
    /// any caller that moves, reallocates, or drops the slice while the
    /// guard lives — the exact shapes that left dangling entries for the
    /// tracer (2026-08-27 optimizer-suite SIGSEGV #2).
    _slice: std::marker::PhantomData<&'a [cljrs_value::Value]>,
}

impl Drop for ValueRootGuard<'_> {
    fn drop(&mut self) {
        if let Some(entry) = self.entry {
            VALUE_ROOTS.with(|roots| {
                let mut roots = roots.borrow_mut();
                match roots.iter().rposition(|e| *e == entry) {
                    Some(i) => {
                        roots.swap_remove(i);
                    }
                    None => debug_assert!(false, "value root entry missing at unregister"),
                }
            });
        }
    }
}

/// Register an Env as a GC root for the duration of its use.
/// Returns a guard that unregisters on drop.
pub fn push_env_root(env: &Env) -> EnvRootGuard {
    ENV_ROOTS.with(|roots| {
        roots.borrow_mut().push(env as *const Env);
    });
    EnvRootGuard
}

/// Register a single Value as a GC root.
pub fn root_value(val: &cljrs_value::Value) -> ValueRootGuard<'_> {
    let entry = (val as *const cljrs_value::Value, 1);
    VALUE_ROOTS.with(|roots| {
        roots.borrow_mut().push(entry);
    });
    ValueRootGuard {
        entry: Some(entry),
        _slice: std::marker::PhantomData,
    }
}

/// Register a slice of Values as GC roots (e.g., a Vec<Value>).
pub fn root_values(vals: &[cljrs_value::Value]) -> ValueRootGuard<'_> {
    if vals.is_empty() {
        return ValueRootGuard {
            entry: None,
            _slice: std::marker::PhantomData,
        };
    }
    let entry = (vals.as_ptr(), vals.len());
    VALUE_ROOTS.with(|roots| {
        roots.borrow_mut().push(entry);
    });
    ValueRootGuard {
        entry: Some(entry),
        _slice: std::marker::PhantomData,
    }
}

/// A rooted, heap-stable `Value` slot that OWNS its storage: the guard holds
/// the Box, so the registered entry cannot outlive the memory it points to.
/// For roots that must live inside a struct (ValueIter's cursor), where a
/// borrowing [`ValueRootGuard`] cannot express the self-reference.
pub struct BoxedValueRoot {
    slot: Box<cljrs_value::Value>,
}

impl BoxedValueRoot {
    pub fn new(val: cljrs_value::Value) -> Self {
        let slot = Box::new(val);
        VALUE_ROOTS.with(|roots| {
            roots
                .borrow_mut()
                .push((&*slot as *const cljrs_value::Value, 1));
        });
        Self { slot }
    }

    pub fn get(&self) -> &cljrs_value::Value {
        &self.slot
    }

    pub fn set(&mut self, val: cljrs_value::Value) {
        *self.slot = val;
    }

    pub fn get_mut(&mut self) -> &mut cljrs_value::Value {
        &mut self.slot
    }
}

impl Drop for BoxedValueRoot {
    fn drop(&mut self) {
        let entry = (&*self.slot as *const cljrs_value::Value, 1);
        VALUE_ROOTS.with(|roots| {
            let mut roots = roots.borrow_mut();
            match roots.iter().rposition(|e| *e == entry) {
                Some(i) => {
                    roots.swap_remove(i);
                }
                None => debug_assert!(false, "boxed value root missing at unregister"),
            }
        });
        // The Box frees after this, so the entry is gone before the memory.
    }
}

/// RAII guard that removes its own entry from the option-value shadow stack
/// on drop.  Identity removal, same rationale as [`ValueRootGuard`].
pub struct OptionValueRootGuard<'a> {
    entry: Option<(*const Option<cljrs_value::Value>, usize)>,
    _slice: std::marker::PhantomData<&'a [Option<cljrs_value::Value>]>,
}

impl Drop for OptionValueRootGuard<'_> {
    fn drop(&mut self) {
        if let Some(entry) = self.entry {
            OPTION_VALUE_ROOTS.with(|roots| {
                let mut roots = roots.borrow_mut();
                match roots.iter().rposition(|e| *e == entry) {
                    Some(i) => {
                        roots.swap_remove(i);
                    }
                    None => debug_assert!(false, "option-value root entry missing at unregister"),
                }
            });
        }
    }
}

/// A rooted, growable vector of Values that OWNS its storage.  `push`
/// re-registers the shadow-stack entry around any reallocation, so the
/// registered pointer always matches the live buffer — unlike rooting a
/// borrowed `&Vec` and then growing it, which dangles the entry the moment
/// the buffer reallocates (the sort-by keys bug, 2026-08-27).
pub struct RootedValueVec {
    vals: Vec<cljrs_value::Value>,
}

impl RootedValueVec {
    pub fn new(vals: Vec<cljrs_value::Value>) -> Self {
        if !vals.is_empty() {
            VALUE_ROOTS.with(|roots| {
                roots.borrow_mut().push((vals.as_ptr(), vals.len()));
            });
        }
        Self { vals }
    }

    fn unregister(&self) {
        if self.vals.is_empty() {
            return;
        }
        let entry = (self.vals.as_ptr(), self.vals.len());
        VALUE_ROOTS.with(|roots| {
            let mut roots = roots.borrow_mut();
            match roots.iter().rposition(|e| *e == entry) {
                Some(i) => {
                    roots.swap_remove(i);
                }
                None => debug_assert!(false, "rooted vec entry missing at unregister"),
            }
        });
    }

    pub fn push(&mut self, v: cljrs_value::Value) {
        self.unregister();
        self.vals.push(v);
        VALUE_ROOTS.with(|roots| {
            roots.borrow_mut().push((self.vals.as_ptr(), self.vals.len()));
        });
    }

    pub fn as_slice(&self) -> &[cljrs_value::Value] {
        &self.vals
    }

    /// Mutable access for in-place permutation (sorting).  Element writes
    /// are fine: the tracer reads whatever the slots currently hold.  Do
    /// NOT grow or shrink through this — use `push`.
    pub fn as_mut_slice(&mut self) -> &mut [cljrs_value::Value] {
        &mut self.vals
    }

    pub fn len(&self) -> usize {
        self.vals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }

    /// Unregister and hand the vector back (for a rooting handoff to a
    /// consumer that re-roots or immediately stores it).
    pub fn into_vec(self) -> Vec<cljrs_value::Value> {
        self.unregister();
        // Move the Vec out without running Drop (which would unregister twice).
        let this = std::mem::ManuallyDrop::new(self);
        unsafe { std::ptr::read(&this.vals) }
    }
}

impl Drop for RootedValueVec {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Register a slice of `Option<Value>` as GC roots.
///
/// The caller **must** ensure the slice's heap address is stable for the
/// lifetime of the returned guard — use `Box<[Option<Value>]>` rather than
/// a `Vec` that could reallocate.
pub fn root_option_values(vals: &[Option<cljrs_value::Value>]) -> OptionValueRootGuard<'_> {
    if vals.is_empty() {
        return OptionValueRootGuard {
            entry: None,
            _slice: std::marker::PhantomData,
        };
    }
    let entry = (vals.as_ptr(), vals.len());
    OPTION_VALUE_ROOTS.with(|roots| {
        roots.borrow_mut().push(entry);
    });
    OptionValueRootGuard {
        entry: Some(entry),
        _slice: std::marker::PhantomData,
    }
}

/// Register a FIXED-SIZE `Option<Value>` slice owned by the caller's own
/// struct (a self-rooting container).  The caller must pair this with
/// [`unregister_option_value_root`] on the same (unmoved) buffer in its
/// Drop, and must never grow, shrink, or reallocate the buffer while
/// registered.  Prefer [`root_option_values`] wherever a borrow works.
pub fn register_option_value_root(vals: &[Option<cljrs_value::Value>]) {
    if vals.is_empty() {
        return;
    }
    OPTION_VALUE_ROOTS.with(|roots| {
        roots.borrow_mut().push((vals.as_ptr(), vals.len()));
    });
}

/// Remove the entry registered by [`register_option_value_root`].
pub fn unregister_option_value_root(vals: &[Option<cljrs_value::Value>]) {
    if vals.is_empty() {
        return;
    }
    let entry = (vals.as_ptr(), vals.len());
    OPTION_VALUE_ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        match roots.iter().rposition(|e| *e == entry) {
            Some(i) => {
                roots.swap_remove(i);
            }
            None => debug_assert!(false, "option-value root missing at unregister"),
        }
    });
}

/// Same contract as [`register_option_value_root`] for a plain Value slice
/// owned by a self-rooting container (fixed buffer, paired unregister).
pub fn register_value_root_slice(vals: &[cljrs_value::Value]) {
    if vals.is_empty() {
        return;
    }
    VALUE_ROOTS.with(|roots| {
        roots.borrow_mut().push((vals.as_ptr(), vals.len()));
    });
}

/// Remove the entry registered by [`register_value_root_slice`].
pub fn unregister_value_root_slice(vals: &[cljrs_value::Value]) {
    if vals.is_empty() {
        return;
    }
    let entry = (vals.as_ptr(), vals.len());
    VALUE_ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        match roots.iter().rposition(|e| *e == entry) {
            Some(i) => {
                roots.swap_remove(i);
            }
            None => debug_assert!(false, "value root slice missing at unregister"),
        }
    });
}

/// Force an immediate GC collection, bypassing the memory-pressure threshold.
///
/// Unlike `gc_safepoint`, this always initiates collection regardless of
/// `gc_requested()`. Use this after removing namespaces from globals to ensure
/// their closures and form-trees are freed before the next namespace is loaded.
///
/// Under `no-gc` this is a no-op.
#[cfg(feature = "no-gc")]
pub fn force_collect(_env: &Env) {}

#[cfg(not(feature = "no-gc"))]
pub fn force_collect(env: &Env) {
    let Some(_stw_guard) = cljrs_gc::begin_stw() else {
        // Another thread is already collecting — just wait for it.
        cljrs_gc::safepoint();
        return;
    };

    cljrs_gc::HEAP.collect_with_stack_scan(|visitor| {
        cljrs_gc::HEAP.trace_registered_roots(visitor);
        trace_env_roots(env, visitor);
        trace_thread_env_roots(visitor);
        trace_value_roots(visitor);
        cljrs_value::trace_interop_methods(visitor);
        trace_option_value_roots(visitor);
        dynamics::trace_current(visitor);
        crate::taps::trace_roots(visitor);
        cljrs_gc::trace_thread_alloc_roots(visitor);
    });
    // Reclaim superseded JIT code while the world is still stopped.
    run_stw_reclaim();
}

/// Interpreter-level GC safepoint.
///
/// Under `no-gc` this is a no-op. Under GC mode it either parks (if collection
/// is in progress) or initiates a collection (if memory pressure was signalled).
#[cfg(feature = "no-gc")]
pub fn gc_safepoint(_env: &Env) {}

#[cfg(not(feature = "no-gc"))]
pub fn gc_safepoint(env: &Env) {
    // Stress mode (CLJRS_GC_STRESS): force a collection request at every
    // Nth safepoint so unrooted-value windows surface deterministically.
    if cljrs_gc::debug_modes::stress_due() {
        cljrs_gc::request_gc();
    }

    // Fast path: no GC activity at all.
    if !cljrs_gc::gc_requested() && !cljrs_gc::CONFIG_CANCELLATION.in_progress() {
        return;
    }

    // If a GC is already in progress (another thread is collecting), just park.
    if cljrs_gc::CONFIG_CANCELLATION.in_progress() {
        cljrs_gc::safepoint();
        return;
    }

    // A GC was requested (memory pressure). Try to become the collector.
    if !cljrs_gc::take_gc_request() {
        // Another thread took the request; if collection started, park.
        cljrs_gc::safepoint();
        return;
    }

    // We won the request. Initiate STW collection.
    let Some(_stw_guard) = cljrs_gc::begin_stw() else {
        // Race: another thread started collecting between our take and begin.
        cljrs_gc::safepoint();
        return;
    };

    // All other threads are now parked. Collect with registered roots
    // plus ALL of this thread's active environments and dynamic bindings.
    cljrs_gc::HEAP.collect_with_stack_scan(|visitor| {
        // Trace globally registered roots (GlobalEnv, etc.)
        cljrs_gc::HEAP.trace_registered_roots(visitor);
        // Trace the current (innermost) env
        trace_env_roots(env, visitor);
        // Trace all caller Envs registered on this thread's stack
        trace_thread_env_roots(visitor);
        // Trace values on the Rust call stack (shadow stack)
        trace_value_roots(visitor);
        cljrs_value::trace_interop_methods(visitor);
        // Trace Option<Value> slices (e.g. IR interpreter register files)
        trace_option_value_roots(visitor);
        // Trace dynamic variable bindings on this thread
        dynamics::trace_current(visitor);
        // Trace the global tap system (functions and queued values)
        crate::taps::trace_roots(visitor);
        // Trace in-flight allocations from this thread's alloc root frames
        cljrs_gc::trace_thread_alloc_roots(visitor);
    });
    // Reclaim superseded JIT code while the world is still stopped.
    run_stw_reclaim();
    // _stw_guard drop clears in_progress, waking parked threads.
}

// ── GC-only root tracing helpers ─────────────────────────────────────────────

/// Trace all GcPtr values reachable from an Env's local frames.
#[cfg(not(feature = "no-gc"))]
fn trace_env_roots(env: &Env, visitor: &mut cljrs_gc::MarkVisitor) {
    use cljrs_gc::Trace;
    // Trace local frame bindings
    for frame in &env.frames {
        for (_name, val) in &frame.bindings {
            val.trace(visitor);
        }
    }
    // Trace the globals (namespaces, vars) — these are also registered
    // as root tracers, but it's safe to trace twice (idempotent marking).
    trace_globals(&env.globals, visitor);
}

/// Trace all Values registered in the thread-local value shadow stack.
#[cfg(not(feature = "no-gc"))]
fn trace_value_roots(visitor: &mut cljrs_gc::MarkVisitor) {
    use cljrs_gc::Trace;
    VALUE_ROOTS.with(|roots| {
        for &(ptr, count) in roots.borrow().iter() {
            // SAFETY: pointers are valid — they point to Values on this thread's
            // still-live stack frames or heap-allocated Vecs whose owners are
            // on still-live stack frames.
            let slice = unsafe { std::slice::from_raw_parts(ptr, count) };
            for val in slice {
                val.trace(visitor);
            }
        }
    });
}

/// Trace all Option<Value> slices registered in the thread-local shadow stack.
///
/// Used for the IR interpreter's register file (a `Box<[Option<Value>]>`).
#[cfg(not(feature = "no-gc"))]
fn trace_option_value_roots(visitor: &mut cljrs_gc::MarkVisitor) {
    use cljrs_gc::Trace;
    OPTION_VALUE_ROOTS.with(|roots| {
        for &(ptr, count) in roots.borrow().iter() {
            // SAFETY: the slice is a Box<[Option<Value>]> owned by an active
            // stack frame; the address is stable for the guard's lifetime.
            let slice = unsafe { std::slice::from_raw_parts(ptr, count) };
            for val in slice.iter().flatten() {
                val.trace(visitor);
            }
        }
    });
}

/// Trace all Envs registered in the thread-local root stack.
#[cfg(not(feature = "no-gc"))]
fn trace_thread_env_roots(visitor: &mut cljrs_gc::MarkVisitor) {
    use cljrs_gc::Trace;
    ENV_ROOTS.with(|roots| {
        for env_ptr in roots.borrow().iter() {
            // SAFETY: pointers are valid — they point to Envs on this thread's
            // still-live stack frames (we are the collector, so our stack is active).
            let env = unsafe { &**env_ptr };
            for frame in &env.frames {
                for (_name, val) in &frame.bindings {
                    val.trace(visitor);
                }
            }
        }
    });
}

/// Trace all namespaces and their contents.
#[cfg(not(feature = "no-gc"))]
fn trace_globals(globals: &GlobalEnv, visitor: &mut cljrs_gc::MarkVisitor) {
    use cljrs_gc::{GcVisitor as _, Trace};
    let namespaces = globals.namespaces.read().unwrap();
    for ns_ptr in namespaces.values() {
        visitor.visit(ns_ptr);
    }
    drop(namespaces);
    // Values resolved at a pinned commit may live only in the version cache
    // (e.g. native HEAD fallbacks) — without this they would be collected.
    let version_cache = globals.version_cache.lock().unwrap();
    for val in version_cache.values() {
        val.trace(visitor);
    }
}

/// Service a pending GC request from an async (LocalSet) context.
///
/// Safe to call from within a Tokio `LocalSet` task at any cooperative yield
/// point: when this executes, no other tasks are polling, so thread-local root
/// stacks (ENV_ROOTS, VALUE_ROOTS, ALLOC_ROOTS) fully describe all GcPtrs held
/// by suspended tasks and can be scanned safely.
///
/// Under `no-gc` this is a no-op.
#[cfg(feature = "no-gc")]
pub fn async_gc_collect() {}

#[cfg(not(feature = "no-gc"))]
pub fn async_gc_collect() {
    // Stress mode: same forcing as gc_safepoint, for the async runtime's
    // collection points.
    if cljrs_gc::debug_modes::stress_due() {
        cljrs_gc::request_gc();
    }

    if !cljrs_gc::gc_requested() && !cljrs_gc::CONFIG_CANCELLATION.in_progress() {
        return;
    }
    if cljrs_gc::CONFIG_CANCELLATION.in_progress() {
        cljrs_gc::safepoint();
        return;
    }
    if !cljrs_gc::take_gc_request() {
        cljrs_gc::safepoint();
        return;
    }
    let Some(_stw_guard) = cljrs_gc::begin_stw() else {
        cljrs_gc::safepoint();
        return;
    };
    cljrs_gc::HEAP.collect_with_stack_scan(|visitor| {
        cljrs_gc::HEAP.trace_registered_roots(visitor);
        trace_thread_env_roots(visitor);
        trace_value_roots(visitor);
        cljrs_value::trace_interop_methods(visitor);
        trace_option_value_roots(visitor);
        dynamics::trace_current(visitor);
        crate::taps::trace_roots(visitor);
        cljrs_gc::trace_thread_alloc_roots(visitor);
    });
    // Reclaim superseded JIT code while the world is still stopped.
    run_stw_reclaim();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_roots_snapshot() -> Vec<(*const cljrs_value::Value, usize)> {
        VALUE_ROOTS.with(|r| r.borrow().clone())
    }

    /// Guards stored in structs (ValueIter) drop in arbitrary order relative
    /// to scoped guards.  A LIFO pop here unregisters a still-live root and
    /// leaves the dead entry dangling for the tracer (the optimizer-suite
    /// SIGSEGV of 2026-08-27); removal must be by identity.
    #[test]
    fn value_root_guards_unregister_by_identity_not_lifo() {
        let a = cljrs_value::Value::Long(1);
        let b = cljrs_value::Value::Long(2);
        let ga = root_value(&a);
        let gb = root_value(&b);
        drop(ga); // out of LIFO order
        let snap = value_roots_snapshot();
        assert!(
            snap.contains(&(&b as *const _, 1)),
            "live root must survive an out-of-order guard drop"
        );
        assert!(
            !snap.contains(&(&a as *const _, 1)),
            "dropped guard's entry must be unregistered"
        );
        drop(gb);
        assert!(!value_roots_snapshot().contains(&(&b as *const _, 1)));
    }
}

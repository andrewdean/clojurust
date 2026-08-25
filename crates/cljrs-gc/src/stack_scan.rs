//! Conservative stack scanning: an additional GC root source that closes
//! the in-flight rooting class (docs/gc-inflight-rooting-bug.md).
//!
//! Precise rooting cannot see a `Value` held only in a Rust local while a
//! builtin re-enters evaluation.  This module scans the collecting thread's
//! own stack for words that equal a live heap object's address and marks the
//! hits.  The scan is sound because the heap is non-moving mark-sweep and
//! `GcPtr` is `!Send`: heaps are per-thread, so no other thread's stack can
//! reference the heap being collected, and a false positive only retains
//! garbage for one extra cycle.
//!
//! Enablement: on by default; `CLJRS_GC_CONSERVATIVE=0` (or `off`/`false`)
//! disables it, and embedders can call [`set_conservative`].  The scan runs
//! only for collections entered through the interpreter's production paths
//! (`collect_with_stack_scan`); plain `collect` keeps precise-only semantics
//! so tests that assert exact free behavior stay deterministic.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Runtime switch, initialized from `CLJRS_GC_CONSERVATIVE` on first use.
static ENABLED: OnceLock<AtomicBool> = OnceLock::new();

fn enabled_cell() -> &'static AtomicBool {
    ENABLED.get_or_init(|| {
        let on = match std::env::var("CLJRS_GC_CONSERVATIVE") {
            Ok(v) => !matches!(v.trim(), "0" | "off" | "false" | "no"),
            Err(_) => true,
        };
        AtomicBool::new(on)
    })
}

/// Whether conservative stack scanning is enabled.
pub fn conservative_enabled() -> bool {
    enabled_cell().load(Ordering::Relaxed)
}

/// Override the conservative-scan switch (tests, embedders).
pub fn set_conservative(on: bool) {
    enabled_cell().store(on, Ordering::Relaxed);
}

thread_local! {
    /// Highest stack address to scan to, recorded near thread start by
    /// `record_stack_base` (via `register_mutator`).  Zero = unset.
    static STACK_BASE: Cell<usize> = const { Cell::new(0) };
    /// Cached OS-derived stack top for threads that never registered.
    static OS_STACK_TOP: Cell<usize> = const { Cell::new(0) };
}

/// Record the current stack position as this thread's scan ceiling.  Called
/// from `register_mutator`, which mutator threads run at thread start, so
/// every frame that can hold a `Value` lies below the recorded address.
#[inline(never)]
pub fn record_stack_base() {
    let marker: usize = 0;
    STACK_BASE.with(|b| b.set(&marker as *const usize as usize));
}

/// The address above which this thread's frames cannot hold heap pointers.
/// Prefers the `record_stack_base` mark; falls back to the OS's exact
/// per-thread stack bounds, else `None` and the scan is skipped for this
/// thread.  (`/proc/self/maps` is NOT a valid source here: the kernel merges
/// adjacent same-permission VMAs, so a thread stack's displayed mapping can
/// extend into a neighboring thread's allocation that later gets unmapped.)
#[cfg(not(feature = "no-gc"))]
fn stack_top(sp: usize) -> Option<usize> {
    let recorded = STACK_BASE.with(|b| b.get());
    if recorded > sp {
        return Some(recorded);
    }
    let cached = OS_STACK_TOP.with(|t| t.get());
    if cached > sp {
        return Some(cached);
    }
    let found = os_stack_top()?;
    if found <= sp {
        return None;
    }
    OS_STACK_TOP.with(|t| t.set(found));
    Some(found)
}

/// Exact top of the current thread's stack from pthreads.  For the main
/// thread glibc reports the rlimit-sized range; the top end is exact, and
/// `[sp, top)` is the already-grown (mapped) region either way.
#[cfg(not(feature = "no-gc"))]
#[cfg(any(target_os = "linux", target_os = "android"))]
fn os_stack_top() -> Option<usize> {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return None;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::size_t = 0;
        let ok = libc::pthread_attr_getstack(&attr, &mut addr, &mut size) == 0;
        libc::pthread_attr_destroy(&mut attr);
        if !ok {
            return None;
        }
        Some(addr as usize + size)
    }
}

/// macOS: `pthread_get_stackaddr_np` returns the high end (stacks grow down).
#[cfg(not(feature = "no-gc"))]
#[cfg(target_os = "macos")]
fn os_stack_top() -> Option<usize> {
    unsafe { Some(libc::pthread_get_stackaddr_np(libc::pthread_self()) as usize) }
}

#[cfg(not(feature = "no-gc"))]
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn os_stack_top() -> Option<usize> {
    None
}

/// Copy the callee-saved registers into a stack-resident array.  A caller
/// value that lives only in a callee-saved register is thereby visible to
/// the scan: either through this array, or through the register spill this
/// function's prologue performs because the registers are clobbered here.
#[cfg(not(feature = "no-gc"))]
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn spill_registers() -> [usize; 6] {
    let mut regs = [0usize; 6];
    unsafe {
        std::arch::asm!(
            "mov {0}, rbx",
            "mov {1}, rbp",
            "mov {2}, r12",
            "mov {3}, r13",
            "mov {4}, r14",
            "mov {5}, r15",
            out(reg) regs[0],
            out(reg) regs[1],
            out(reg) regs[2],
            out(reg) regs[3],
            out(reg) regs[4],
            out(reg) regs[5],
            options(nostack, nomem, preserves_flags),
        );
    }
    regs
}

#[cfg(not(feature = "no-gc"))]
#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn spill_registers() -> [usize; 11] {
    let mut regs = [0usize; 11];
    unsafe {
        std::arch::asm!(
            "mov {0}, x19",
            "mov {1}, x20",
            "mov {2}, x21",
            "mov {3}, x22",
            "mov {4}, x23",
            "mov {5}, x24",
            "mov {6}, x25",
            "mov {7}, x26",
            "mov {8}, x27",
            "mov {9}, x28",
            "mov {10}, x29",
            out(reg) regs[0],
            out(reg) regs[1],
            out(reg) regs[2],
            out(reg) regs[3],
            out(reg) regs[4],
            out(reg) regs[5],
            out(reg) regs[6],
            out(reg) regs[7],
            out(reg) regs[8],
            out(reg) regs[9],
            out(reg) regs[10],
            options(nostack, nomem, preserves_flags),
        );
    }
    regs
}

#[cfg(not(feature = "no-gc"))]
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(never)]
fn spill_registers() -> [usize; 0] {
    // No register capture on this architecture: values spilled to the stack
    // are still found; values held only in registers across the safepoint
    // are not.  The lives grace period remains the fallback there.
    []
}

/// Scan the current thread's stack for words equal to a live object address
/// and mark the hits.  `objects` must be sorted by address and contain every
/// live `GcBoxHeader` pointer of the heap being collected.  Call only after
/// precise marking has drained, so the return value counts exactly the
/// objects precise rooting missed.  Returns that rescue count.
#[cfg(not(feature = "no-gc"))]
#[inline(never)]
pub(crate) fn scan_and_mark(
    objects: &[*mut crate::GcBoxHeader],
    visitor: &mut crate::MarkVisitor,
) -> usize {
    if objects.is_empty() {
        return 0;
    }
    let regs = spill_registers();
    let marker: usize = 0;
    let sp = (&marker as *const usize as usize) & !(std::mem::size_of::<usize>() - 1);
    let Some(top) = stack_top(sp) else {
        return 0;
    };
    let lo = objects[0] as usize;
    let hi = objects[objects.len() - 1] as usize;
    let mut rescued = 0usize;
    let mut addr = sp;
    while addr + std::mem::size_of::<usize>() <= top {
        // SAFETY: [sp, top) is this thread's own mapped stack; volatile
        // reads of possibly-dead slots are how every conservative scanner
        // works, and the values are only compared, never dereferenced
        // except through the authoritative `objects` pointers.
        let word = unsafe { std::ptr::read_volatile(addr as *const usize) };
        if word >= lo
            && word <= hi
            && let Ok(idx) = objects.binary_search(&(word as *mut crate::GcBoxHeader))
        {
            let header = objects[idx];
            // SAFETY: `header` comes from the heap's live-object list.
            unsafe {
                if (*header).lives.get() < crate::gc_header::GC_INITIAL_LIVES {
                    rescued += 1;
                    visitor.mark_header(header);
                }
            }
        }
        addr += std::mem::size_of::<usize>();
    }
    std::hint::black_box(&regs);
    rescued
}

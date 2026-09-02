//! Regression: primitive arrays reachable only through a traced collection
//! must survive garbage collection.
//!
//! `Value::trace` used to match every primitive-array variant to `{}` — the
//! boxes hold no child Values, but they are GcPtr heap objects and must be
//! marked. An array stored inside a collection (and not also live in a
//! conservatively scanned stack slot) was swept while still referenced; a
//! later aget saw a freed Vec (observed as length 0) or panicked locking
//! the freed Mutex. Arrays held in stack locals survived via conservative
//! root scanning, which is why this stayed hidden until long-lived arrays
//! were stored inside collections.
//!
//! The test observes the sweep itself (total_freed) rather than reading
//! back through the pointer, so a regression fails cleanly instead of
//! exercising use-after-free.

#![cfg(not(feature = "no-gc"))]

use std::sync::Mutex;

use cljrs_gc::{GcPtr, HEAP, Trace, push_alloc_frame};
use cljrs_value::{PersistentVector, Value};

#[test]
fn primitive_arrays_inside_collections_survive_collection() {
    // Flush any pre-existing garbage so total_freed stays flat afterwards.
    // (Objects get GC_INITIAL_LIVES sweeps of grace, so collect a few times.)
    for _ in 0..4 {
        HEAP.collect(|_| {});
    }

    // One of each primitive-array flavor, reachable only through the vector.
    // The alloc frame pins fresh allocations as GC roots until it drops;
    // without dropping it the arrays would be rooted no matter what trace
    // does, and the test could never fail.
    let frame = push_alloc_frame();
    let root = Value::Vector(GcPtr::new(PersistentVector::from_iter(vec![
        Value::DoubleArray(GcPtr::new(Mutex::new(vec![1.5_f64; 32]))),
        Value::LongArray(GcPtr::new(Mutex::new(vec![7_i64; 32]))),
        Value::IntArray(GcPtr::new(Mutex::new(vec![3_i32; 32]))),
        Value::BooleanArray(GcPtr::new(Mutex::new(vec![true; 32]))),
    ])));

    drop(frame);

    let freed_before = HEAP.total_freed();
    for _ in 0..4 {
        HEAP.collect(|visitor| root.trace(visitor));
    }
    let freed_during = HEAP.total_freed() - freed_before;
    assert_eq!(
        freed_during, 0,
        "collection freed {freed_during} object(s) that are reachable through the vector"
    );

    // The arrays are still intact and usable.
    if let Value::Vector(v) = &root {
        let first = v.get().iter().next().cloned();
        match first {
            Some(Value::DoubleArray(a)) => {
                let guard = a.get().lock().unwrap();
                assert_eq!(guard.len(), 32);
                assert_eq!(guard[0], 1.5);
            }
            other => panic!("expected DoubleArray, got {other:?}"),
        }
    }
}

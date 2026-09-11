//! Process-global GC statistics counters.
//!
//! Tracks three classes of events:
//!
//! 1. **GC heap allocations** — every object allocated through `GcHeap::alloc`
//!    (default build) bumps `gc_allocations` and `gc_alloc_bytes`.
//! 2. **Region (bump) allocations** — every object allocated through
//!    [`crate::region::Region::alloc`] bumps `region_allocations` and
//!    `region_alloc_bytes`.  These represent cases where the bump allocator
//!    was used instead of the GC heap.
//! 3. **Stop-the-world collections** — every completed `GcHeap::collect`
//!    bumps `gc_collections` and accumulates pause time, freed object count,
//!    and freed bytes.
//! 4. **Isolate boundary crossings** — every value deep-copied across an
//!    isolate boundary (the Phase B2 structured-clone seam) bumps
//!    `boundary_crossings`, accumulates the estimated bytes copied, and the
//!    time spent serializing. This is the metering the isolate-boundary plan
//!    requires so a silent fan-out copy shows up as a number, not mystery
//!    latency.  The largest single crossing and a three-bucket size
//!    histogram (C5 telemetry review) say *which* values dominate: the
//!    `shared-vec` zero-copy path only pays off for the large bucket.
//! 5. **Memory-pressure transitions** — every change of the process-wide
//!    [`crate::pressure::PressureLevel`].
//!
//! Counters are process-global ([`GC_STATS`]) and thread-safe via atomics.
//! Reset is not supported — the counters live for the lifetime of the process.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Process-global GC statistics counters.
pub struct GcStats {
    gc_allocations: AtomicU64,
    gc_alloc_bytes: AtomicU64,
    region_allocations: AtomicU64,
    region_alloc_bytes: AtomicU64,
    region_poisons: AtomicU64,
    gc_collections: AtomicU64,
    gc_pause_total_nanos: AtomicU64,
    gc_objects_freed: AtomicU64,
    gc_bytes_freed: AtomicU64,
    boundary_crossings: AtomicU64,
    boundary_bytes_copied: AtomicU64,
    boundary_copy_total_nanos: AtomicU64,
    boundary_max_bytes: AtomicU64,
    boundary_small: AtomicU64,
    boundary_medium: AtomicU64,
    boundary_large: AtomicU64,
    conservative_rescues: AtomicU64,
    pressure_transitions: AtomicU64,
}

/// Upper bound (inclusive) of the "small" boundary-crossing bucket.
///
/// 4 KiB is a typical request or reply map: a crossing this size costs
/// about as much as the channel operation carrying it, so nothing here is
/// worth making zero-copy.
pub const BOUNDARY_SMALL_MAX: u64 = 4 * 1024;
/// Upper bound (inclusive) of the "medium" bucket.
///
/// Up to 256 KiB the deep copy is still tens of microseconds; above it a
/// crossing starts to show up as latency, which is where a refcount bump
/// instead of a copy would matter.
pub const BOUNDARY_MEDIUM_MAX: u64 = 256 * 1024;

impl GcStats {
    pub const fn new() -> Self {
        Self {
            gc_allocations: AtomicU64::new(0),
            gc_alloc_bytes: AtomicU64::new(0),
            region_allocations: AtomicU64::new(0),
            region_alloc_bytes: AtomicU64::new(0),
            region_poisons: AtomicU64::new(0),
            gc_collections: AtomicU64::new(0),
            gc_pause_total_nanos: AtomicU64::new(0),
            gc_objects_freed: AtomicU64::new(0),
            gc_bytes_freed: AtomicU64::new(0),
            boundary_crossings: AtomicU64::new(0),
            boundary_bytes_copied: AtomicU64::new(0),
            boundary_copy_total_nanos: AtomicU64::new(0),
            boundary_max_bytes: AtomicU64::new(0),
            boundary_small: AtomicU64::new(0),
            boundary_medium: AtomicU64::new(0),
            boundary_large: AtomicU64::new(0),
            conservative_rescues: AtomicU64::new(0),
            pressure_transitions: AtomicU64::new(0),
        }
    }

    /// Record one allocation through the GC heap (`bytes` is the estimated
    /// or actual allocated size).
    #[inline]
    pub fn record_gc_alloc(&self, bytes: usize) {
        self.gc_allocations.fetch_add(1, Ordering::Relaxed);
        self.gc_alloc_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record one allocation through a bump region (instead of the GC heap).
    #[inline]
    pub fn record_region_alloc(&self, bytes: usize) {
        self.region_allocations.fetch_add(1, Ordering::Relaxed);
        self.region_alloc_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record one poisoning of the active regions (a publish-barrier hit on
    /// a value that was opaque to heap promotion; the affected regions are
    /// retired instead of reset).
    #[inline]
    pub fn record_region_poison(&self) {
        self.region_poisons.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a completed stop-the-world collection.
    #[inline]
    pub fn record_gc_pause(&self, pause: Duration, freed_objects: u64, freed_bytes: u64) {
        self.gc_collections.fetch_add(1, Ordering::Relaxed);
        self.gc_pause_total_nanos
            .fetch_add(pause.as_nanos() as u64, Ordering::Relaxed);
        self.gc_objects_freed
            .fetch_add(freed_objects, Ordering::Relaxed);
        self.gc_bytes_freed
            .fetch_add(freed_bytes, Ordering::Relaxed);
    }

    /// Record one value crossing an isolate boundary (the Phase B2
    /// structured-clone seam). `bytes` is the estimated heap footprint of the
    /// deep copy; `copy_time` is the time spent serializing it.
    #[inline]
    pub fn record_boundary_crossing(&self, bytes: u64, copy_time: Duration) {
        self.boundary_crossings.fetch_add(1, Ordering::Relaxed);
        self.boundary_bytes_copied
            .fetch_add(bytes, Ordering::Relaxed);
        self.boundary_copy_total_nanos
            .fetch_add(copy_time.as_nanos() as u64, Ordering::Relaxed);
        self.boundary_max_bytes.fetch_max(bytes, Ordering::Relaxed);
        let bucket = if bytes <= BOUNDARY_SMALL_MAX {
            &self.boundary_small
        } else if bytes <= BOUNDARY_MEDIUM_MAX {
            &self.boundary_medium
        } else {
            &self.boundary_large
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one transition of the process-wide memory-pressure level.
    #[inline]
    pub fn record_pressure_transition(&self) {
        self.pressure_transitions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record objects the conservative stack scan rescued in one
    /// collection: values precise rooting missed (held only in Rust locals
    /// across a re-entrant call).  A nonzero total is the audit signal from
    /// docs/gc-inflight-rooting-bug.md.
    #[inline]
    pub fn record_conservative_rescues(&self, count: u64) {
        self.conservative_rescues
            .fetch_add(count, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of all counters.
    pub fn snapshot(&self) -> GcStatsSnapshot {
        GcStatsSnapshot {
            gc_allocations: self.gc_allocations.load(Ordering::Relaxed),
            gc_alloc_bytes: self.gc_alloc_bytes.load(Ordering::Relaxed),
            region_allocations: self.region_allocations.load(Ordering::Relaxed),
            region_alloc_bytes: self.region_alloc_bytes.load(Ordering::Relaxed),
            region_poisons: self.region_poisons.load(Ordering::Relaxed),
            gc_collections: self.gc_collections.load(Ordering::Relaxed),
            gc_pause_total_nanos: self.gc_pause_total_nanos.load(Ordering::Relaxed),
            gc_objects_freed: self.gc_objects_freed.load(Ordering::Relaxed),
            gc_bytes_freed: self.gc_bytes_freed.load(Ordering::Relaxed),
            boundary_crossings: self.boundary_crossings.load(Ordering::Relaxed),
            boundary_bytes_copied: self.boundary_bytes_copied.load(Ordering::Relaxed),
            boundary_copy_total_nanos: self.boundary_copy_total_nanos.load(Ordering::Relaxed),
            boundary_max_bytes: self.boundary_max_bytes.load(Ordering::Relaxed),
            boundary_small: self.boundary_small.load(Ordering::Relaxed),
            boundary_medium: self.boundary_medium.load(Ordering::Relaxed),
            boundary_large: self.boundary_large.load(Ordering::Relaxed),
            conservative_rescues: self.conservative_rescues.load(Ordering::Relaxed),
            pressure_transitions: self.pressure_transitions.load(Ordering::Relaxed),
            pressure_level: crate::pressure::level(),
            pressure_live_bytes: crate::pressure::live_bytes() as u64,
            pressure_peak_bytes: crate::pressure::peak_live_bytes() as u64,
            pressure_budget_bytes: crate::pressure::budget() as u64,
        }
    }
}

impl Default for GcStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable point-in-time view of [`GcStats`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GcStatsSnapshot {
    pub gc_allocations: u64,
    pub gc_alloc_bytes: u64,
    pub region_allocations: u64,
    pub region_alloc_bytes: u64,
    pub region_poisons: u64,
    pub gc_collections: u64,
    pub gc_pause_total_nanos: u64,
    pub gc_objects_freed: u64,
    pub gc_bytes_freed: u64,
    pub boundary_crossings: u64,
    pub boundary_bytes_copied: u64,
    pub boundary_copy_total_nanos: u64,
    /// Largest single crossing, in estimated bytes.
    pub boundary_max_bytes: u64,
    /// Crossings of at most [`BOUNDARY_SMALL_MAX`] bytes.
    pub boundary_small: u64,
    /// Crossings above small and at most [`BOUNDARY_MEDIUM_MAX`] bytes.
    pub boundary_medium: u64,
    /// Crossings above [`BOUNDARY_MEDIUM_MAX`] bytes.
    pub boundary_large: u64,
    pub conservative_rescues: u64,
    /// Level changes of the process-wide pressure coordinator.
    pub pressure_transitions: u64,
    /// Pressure level at snapshot time.
    pub pressure_level: crate::pressure::PressureLevel,
    /// Process live bytes as published by every heap at snapshot time.
    pub pressure_live_bytes: u64,
    /// High-water mark of `pressure_live_bytes`.
    pub pressure_peak_bytes: u64,
    /// Process memory budget; zero means the coordinator is off.
    pub pressure_budget_bytes: u64,
}

impl GcStatsSnapshot {
    /// Total cumulative GC pause time.
    pub fn total_pause(&self) -> Duration {
        Duration::from_nanos(self.gc_pause_total_nanos)
    }

    /// Total cumulative time spent serializing values across isolate boundaries.
    pub fn total_boundary_copy(&self) -> Duration {
        Duration::from_nanos(self.boundary_copy_total_nanos)
    }
}

impl fmt::Display for GcStatsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "GC stats:")?;
        writeln!(
            f,
            "  GC allocations:        {} ({} bytes)",
            self.gc_allocations, self.gc_alloc_bytes
        )?;
        writeln!(
            f,
            "  Region (bump) allocs:  {} ({} bytes)",
            self.region_allocations, self.region_alloc_bytes
        )?;
        writeln!(f, "  Region poisons:        {}", self.region_poisons)?;
        writeln!(f, "  GC collections:        {}", self.gc_collections)?;
        writeln!(f, "  Total GC pause time:   {:.3?}", self.total_pause())?;
        writeln!(f, "  Objects freed by GC:   {}", self.gc_objects_freed)?;
        writeln!(f, "  Bytes freed by GC:     {}", self.gc_bytes_freed)?;
        writeln!(
            f,
            "  Boundary crossings:    {} ({} bytes copied, largest {} bytes)",
            self.boundary_crossings, self.boundary_bytes_copied, self.boundary_max_bytes
        )?;
        writeln!(
            f,
            "  Boundary size buckets: <=4 KiB: {}, <=256 KiB: {}, >256 KiB: {}",
            self.boundary_small, self.boundary_medium, self.boundary_large
        )?;
        writeln!(
            f,
            "  Boundary copy time:    {:.3?}",
            self.total_boundary_copy()
        )?;
        writeln!(f, "  Conservative rescues:  {}", self.conservative_rescues)?;
        write!(
            f,
            "  Memory pressure:       {} ({} live of {} budget bytes, peak {}, {} transitions)",
            self.pressure_level,
            self.pressure_live_bytes,
            self.pressure_budget_bytes,
            self.pressure_peak_bytes,
            self.pressure_transitions
        )
    }
}

/// Process-global GC statistics counters.
pub static GC_STATS: GcStats = GcStats::new();

/// Environment variable consulted by [`dump_stats_from_env`].
pub const CLJRS_GC_STATS_ENV: &str = "CLJRS_GC_STATS";

/// If `CLJRS_GC_STATS` is set in the environment, write a snapshot of
/// [`GC_STATS`] to its target.  Intended to be called once at program exit
/// from AOT-compiled binaries (and the AOT test harness), which have no CLI
/// flag parsing of their own.
///
/// Target conventions:
/// - unset → do nothing
/// - empty string or `"-"` → write to stdout
/// - any other value → treated as a filesystem path
///
/// I/O failures are reported on stderr; they never panic.
pub fn dump_stats_from_env() {
    let target = match std::env::var(CLJRS_GC_STATS_ENV) {
        Ok(t) => t,
        Err(_) => return,
    };

    let snapshot = GC_STATS.snapshot();
    let result = if target.is_empty() || target == "-" {
        println!("{snapshot}");
        Ok(())
    } else {
        std::fs::write(&target, format!("{snapshot}\n"))
    };

    if let Err(e) = result {
        eprintln!("cljrs: failed to write GC stats to {target:?}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_zero_by_default() {
        let stats = GcStats::new();
        let snap = stats.snapshot();
        assert_eq!(snap.gc_allocations, 0);
        assert_eq!(snap.region_allocations, 0);
        assert_eq!(snap.gc_collections, 0);
        assert_eq!(snap.total_pause(), Duration::ZERO);
    }

    #[test]
    fn record_gc_alloc_updates_counters() {
        let stats = GcStats::new();
        stats.record_gc_alloc(48);
        stats.record_gc_alloc(48);
        let snap = stats.snapshot();
        assert_eq!(snap.gc_allocations, 2);
        assert_eq!(snap.gc_alloc_bytes, 96);
    }

    #[test]
    fn record_region_alloc_updates_counters() {
        let stats = GcStats::new();
        stats.record_region_alloc(16);
        stats.record_region_alloc(32);
        let snap = stats.snapshot();
        assert_eq!(snap.region_allocations, 2);
        assert_eq!(snap.region_alloc_bytes, 48);
    }

    #[test]
    fn record_gc_pause_updates_counters() {
        let stats = GcStats::new();
        stats.record_gc_pause(Duration::from_micros(500), 7, 336);
        stats.record_gc_pause(Duration::from_micros(250), 3, 144);
        let snap = stats.snapshot();
        assert_eq!(snap.gc_collections, 2);
        assert_eq!(snap.gc_objects_freed, 10);
        assert_eq!(snap.gc_bytes_freed, 480);
        assert_eq!(snap.total_pause(), Duration::from_micros(750));
    }

    #[test]
    fn record_boundary_crossing_updates_counters() {
        let stats = GcStats::new();
        stats.record_boundary_crossing(2048, Duration::from_micros(40));
        stats.record_boundary_crossing(1024, Duration::from_micros(10));
        let snap = stats.snapshot();
        assert_eq!(snap.boundary_crossings, 2);
        assert_eq!(snap.boundary_bytes_copied, 3072);
        assert_eq!(snap.total_boundary_copy(), Duration::from_micros(50));
        assert_eq!(snap.boundary_max_bytes, 2048);
        assert_eq!(snap.boundary_small, 2);
        assert_eq!(snap.boundary_medium, 0);
        assert_eq!(snap.boundary_large, 0);
    }

    #[test]
    fn boundary_buckets_split_on_documented_edges() {
        let stats = GcStats::new();
        stats.record_boundary_crossing(BOUNDARY_SMALL_MAX, Duration::ZERO);
        stats.record_boundary_crossing(BOUNDARY_SMALL_MAX + 1, Duration::ZERO);
        stats.record_boundary_crossing(BOUNDARY_MEDIUM_MAX, Duration::ZERO);
        stats.record_boundary_crossing(BOUNDARY_MEDIUM_MAX + 1, Duration::ZERO);
        let snap = stats.snapshot();
        assert_eq!(
            (
                snap.boundary_small,
                snap.boundary_medium,
                snap.boundary_large
            ),
            (1, 2, 1)
        );
        assert_eq!(snap.boundary_max_bytes, BOUNDARY_MEDIUM_MAX + 1);
    }

    #[test]
    fn display_renders_all_fields() {
        let snap = GcStatsSnapshot {
            gc_allocations: 5,
            gc_alloc_bytes: 240,
            region_allocations: 3,
            region_alloc_bytes: 96,
            region_poisons: 0,
            gc_collections: 1,
            gc_pause_total_nanos: 1_000_000,
            gc_objects_freed: 2,
            gc_bytes_freed: 96,
            boundary_crossings: 4,
            boundary_bytes_copied: 8192,
            boundary_copy_total_nanos: 2_000_000,
            boundary_max_bytes: 4096,
            boundary_small: 3,
            boundary_medium: 1,
            boundary_large: 0,
            conservative_rescues: 1,
            pressure_transitions: 2,
            pressure_level: crate::pressure::PressureLevel::Yellow,
            pressure_live_bytes: 800,
            pressure_peak_bytes: 900,
            pressure_budget_bytes: 1000,
        };
        let s = format!("{snap}");
        assert!(s.contains("GC allocations:"));
        assert!(s.contains("Region (bump) allocs:"));
        assert!(s.contains("Region poisons:"));
        assert!(s.contains("GC collections:"));
        assert!(s.contains("Total GC pause time:"));
        assert!(s.contains("Objects freed by GC:"));
        assert!(s.contains("Conservative rescues:"));
        assert!(s.contains("Bytes freed by GC:"));
        assert!(s.contains("Boundary crossings:"));
        assert!(s.contains("largest 4096 bytes"));
        assert!(s.contains("Boundary size buckets: <=4 KiB: 3, <=256 KiB: 1, >256 KiB: 0"));
        assert!(s.contains("Boundary copy time:"));
        assert!(s.contains(
            "Memory pressure:       yellow (800 live of 1000 budget bytes, peak 900, 2 transitions)"
        ));
    }
}

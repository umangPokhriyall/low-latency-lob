//! Low-overhead monotonic clock for ns-scale measurement, plus its own overhead.
//!
//! Wraps `quanta::Clock` (TSC-backed, calibrated, **safe** API — no `rdtsc`
//! `unsafe` leaks into `bench`, which keeps `#![forbid(unsafe_code)]`). The
//! clock's own read-read cost is measured at construction and reported as a
//! floor: §3.2 forbids silently subtracting it, so a per-op number can be read
//! against the floor that produced it.

use quanta::Clock;
use std::hint::black_box;

/// Iterations used to characterise the clock's read-read overhead (§3.2 asks
/// for ≥100k; we use more for a stable median).
const OVERHEAD_ITERS: usize = 200_000;

/// Convert a `Duration`-worth of nanoseconds (`u128`) to `u64`, saturating.
/// Per-op deltas are tiny; this only guards the type boundary clippy-cleanly.
#[inline]
fn ns_u64(nanos: u128) -> u64 {
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub struct BenchClock {
    clock: Clock,
    overhead_ns: u64,
}

impl BenchClock {
    /// Build the clock and measure its own read-read overhead once.
    #[must_use]
    pub fn new() -> Self {
        let clock = Clock::new();
        let overhead_ns = Self::measure_overhead(&clock);
        Self { clock, overhead_ns }
    }

    /// Raw monotonic counter read — the cheapest timestamp the clock offers.
    /// Bracket a measured op with two `raw()` calls and one `delta_ns`.
    #[inline]
    #[must_use]
    pub fn raw(&self) -> u64 {
        self.clock.raw()
    }

    /// Nanoseconds between two raw reads `a` (earlier) and `b` (later).
    #[inline]
    #[must_use]
    pub fn delta_ns(&self, a: u64, b: u64) -> u64 {
        ns_u64(self.clock.delta(a, b).as_nanos())
    }

    /// The measured read-read floor in ns: the cost below which a per-op number
    /// is indistinguishable from clock noise. Reported, never subtracted (§3.2).
    #[must_use]
    pub fn overhead_ns(&self) -> u64 {
        self.overhead_ns
    }

    /// Absolute nanoseconds elapsed since a `base` raw reading. The CO-correct
    /// sustained loop (§3.1) takes one `base = raw()` at the start and compares
    /// `now_since_ns(base)` against each event's scheduled arrival offset.
    #[inline]
    #[must_use]
    pub fn now_since_ns(&self, base: u64) -> u64 {
        self.delta_ns(base, self.raw())
    }

    /// Median read-read delta over `OVERHEAD_ITERS` iterations. Both reads are
    /// `black_box`ed so the optimizer cannot fold the pair away (which would
    /// report a fictitious 0 ns floor).
    fn measure_overhead(clock: &Clock) -> u64 {
        let mut samples = Vec::with_capacity(OVERHEAD_ITERS);
        for _ in 0..OVERHEAD_ITERS {
            let a = black_box(clock.raw());
            let b = black_box(clock.raw());
            samples.push(ns_u64(clock.delta(a, b).as_nanos()));
        }
        samples.sort_unstable();
        samples[samples.len() / 2]
    }
}

impl Default for BenchClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock's overhead is measurable (non-zero) and recorded — a 0 ns floor
    /// would mean the read-read pair was elided, invalidating every measurement.
    #[test]
    fn clock_overhead_is_measured_and_recorded() {
        let clock = BenchClock::new();
        assert!(
            clock.overhead_ns() > 0,
            "clock read-read overhead must be a measurable, non-zero floor"
        );
        // Sanity: a TSC read is cheap — the floor should be well under a µs.
        assert!(
            clock.overhead_ns() < 1_000,
            "implausibly large clock floor: {} ns",
            clock.overhead_ns()
        );
    }

    /// `delta_ns` is monotonic and non-negative for ordered reads.
    #[test]
    fn delta_ns_is_non_negative_for_ordered_reads() {
        let clock = BenchClock::new();
        let a = clock.raw();
        // A little work between reads so the delta is unambiguously positive.
        let mut acc = 0u64;
        for i in 0..10_000u64 {
            acc = acc.wrapping_add(i);
        }
        let b = clock.raw();
        let _ = std::hint::black_box(acc);
        assert!(b >= a, "raw counter must not go backwards");
        // delta is u64; just exercise the path.
        let _ = clock.delta_ns(a, b);
    }
}

//! Real-thread torn-read stress test for `SeqLock` (Phase 6 §5).
//!
//! loom (`tests/loom_seqlock.rs`) proves the memory-ordering MODEL exhaustively
//! but only for a tiny, bounded interleaving. This test corroborates the proof on
//! real hardware and the real `std` atomics: one writer storing monotonically-
//! stamped witness snapshots as fast as it can for a fixed iteration budget, and
//! `N = max(2, available_parallelism() - 1)` reader threads hammering `load()` and
//! checking the §4 consistency witness on EVERY returned snapshot.
//!
//! Two properties are asserted:
//!   1. Zero witness violations across the (millions of) reads — no torn snapshot
//!      ever escapes the version check.
//!   2. Writer-never-blocks (coarse): the writer completes its full, fixed
//!      iteration budget regardless of how many readers are pounding the cell —
//!      readers cannot stall the wait-free writer.
//!
//! Consistency witness (same as loom §4): every field of a stored snapshot is a
//! deterministic function of `stamp` — `bid_px = stamp*4`, `bid_qty = +1`,
//! `ask_px = +2`, `ask_qty = +3`. A snapshot whose fields come from two different
//! stamps (a torn read) violates these relations and is counted as a violation.
//!
//! `std` threads only, no deps. Runtime is bounded to a couple of seconds (the
//! writer's fixed budget) so it fits CI. Run `cargo test -p sync --
//! --nocapture` to see the read/retry/throughput summary.
#![cfg(not(loom))]
// Stamps are bounded by `WRITER_BUDGET` (10M), far below `i64::MAX`, so the
// witness's `stamp as i64` never wraps. No floats: the retry ratio is reported
// with integer arithmetic (the `sync` crate forbids `f64`/`f32`).
#![allow(clippy::cast_possible_wrap)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, available_parallelism};
use std::time::Instant;
use sync::{SeqLock, TopOfBook};

/// Fixed writer budget: number of `store`s the single writer performs before it
/// signals readers to stop. Sized so the run lasts ~1s on a typical host while the
/// readers accumulate well into the millions of `load`s.
const WRITER_BUDGET: u64 = 10_000_000;

/// The witness snapshot for a given `stamp`.
fn snapshot_for(stamp: u64) -> TopOfBook {
    let base = (stamp as i64).wrapping_mul(4);
    TopOfBook { bid_px: base, bid_qty: base + 1, ask_px: base + 2, ask_qty: base + 3, stamp }
}

/// True iff `t` is a consistent (non-torn) witness snapshot.
fn is_consistent(t: TopOfBook) -> bool {
    let base = (t.stamp as i64).wrapping_mul(4);
    t.bid_px == base && t.bid_qty == base + 1 && t.ask_px == base + 2 && t.ask_qty == base + 3
}

/// Per-reader tally returned from each reader thread.
#[derive(Debug, Default)]
struct ReaderStats {
    reads: u64,
    retries: u64,
    violations: u64,
    max_stamp: u64,
    /// The first torn snapshot seen, if any (for a useful failure message).
    first_bad: Option<TopOfBook>,
}

#[test]
fn stress_no_torn_reads() {
    let readers = available_parallelism().map_or(2, |n| (n.get() - 1).max(2));

    // seq starts even/stable holding stamp 0; readers may observe it before the
    // writer's first store, which is consistent (stamp 0).
    let cell = Arc::new(SeqLock::new(snapshot_for(0)));
    let done = Arc::new(AtomicBool::new(false));

    // Readers: load until the writer signals done, asserting the witness each time.
    let reader_handles: Vec<_> = (0..readers)
        .map(|_| {
            let cell = Arc::clone(&cell);
            let done = Arc::clone(&done);
            thread::spawn(move || {
                let mut s = ReaderStats::default();
                // `Relaxed` on the flag is fine: it only ends the loop; correctness
                // of each snapshot is carried entirely by the seqlock itself.
                while !done.load(Ordering::Relaxed) {
                    let (t, retries) = cell.load_counted();
                    s.reads += 1;
                    s.retries += u64::from(retries);
                    if t.stamp > s.max_stamp {
                        s.max_stamp = t.stamp;
                    }
                    if !is_consistent(t) {
                        s.violations += 1;
                        if s.first_bad.is_none() {
                            s.first_bad = Some(t);
                        }
                    }
                }
                // One final read after the writer is done: must equal the last write.
                let (t, _) = cell.load_counted();
                s.reads += 1;
                if t.stamp > s.max_stamp {
                    s.max_stamp = t.stamp;
                }
                if !is_consistent(t) {
                    s.violations += 1;
                    if s.first_bad.is_none() {
                        s.first_bad = Some(t);
                    }
                }
                s
            })
        })
        .collect();

    // Writer: a fixed budget of monotonically-stamped witness stores, as fast as
    // possible. Single writer => the parity invariant holds.
    let writer = {
        let cell = Arc::clone(&cell);
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let start = Instant::now();
            for stamp in 1..=WRITER_BUDGET {
                cell.store(snapshot_for(stamp));
            }
            let elapsed = start.elapsed();
            // Release so readers' final post-`done` load is guaranteed to observe
            // the writer's last store (belt-and-braces; the seqlock already orders
            // the payload).
            done.store(true, Ordering::Release);
            elapsed
        })
    };

    let writer_elapsed = writer.join().expect("writer thread panicked");
    let stats: Vec<ReaderStats> =
        reader_handles.into_iter().map(|h| h.join().expect("reader thread panicked")).collect();

    let total_reads: u64 = stats.iter().map(|s| s.reads).sum();
    let total_retries: u64 = stats.iter().map(|s| s.retries).sum();
    let total_violations: u64 = stats.iter().map(|s| s.violations).sum();
    let max_stamp = stats.iter().map(|s| s.max_stamp).max().unwrap_or(0);
    let first_bad = stats.iter().find_map(|s| s.first_bad);

    // Integer ratio (no floats in `sync`): retries observed per million reads.
    let retries_per_million =
        total_retries.saturating_mul(1_000_000).checked_div(total_reads).unwrap_or(0);
    println!(
        "stress_seqlock: readers={readers} writer_stores={WRITER_BUDGET} writer_elapsed={writer_elapsed:?} \
         total_reads={total_reads} total_retries={total_retries} \
         retries_per_million_reads={retries_per_million} max_stamp_seen={max_stamp} violations={total_violations}"
    );

    // (1) No torn snapshot ever escaped the version check.
    assert_eq!(total_violations, 0, "torn read(s) detected; first bad snapshot: {first_bad:?}");

    // (2) Writer-never-blocks (coarse): the writer ran its FULL fixed budget — the
    // loop completed `WRITER_BUDGET` stores regardless of reader count, so readers
    // did not stall the wait-free writer. (The `for 1..=WRITER_BUDGET` loop running
    // to completion before `done` is set is the witness; max_stamp confirms readers
    // observed writes up to near the end.)
    assert!(max_stamp > 0, "readers never observed a single write");
    assert!(max_stamp <= WRITER_BUDGET, "observed a stamp the writer never stored: {max_stamp}");

    // Corroboration that this was a real stress, not a trivial pass: the readers
    // performed millions of reads against a live writer.
    assert!(
        total_reads >= 1_000_000,
        "expected millions of reads under contention, got {total_reads}"
    );
}

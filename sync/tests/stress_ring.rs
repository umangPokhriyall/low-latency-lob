//! Real-thread stress for the SPMC broadcast ring (Phase 7 §5).
//!
//! loom (`tests/loom_ring.rs`) proves the memory-ordering MODEL exhaustively but
//! only for a tiny, bounded interleaving. This corroborates the proof on real
//! hardware with real `std` atomics across three scenarios, each `std`-threads-only,
//! integer accounting (no floats — `sync` bans `f64`/`f32`), bounded runtime:
//!
//!   1. **No-loss path** — capacity >= N, so the producer never wraps: K keeping-up
//!      consumers must each deliver EVERY position `0..N` exactly once, in order,
//!      untorn, with ZERO overruns. (Making `cap >= N` turns "consumers keep up"
//!      into a structural guarantee instead of a timing bet, so the zero-overrun
//!      assertion is deterministic, not flaky.)
//!   2. **Overrun-detection path** — small capacity + a throttled consumer that is
//!      first let start only after the producer has pulled a full burst ahead (so at
//!      least one overrun is GUARANTEED), against a full-tilt producer. Overruns are
//!      reported (never silent); the `skipped` counts account monotonically for the
//!      whole produced stream (`delivered + skipped == N`); every delivered item is
//!      self-consistent and strictly in order.
//!   3. **No-tear / no-dup under contention** — full-tilt producer + `K = max(2,
//!      available_parallelism()-1)` consumers; ZERO witness violations and ZERO
//!      duplicates across millions of deliveries; the producer completes its FULL
//!      budget regardless of K (the writer-never-blocks check).
//!
//! Consistency witness (same as loom §4): record at position `pos` has words
//! `rec[k] = pos*W + k`. A consumer recovers `pos = rec[0] / W` and checks every
//! word; a torn record (words from two generations) fails the check. Strict increase
//! of delivered positions catches duplicates and reordering.
#![cfg(not(loom))]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, available_parallelism};
use std::time::{Duration, Instant};
use sync::{Consumer, Recv, SpmcRing};

/// Payload width. W = 4 exercises tearing across multiple words per record.
const W: usize = 4;
/// Per-consumer watchdog: a drain that fails to terminate within this many seconds
/// panics rather than hanging CI (a correctness bug, not a timeout, is the intent).
const WATCHDOG: Duration = Duration::from_secs(60);

// Scenario 1 — no-loss. CAP >= N (power of two) => producer never wraps => no overrun.
const NOLOSS_N: u64 = 500_000;
const NOLOSS_CAP: usize = 1 << 19; // 524_288 >= NOLOSS_N

// Scenario 2 — overrun detection.
const OVR_N: u64 = 2_000_000;
const OVR_CAP: usize = 1 << 10; // 1024
const OVR_BURST: u64 = 4096; // 4 * OVR_CAP: producer gets > cap ahead before the consumer starts

// Scenario 3 — contention.
const CONT_N: u64 = 2_000_000;
const CONT_CAP: usize = 1 << 14; // 16_384

/// The witness record for a position.
fn witness(pos: u64) -> [u64; W] {
    core::array::from_fn(|k| pos * W as u64 + k as u64)
}

/// Recover the position a record claims (`rec[0] = pos*W`).
fn decode_pos(rec: &[u64; W]) -> u64 {
    rec[0] / W as u64
}

/// True iff every word matches the witness for its recovered position (untorn).
fn is_consistent(rec: &[u64; W]) -> bool {
    let pos = decode_pos(rec);
    rec.iter().enumerate().all(|(k, &v)| v == pos * W as u64 + k as u64)
}

/// Number of consumer threads for the contention scenario.
fn contention_consumers() -> usize {
    available_parallelism().map_or(2, |n| (n.get() - 1).max(2))
}

/// Spin until `flag` is set, yielding so we never wedge a single-core scheduler.
fn wait_until(flag: &AtomicBool) {
    while !flag.load(Ordering::Acquire) {
        thread::yield_now();
    }
}

// ---- Scenario 1: no-loss --------------------------------------------------------

#[derive(Debug, Default)]
struct NoLoss {
    delivered: u64,
    overruns: u64,
    violations: u64,
    first_bad: Option<(u64, [u64; W])>, // (expected pos, offending record)
}

/// Drain expecting EVERY position `0..n` in order, untorn, with no overrun.
fn drain_no_loss(mut c: Consumer<W>, n: u64, done: &AtomicBool) -> NoLoss {
    let mut s = NoLoss::default();
    let mut expected = 0u64;
    let deadline = Instant::now() + WATCHDOG;
    loop {
        match c.try_recv() {
            Recv::Item(rec) => {
                if !is_consistent(&rec) || decode_pos(&rec) != expected {
                    s.violations += 1;
                    if s.first_bad.is_none() {
                        s.first_bad = Some((expected, rec));
                    }
                }
                expected += 1;
                s.delivered += 1;
            }
            Recv::Overrun { .. } => s.overruns += 1, // never expected here
            Recv::Empty => {
                if done.load(Ordering::Acquire) && c.cursor() >= n {
                    break;
                }
                assert!(Instant::now() < deadline, "no-loss consumer watchdog fired");
                std::hint::spin_loop();
            }
        }
    }
    s
}

#[test]
fn no_loss_when_consumers_keep_up() {
    let consumers = contention_consumers();
    let (mut producer, handle) = SpmcRing::<W>::with_capacity(NOLOSS_CAP).split();
    let done = Arc::new(AtomicBool::new(false));

    // Create every consumer at cursor 0 BEFORE the producer pushes anything.
    let cons: Vec<Consumer<W>> = (0..consumers).map(|_| handle.consumer()).collect();
    let reader_handles: Vec<_> = cons
        .into_iter()
        .map(|c| {
            let done = Arc::clone(&done);
            thread::spawn(move || drain_no_loss(c, NOLOSS_N, &done))
        })
        .collect();

    let writer = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            for pos in 0..NOLOSS_N {
                producer.push(witness(pos));
            }
            done.store(true, Ordering::Release);
            producer.position()
        })
    };

    let pushed = writer.join().expect("producer panicked");
    let stats: Vec<NoLoss> =
        reader_handles.into_iter().map(|h| h.join().expect("consumer panicked")).collect();

    let total_delivered: u64 = stats.iter().map(|s| s.delivered).sum();
    let total_overruns: u64 = stats.iter().map(|s| s.overruns).sum();
    let total_violations: u64 = stats.iter().map(|s| s.violations).sum();
    let first_bad = stats.iter().find_map(|s| s.first_bad);
    println!(
        "no_loss: consumers={consumers} cap={NOLOSS_CAP} pushed={pushed} \
         delivered={total_delivered} overruns={total_overruns} violations={total_violations}"
    );

    assert_eq!(pushed, NOLOSS_N, "producer did not complete its budget");
    assert_eq!(total_overruns, 0, "no-loss path saw an overrun (cap >= N should prevent it)");
    assert_eq!(total_violations, 0, "torn / out-of-order delivery; first bad: {first_bad:?}");
    // Each consumer delivered every position exactly once.
    for s in &stats {
        assert_eq!(s.delivered, NOLOSS_N, "a consumer missed/duplicated positions: {s:?}");
    }
    assert_eq!(total_delivered, NOLOSS_N * consumers as u64);
}

// ---- Scenario 2: overrun detection ---------------------------------------------

#[derive(Debug, Default)]
struct Overrun {
    delivered: u64,
    overruns: u64,
    skipped_total: u64,
    violations: u64,
    order_errors: u64,
    first_bad: Option<(u64, [u64; W])>,
}

/// Drain a throttled consumer, accounting every produced position as delivered or
/// skipped, and checking each delivered item is self-consistent and in order.
fn drain_with_overruns(
    mut c: Consumer<W>,
    n: u64,
    burst_done: &AtomicBool,
    done: &AtomicBool,
) -> Overrun {
    let mut s = Overrun::default();
    let mut expected = 0u64; // = delivered + skipped_total at all times (the accounting invariant)
    let deadline = Instant::now() + WATCHDOG;

    // Start only after the producer is a full burst ahead => the first read is a
    // guaranteed overrun (cursor 0 but the oldest resident position is > 0).
    wait_until(burst_done);

    loop {
        match c.try_recv() {
            Recv::Item(rec) => {
                if !is_consistent(&rec) {
                    s.violations += 1;
                    if s.first_bad.is_none() {
                        s.first_bad = Some((expected, rec));
                    }
                } else if decode_pos(&rec) != expected {
                    s.order_errors += 1;
                    if s.first_bad.is_none() {
                        s.first_bad = Some((expected, rec));
                    }
                }
                expected += 1;
                s.delivered += 1;
                // Throttle: stay behind the full-tilt producer so overruns recur.
                for _ in 0..256 {
                    black_box(expected);
                }
            }
            Recv::Overrun { skipped } => {
                s.overruns += 1;
                s.skipped_total += skipped;
                expected += skipped; // cursor jumped to the oldest resident position
            }
            Recv::Empty => {
                if done.load(Ordering::Acquire) && c.cursor() >= n {
                    break;
                }
                assert!(Instant::now() < deadline, "overrun consumer watchdog fired");
                std::hint::spin_loop();
            }
        }
    }
    s
}

#[test]
fn overruns_are_detected_and_accounted() {
    let (mut producer, handle) = SpmcRing::<W>::with_capacity(OVR_CAP).split();
    let burst_done = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));

    let consumer = handle.consumer(); // cursor 0, created before any push
    let reader = {
        let burst_done = Arc::clone(&burst_done);
        let done = Arc::clone(&done);
        thread::spawn(move || drain_with_overruns(consumer, OVR_N, &burst_done, &done))
    };

    let writer = {
        let burst_done = Arc::clone(&burst_done);
        let done = Arc::clone(&done);
        thread::spawn(move || {
            for pos in 0..OVR_N {
                producer.push(witness(pos));
                if pos + 1 == OVR_BURST {
                    burst_done.store(true, Ordering::Release);
                }
            }
            // In case N < BURST (it is not here), make sure the consumer is released.
            burst_done.store(true, Ordering::Release);
            done.store(true, Ordering::Release);
            producer.position()
        })
    };

    let pushed = writer.join().expect("producer panicked");
    let s = reader.join().expect("consumer panicked");
    println!(
        "overrun: cap={OVR_CAP} pushed={pushed} delivered={} overruns={} skipped={} \
         violations={} order_errors={}",
        s.delivered, s.overruns, s.skipped_total, s.violations, s.order_errors
    );

    assert_eq!(pushed, OVR_N, "producer did not complete its budget");
    assert_eq!(s.violations, 0, "torn delivered item; first bad: {:?}", s.first_bad);
    assert_eq!(s.order_errors, 0, "out-of-order delivery; first bad: {:?}", s.first_bad);
    assert!(s.overruns > 0, "expected overruns under a throttled consumer, saw none");
    // The whole produced stream is accounted for, monotonically, with no silent loss.
    assert_eq!(
        s.delivered + s.skipped_total,
        OVR_N,
        "delivered ({}) + skipped ({}) must equal produced ({OVR_N})",
        s.delivered,
        s.skipped_total
    );
}

// ---- Scenario 3: no-tear / no-dup under contention ------------------------------

#[derive(Debug, Default)]
struct Contention {
    delivered: u64,
    overruns: u64,
    violations: u64,
    duplicates: u64,
    last_pos: Option<u64>,
    first_bad: Option<[u64; W]>,
}

/// Full-tilt drain: assert each item is untorn and delivered positions strictly
/// increase (so no duplicate and no reordering). Overruns (gaps) are allowed.
fn drain_contended(mut c: Consumer<W>, n: u64, done: &AtomicBool) -> Contention {
    let mut s = Contention::default();
    let deadline = Instant::now() + WATCHDOG;
    loop {
        match c.try_recv() {
            Recv::Item(rec) => {
                if is_consistent(&rec) {
                    let pos = decode_pos(&rec);
                    if let Some(last) = s.last_pos {
                        if pos <= last {
                            s.duplicates += 1; // duplicate or out-of-order
                        }
                    }
                    s.last_pos = Some(pos);
                } else {
                    s.violations += 1;
                    if s.first_bad.is_none() {
                        s.first_bad = Some(rec);
                    }
                }
                s.delivered += 1;
            }
            Recv::Overrun { .. } => s.overruns += 1,
            Recv::Empty => {
                if done.load(Ordering::Acquire) && c.cursor() >= n {
                    break;
                }
                assert!(Instant::now() < deadline, "contention consumer watchdog fired");
                std::hint::spin_loop();
            }
        }
    }
    s
}

#[test]
fn no_tear_no_dup_under_contention() {
    let consumers = contention_consumers();
    let (mut producer, handle) = SpmcRing::<W>::with_capacity(CONT_CAP).split();
    let done = Arc::new(AtomicBool::new(false));

    let cons: Vec<Consumer<W>> = (0..consumers).map(|_| handle.consumer()).collect();
    let reader_handles: Vec<_> = cons
        .into_iter()
        .map(|c| {
            let done = Arc::clone(&done);
            thread::spawn(move || drain_contended(c, CONT_N, &done))
        })
        .collect();

    let writer = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let start = Instant::now();
            for pos in 0..CONT_N {
                producer.push(witness(pos));
            }
            done.store(true, Ordering::Release);
            (producer.position(), start.elapsed())
        })
    };

    let (pushed, elapsed) = writer.join().expect("producer panicked");
    let stats: Vec<Contention> =
        reader_handles.into_iter().map(|h| h.join().expect("consumer panicked")).collect();

    let total_delivered: u64 = stats.iter().map(|s| s.delivered).sum();
    let total_overruns: u64 = stats.iter().map(|s| s.overruns).sum();
    let total_violations: u64 = stats.iter().map(|s| s.violations).sum();
    let total_dups: u64 = stats.iter().map(|s| s.duplicates).sum();
    let first_bad = stats.iter().find_map(|s| s.first_bad);
    println!(
        "contention: consumers={consumers} cap={CONT_CAP} pushed={pushed} elapsed={elapsed:?} \
         delivered={total_delivered} overruns={total_overruns} \
         violations={total_violations} duplicates={total_dups}"
    );

    // Writer-never-blocks: the producer ran its FULL budget regardless of K.
    assert_eq!(pushed, CONT_N, "producer did not complete its budget under {consumers} consumers");
    assert_eq!(total_violations, 0, "torn record under contention; first bad: {first_bad:?}");
    assert_eq!(total_dups, 0, "duplicate / out-of-order delivery under contention");
    // Corroboration that this was a real stress, not a trivial pass.
    assert!(
        total_delivered >= 1_000_000,
        "expected millions of deliveries under contention, got {total_delivered}"
    );
}

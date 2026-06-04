//! loom model-checked verification of the SPMC broadcast ring ordering (Phase 7 §4).
//!
//! loom exhaustively explores thread interleavings *and* the permitted
//! memory-ordering reorderings, so it proves the §3 ordering (P1–P4 / R0–R3) rather
//! than merely asserting it. The model is kept small (loom is exponential): one
//! producer pushing 3 records into a **capacity-2** ring (so an overrun is reachable
//! in-model), two consumers each draining a few `try_recv`s.
//!
//! Consistency witness: every record's words are a deterministic function of its
//! position — `rec[k] = pos * W + k`. A consumer that receives `Recv::Item(rec)`
//! recovers `pos = rec[0] / W` and asserts `rec[k] == pos*W + k` for all `k`
//! (self-consistent ⇒ untorn) and that successive *delivered* positions strictly
//! increase (a duplicate or out-of-order delivery fails this; an `Overrun { skipped }`
//! jump is allowed, after which the next delivered position is still > the last one,
//! because the resync target `oldest = w - capacity` exceeds the last position read
//! by a lapped consumer). A torn item fails the witness.
//!
//! Run (the preemption bound is REQUIRED — see below):
//! `RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=1 cargo test -p sync --test loom_ring --release`
//!
//! Why a bounded `LOOM_MAX_PREEMPTIONS`: like the seqlock readers, a ring consumer
//! that sees `Recv::Empty` or gets lapped retries through `loom::thread::yield_now`.
//! With TWO consumers both yielding, each yield looks like global progress to loom's
//! same-state spin detector, so it never prunes the consumer/consumer yield
//! ping-pong; an unbounded preemption budget lets that interleaving balloon the
//! per-path branch count without end. Bound 1 is sufficient AND has teeth (below): a
//! single preemption interleaves the producer's overwrite into the *middle* of a
//! consumer's read (the torn-read scenario the witness must survive), because the
//! producer's overwrite of an in-flight slot is itself one contiguous critical
//! section. The model verifies green in ~120s. Each consumer also caps its own
//! attempts (`MAX_ATTEMPTS`) so a drained consumer that only ever sees `Empty`
//! cannot spin forever.
//!
//! Teeth (verified, broken variants NOT committed): at this exact bound
//! (`LOOM_MAX_PREEMPTIONS=1`) loom finds a witness-violating torn record `[0, 5]`
//! (word 0 from generation 0, word 1 from generation 2) in <0.01s when either
//! load-bearing overwrite-guard is removed: dropping the `(R3)` `Acquire` fence in
//! `try_recv`, or dropping the `(P1)` mark-busy store + its `Release` fence in
//! `push`. Both guard the *overwrite* race (a consumer reading an old generation
//! while the producer laps it). Note (honest): weakening the `(P3)` publish store to
//! `Relaxed` is NOT caught here, and correctly so — a keeping-up consumer only reads
//! generation `g` after observing `write > g`, i.e. after the `(P4)` `Release` /
//! `(R0)` `Acquire` pair, which already establishes the word stores' visibility;
//! `(P3)` reinforces it but is not the sole edge for the reachable executions. The
//! bound is meaningful, not vacuous: the two overwrite-guard edges have teeth.
#![cfg(loom)]

use loom::thread;
use sync::{Consumer, Recv, SpmcRing};

/// The model's word width. W = 2 exercises tearing *across* words (a torn record
/// would mix `rec[0]` from one generation with `rec[1]` from another).
const W: usize = 2;
/// Ring slots. Capacity 2 makes overruns reachable with only 3 pushes.
const CAP: usize = 2;
/// Records the producer pushes. 3 > CAP ⇒ at least one consumer can be lapped.
const PUSHES: u64 = 3;
/// Per-consumer attempt cap. Kept small (loom is exponential in shared-memory ops):
/// a few attempts suffice for a consumer to deliver, hit `Empty`, or be lapped, and
/// for the producer to interleave its overwrite into the middle of a read.
const MAX_ATTEMPTS: usize = 4;

/// Witness payload for a position: word `k` is `pos * W + k`.
fn witness(pos: u64) -> [u64; W] {
    core::array::from_fn(|k| pos * W as u64 + k as u64)
}

/// Assert a delivered record is self-consistent (untorn) and recover its position.
fn check_and_pos(rec: [u64; W]) -> u64 {
    let pos = rec[0] / W as u64;
    for (k, &v) in rec.iter().enumerate() {
        assert_eq!(v, pos * W as u64 + k as u64, "torn record: {rec:?}");
    }
    pos
}

/// Drain a consumer up to `MAX_ATTEMPTS` times, asserting the witness on every
/// `Item` and strict monotonicity of delivered positions across `Item`s and
/// `Overrun` jumps. Returns nothing; any violation panics inside loom.
fn drain(mut c: Consumer<W>) {
    let mut last: Option<u64> = None;
    for _ in 0..MAX_ATTEMPTS {
        match c.try_recv() {
            Recv::Item(rec) => {
                let pos = check_and_pos(rec);
                if let Some(prev) = last {
                    assert!(pos > prev, "non-monotonic / duplicate delivery: {prev} then {pos}");
                }
                last = Some(pos);
            }
            Recv::Overrun { skipped } => {
                // A lapped consumer skipped `skipped` records; the next delivered
                // position will still exceed `last` (resync target > last read), so
                // monotonicity above continues to hold. `skipped` must be plausible.
                assert!(skipped < PUSHES, "implausible skip count: {skipped}");
            }
            Recv::Empty => {
                // Caught up (for now). Yield so loom schedules the producer/other
                // consumer instead of exploring this consumer spinning in place.
                thread::yield_now();
            }
        }
    }
}

#[test]
fn loom_no_torn_no_dup_with_overrun() {
    loom::model(|| {
        let (mut producer, handle) = SpmcRing::<W>::with_capacity(CAP).split();

        // Two independent consumers, both joining at the oldest resident position so
        // they attempt to read every record the producer emits (and can be lapped).
        // Each consumer holds its own Arc to the ring, so the handle can be dropped.
        let c1 = handle.consumer_from_oldest();
        let c2 = handle.consumer_from_oldest();
        drop(handle);

        let prod = thread::spawn(move || {
            for pos in 0..PUSHES {
                producer.push(witness(pos));
            }
        });
        let r1 = thread::spawn(move || drain(c1));
        let r2 = thread::spawn(move || drain(c2));

        prod.join().unwrap();
        r1.join().unwrap();
        r2.join().unwrap();
    });
}

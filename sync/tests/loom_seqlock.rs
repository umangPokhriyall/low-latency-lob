//! loom model-checked verification of the `SeqLock` memory ordering (Phase 6 §4).
//!
//! loom exhaustively explores thread interleavings *and* the permitted
//! memory-ordering reorderings, so it proves the §3 ordering rather than merely
//! asserting it. The model is kept small (loom is exponential): one writer doing
//! a single `store`, two readers each doing one `load`.
//!
//! Consistency witness: the writer only ever stores snapshots whose every field
//! is a deterministic function of `stamp` (`bid_px = stamp*4`, `bid_qty = +1`,
//! `ask_px = +2`, `ask_qty = +3`). A *consistent* snapshot satisfies those
//! relations; a *torn* snapshot (fields from two different stamps — here the
//! initial 0 and the writer's 1) violates them. Every returned snapshot is checked,
//! and the witness is NOT weakened to make the model fit.
//!
//! Run (the preemption bound is REQUIRED — see below):
//! `RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=1 cargo test -p sync --test loom_seqlock --release`
//!
//! Why `LOOM_MAX_PREEMPTIONS=1`: a seqlock reader's retry is a spin that only makes
//! progress when the writer runs. With TWO readers both spinning, each reader's
//! `yield_now` looks like global progress to loom's same-state spin detector, so it
//! never prunes the reader/reader yield ping-pong; once the preemption budget is
//! spent mid-spin the writer cannot be scheduled to break it, and the per-path
//! branch budget blows up (verified: preempt>=2 with two readers does not
//! terminate even at `LOOM_MAX_BRANCHES=50000`). Bound 1 is sufficient to interleave
//! the writer into the middle of a reader's snapshot (the torn-read scenario): it
//! verifies green in ~3s and has teeth (below). A one-writer/one-reader model runs
//! exhaustively at preempt=3 in <1s, corroborating that the bound, not the
//! ordering, is the constraint. The real-thread stress test (§5, Session 2)
//! corroborates on hardware with unbounded readers.
//!
//! Teeth (verified, broken variants NOT committed): at this exact bound,
//! `LOOM_MAX_PREEMPTIONS=1`, loom finds a witness-violating interleaving when the
//! ordering is deliberately broken — both dropping the `(R2)` Acquire fence in
//! `load` and weakening the `(W2)` closing store from `Release` to `Relaxed` are
//! caught (each in <0.01s). The bound is meaningful, not vacuous.
#![cfg(loom)]

use loom::sync::Arc;
use sync::{SeqLock, TopOfBook};

/// The witness: every field derived from `stamp`.
fn snapshot_for(stamp: u64) -> TopOfBook {
    let base = (stamp as i64) * 4;
    TopOfBook { bid_px: base, bid_qty: base + 1, ask_px: base + 2, ask_qty: base + 3, stamp }
}

/// Assert the witness relations and that `stamp` is one the writer actually stored.
fn assert_consistent(t: TopOfBook) {
    assert!(t.stamp <= 1, "stamp out of range: {t:?}");
    assert_eq!(t.bid_px, (t.stamp as i64) * 4, "stamp/payload mismatch (torn): {t:?}");
    assert_eq!(t.bid_qty, t.bid_px + 1, "torn snapshot: {t:?}");
    assert_eq!(t.ask_px, t.bid_px + 2, "torn snapshot: {t:?}");
    assert_eq!(t.ask_qty, t.bid_px + 3, "torn snapshot: {t:?}");
}

#[test]
fn loom_no_torn_reads() {
    loom::model(|| {
        let cell = Arc::new(SeqLock::new(snapshot_for(0)));

        let w = {
            let c = cell.clone();
            loom::thread::spawn(move || {
                c.store(snapshot_for(1));
            })
        };
        let r1 = {
            let c = cell.clone();
            loom::thread::spawn(move || {
                assert_consistent(c.load());
            })
        };
        let r2 = {
            let c = cell.clone();
            loom::thread::spawn(move || {
                assert_consistent(c.load());
            })
        };

        w.join().unwrap();
        r1.join().unwrap();
        r2.join().unwrap();

        // After all writes, the final state is the last consistent snapshot.
        assert_consistent(cell.load());
    });
}

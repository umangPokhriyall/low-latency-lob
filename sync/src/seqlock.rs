//! Single-writer / many-reader seqlock over a top-of-book snapshot.
//! Writer is WAIT-FREE (never blocked by readers). Readers are optimistic and
//! retry on a straddling write (a seqlock reader is NOT lock-free — a continuous
//! writer can starve a reader — but the writer's progress is never impeded).
//! Sound with NO unsafe: payload fields are atomics accessed Relaxed; the version
//! counter (Acquire/Release) carries all ordering; torn snapshots are detected by
//! the version check and discarded, never returned. See [`SeqLock`] for the
//! ordering proof.

// loom/std atomic switch (loom model-checks only the code that uses loom's atomics).
#[cfg(loom)]
use loom::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};
use Ordering::{Acquire, Relaxed, Release};

// The reader's retry is a spin waiting on the writer. Under loom the spin must
// be a yield point (`loom::thread::yield_now`) so the model checker switches to
// the writer instead of exploring the reader spinning forever in place — a raw
// `core::hint::spin_loop` is opaque to loom and the spin path never terminates,
// blowing the branch budget. The std build keeps the identical CPU
// `core::hint::spin_loop`. Aliased to one name so `load` reads the same.
#[cfg(loom)]
use loom::thread::yield_now as spin_loop;
#[cfg(not(loom))]
use core::hint::spin_loop;

/// The value a reader receives — plain, `Copy`, no atomics. `stamp` is a monotonic
/// write counter used for provenance and as the stress/loom consistency witness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TopOfBook {
    pub bid_px: i64,
    pub bid_qty: i64,
    pub ask_px: i64,
    pub ask_qty: i64,
    pub stamp: u64,
}

/// A single-writer / many-reader seqlock holding one [`TopOfBook`] snapshot.
///
/// # Protocol
/// A version counter `seq` is even when the cell is stable and odd while a write
/// is in progress. The writer increments to odd, writes the payload, then
/// increments to even. A reader snapshots `seq`, reads the payload, re-reads
/// `seq`, and accepts the snapshot only if `seq` was even and unchanged across the
/// read; otherwise a write straddled the read and the reader retries.
///
/// # Why no torn snapshot is ever returned
/// A write changes `seq` from an even `s` to odd `s+1` to even `s+2`. If any write
/// *starts* during the reader's payload loads, the reader's `s2` differs from `s1`
/// (it is now `>= s+1`), so the snapshot is discarded. If `s1 == s2` and even, no
/// write was in progress at `s1` and none started before `s2`, so every payload
/// load came from the single write that produced version `s1`. The payload
/// accesses are `Relaxed` *atomics* — race-free by construction — so even a
/// discarded straddling read is well-defined behaviour, never UB.
///
/// # Memory ordering pairings
/// `(W2)` Release on the closing even-store synchronizes-with `(R1)` Acquire on
/// the `s1` load, so a reader observing the new even version is guaranteed to see
/// that write's payload. `(W1)` orders the odd-marker before the payload writes;
/// `(R2)` orders the payload loads before the `s2` load.
///
/// # Progress guarantees (stated honestly)
/// The writer is **wait-free**: `store` is a bounded number of `Relaxed` stores,
/// one fence, and one `Release` store, and waits on no reader. Readers are **not**
/// lock-free: a continuously-storing writer can starve a reader into endless
/// retry. In the engine the writer stores at the (bounded) feed rate, so reads
/// converge.
///
/// # Single-writer contract
/// Exactly one thread may call [`store`](SeqLock::store) at a time. The `&self`
/// API lets the one writer and many readers share an `&SeqLock`; concurrent
/// `store` from multiple threads is unsupported and would corrupt the parity.
/// This is a documented invariant for Phase 6, not enforced by the type.
///
/// Cache-line aligned (`#[repr(align(64))]`) so an array of cells does not
/// false-share, and so the version and payload the single writer touches sit
/// together.
#[repr(align(64))]
#[derive(Debug)]
pub struct SeqLock {
    seq: AtomicU32, // even = stable, odd = write in progress
    bid_px: AtomicI64,
    bid_qty: AtomicI64,
    ask_px: AtomicI64,
    ask_qty: AtomicI64,
    stamp: AtomicU64,
}

impl SeqLock {
    /// Create a cell pre-loaded with `init`. `seq` starts at 0 (even = stable).
    #[must_use]
    pub fn new(init: TopOfBook) -> Self {
        Self {
            seq: AtomicU32::new(0), // even => stable, immediately readable
            bid_px: AtomicI64::new(init.bid_px),
            bid_qty: AtomicI64::new(init.bid_qty),
            ask_px: AtomicI64::new(init.ask_px),
            ask_qty: AtomicI64::new(init.ask_qty),
            stamp: AtomicU64::new(init.stamp),
        }
    }

    /// SINGLE-WRITER store. Wait-free; never blocked by readers.
    pub fn store(&self, t: TopOfBook) {
        let s = self.seq.load(Relaxed); // single writer => no other writer races this
        // Enter write: mark odd. Relaxed is enough for the value; the fence below
        // orders this BEFORE the payload writes so a reader cannot observe new
        // payload while seq still reads even.
        self.seq.store(s.wrapping_add(1), Relaxed);
        fence(Release); // (W1) odd-marker happens-before payload writes

        self.bid_px.store(t.bid_px, Relaxed); // payload: Relaxed; ordering is carried by seq
        self.bid_qty.store(t.bid_qty, Relaxed);
        self.ask_px.store(t.ask_px, Relaxed);
        self.ask_qty.store(t.ask_qty, Relaxed);
        self.stamp.store(t.stamp, Relaxed);

        // Leave write: mark even with RELEASE. (W2) This publishes all payload
        // stores: a reader whose Acquire-load observes this even value is
        // guaranteed to see these payload values.
        self.seq.store(s.wrapping_add(2), Release);
    }

    /// Many-reader load. Returns a snapshot from a single consistent write.
    #[must_use]
    pub fn load(&self) -> TopOfBook {
        loop {
            let s1 = self.seq.load(Acquire); // (R1) Acquire: pairs with (W2); see that write's payload
            if s1 & 1 == 0 {
                // even => no write in progress at s1; take an optimistic snapshot
                let t = TopOfBook {
                    // payload: Relaxed loads
                    bid_px: self.bid_px.load(Relaxed),
                    bid_qty: self.bid_qty.load(Relaxed),
                    ask_px: self.ask_px.load(Relaxed),
                    ask_qty: self.ask_qty.load(Relaxed),
                    stamp: self.stamp.load(Relaxed),
                };
                fence(Acquire); // (R2) order the payload loads BEFORE the s2 load,
                //                    so s2 cannot be reordered ahead of them
                let s2 = self.seq.load(Relaxed);
                if s1 == s2 {
                    // even and unchanged => no write straddled the read
                    return t;
                }
                // else: a write straddled the read => the snapshot may be torn; fall
                // through to retry.
            }
            // Retry: either s1 was odd (write in progress) or a write straddled the
            // read (s1 != s2). Spin once before re-reading. Every retry path passes
            // through this single yield point, which is what bounds the loom model
            // (the spin is a yield so loom switches to the writer instead of
            // exploring the reader spinning in place) and backs off a real CPU.
            spin_loop();
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn snap(stamp: u64) -> TopOfBook {
        TopOfBook { bid_px: 10, bid_qty: 20, ask_px: 11, ask_qty: 21, stamp }
    }

    #[test]
    fn new_then_load_returns_init() {
        let init = snap(7);
        assert_eq!(SeqLock::new(init).load(), init);
    }

    #[test]
    fn store_then_load_returns_stored() {
        let cell = SeqLock::new(TopOfBook::default());
        let x = snap(42);
        cell.store(x);
        assert_eq!(cell.load(), x);
    }

    #[test]
    fn round_trips_several_distinct_snapshots() {
        let cell = SeqLock::new(TopOfBook::default());
        let snapshots = [
            TopOfBook { bid_px: 1, bid_qty: 2, ask_px: 3, ask_qty: 4, stamp: 1 },
            TopOfBook { bid_px: -100, bid_qty: 0, ask_px: 100, ask_qty: 5, stamp: 2 },
            TopOfBook { bid_px: i64::MIN, bid_qty: i64::MAX, ask_px: 0, ask_qty: -1, stamp: u64::MAX },
        ];
        for x in snapshots {
            cell.store(x);
            assert_eq!(cell.load(), x);
        }
    }

    #[test]
    fn default_snapshot_is_all_zero() {
        let cell = SeqLock::new(TopOfBook::default());
        assert_eq!(cell.load(), TopOfBook { bid_px: 0, bid_qty: 0, ask_px: 0, ask_qty: 0, stamp: 0 });
    }
}

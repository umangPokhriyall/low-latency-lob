//! Single-producer / many-independent-consumer **broadcast** ring buffer.
//!
//! One writer streams a sequence of `[u64; W]` records; any number of consumers
//! each read the *whole* stream from their own private cursor. The producer is
//! **wait-free**: it overwrites `slot[p & mask]` unconditionally on wrap and never
//! inspects consumer state, so a slow consumer can never stall it. A consumer that
//! keeps within a ring-length of the producer reads every record exactly once, in
//! order, untorn; a consumer that falls more than `capacity` behind is lapped and
//! **detects** the overrun (it is told how many records it skipped) and resyncs to
//! the oldest still-resident record — never a silent loss, never a torn value.
//!
//! # Soundness — atomic words, NO `unsafe`
//! Each slot stores its payload as `[AtomicU64; W]` plus an `AtomicU64` stamp. The
//! producer writes the words with `Relaxed` stores under a two-phase stamp protocol
//! (mark-busy → write → publish); a consumer reads the words `Relaxed` under a
//! per-slot seqlock-style double-check keyed by the record's *position*. Because a
//! broadcast slot is read by many consumers *concurrently while the producer
//! overwrites it*, the slot is NOT under exclusive ownership — so the
//! `UnsafeCell`-plus-raw-copy shortcut from Vyukov's *queue* (where each slot is
//! producer-XOR-one-consumer) is a **data race = UB** here, even though the stamp
//! check would discard the torn value. Atomic-word accesses, by contrast, cannot
//! race: the discarded
//! straddling read is well-defined, never UB. The one `unsafe` design that would be
//! sound — gate the producer on the slowest consumer's cursor — makes the writer
//! block on the slowest consumer, breaking wait-freedom. We therefore choose the
//! sound atomic-word ring with no `unsafe`. On x86 a `Relaxed` load/store of an
//! aligned `u64` is a plain `mov`, so the atomic copy is not a bottleneck.
//!
//! # Why no torn record is ever returned (the proof — see [`Consumer::try_recv`])
//! A clean return requires the stamp to read exactly `cursor` *both* before the
//! word loads `(R1)` *and* after them `(R3)`/`s2`, with an `Acquire` fence between
//! the word loads and the `s2` load. If the producer began overwriting generation
//! `cursor` at any instant during the read, it first set the `WRITING` bit on the
//! stamp `(P1)` — ordered ahead of its word stores by the `(P1)` `Release` fence —
//! so the consumer's `s2` no longer equals `cursor` and the value is discarded.
//!
//! # No loss / no duplication for a keeping-up consumer
//! If `cursor` never trails `write` by more than `capacity`, `slot[cursor & mask]`
//! still holds generation `cursor` (`stamp == cursor`), so the consumer reads every
//! position once, in monotonic order (`cursor += 1` happens only on a clean read).
//!
//! # Overrun is detected, never silent
//! If the producer lapped the consumer, the slot's stamp is `> cursor` (or has the
//! `WRITING` bit set), caught at `(R1)`/`(R3)`; [`Consumer::resync`] reports the
//! `skipped` count and jumps the cursor to the oldest resident position.
//!
//! # Progress guarantees (stated honestly)
//! The producer is **wait-free**: [`SpmcRing::push`] is a bounded number of stores
//! plus one fence and never reads consumer state or loops. Consumers are **not**
//! lock-free: a perpetually-faster producer can force endless overruns. They always
//! make *defined* progress, though — every `try_recv` returns — and never block the
//! producer. This is a broadcast bus, not a "lock-free queue"; no such overclaim.
//!
//! # No false sharing
//! `Slot<W>` is `#[repr(align(64))]`, one slot per cache line, so the producer
//! writing `slot[i]` never false-shares with a consumer reading `slot[j ≠ i]`. The
//! producer's [`WritePos`] sits on its own line. A static assertion checks
//! `size_of::<Slot<W>>() % 64 == 0`; the Phase 7 benchmark corroborates with a flat
//! producer-throughput-vs-`K` curve.

// loom/std atomic switch — loom model-checks only the code built against its atomics.
#[cfg(loom)]
use loom::sync::Arc;
#[cfg(loom)]
use loom::sync::atomic::{AtomicU64, Ordering, fence};
#[cfg(not(loom))]
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::atomic::{AtomicU64, Ordering, fence};
use Ordering::{Acquire, Relaxed, Release};

/// High bit of a stamp: set while a slot is mid-overwrite (no real position
/// reaches `1 << 63`, so a `WRITING`-marked stamp never aliases a valid position).
const WRITING: u64 = 1 << 63;
/// Initial stamp of an untouched slot. `u64::MAX` is never a published position in
/// any realistic run, so it never aliases a real generation.
const EMPTY: u64 = u64::MAX;

/// One ring slot: a stamp (the position currently stored, `WRITING` bit set
/// mid-overwrite) and the `W`-word payload. `#[repr(align(64))]` puts each slot on
/// its own cache line so adjacent slots never false-share.
#[repr(align(64))]
#[derive(Debug)]
struct Slot<const W: usize> {
    stamp: AtomicU64,
    words: [AtomicU64; W],
}

impl<const W: usize> Slot<W> {
    /// `#[repr(align(64))]` forces `size_of` to a multiple of the 64-byte alignment;
    /// this static assertion is a regression guard that fails to compile if the
    /// `repr` is ever dropped. Forced to evaluate from [`SpmcRing::with_capacity`].
    const ALIGN_CHECK: () = assert!(size_of::<Slot<W>>() % 64 == 0);

    fn new() -> Self {
        Self {
            stamp: AtomicU64::new(EMPTY),
            words: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// The producer's monotonic write position, alone on its own cache line so the one
/// store the producer makes per push never false-shares with the payload slots.
#[repr(align(64))]
#[derive(Debug)]
struct WritePos {
    v: AtomicU64,
}

/// A single-producer / many-consumer broadcast ring. Construct with
/// [`with_capacity`](SpmcRing::with_capacity), then [`split`](SpmcRing::split) it
/// into the one [`Producer`] and a [`RingHandle`] that mints [`Consumer`]s.
#[derive(Debug)]
pub struct SpmcRing<const W: usize> {
    slots: Box<[Slot<W>]>, // len = capacity, a power of two
    mask: u64,             // capacity - 1
    write: WritePos,
}

impl<const W: usize> SpmcRing<W> {
    /// Create an empty ring with `cap` slots. `cap` must be a power of two
    /// (so `& mask` indexing is exact) and non-zero.
    ///
    /// # Panics
    /// Panics if `cap` is not a power of two (zero included).
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        assert!(cap.is_power_of_two(), "ring capacity must be a power of two");
        let () = Slot::<W>::ALIGN_CHECK; // force the static size assertion to evaluate
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(Slot::new());
        }
        Self {
            slots: slots.into_boxed_slice(),
            // cap is a power of two <= isize::MAX elements; cap-1 fits u64 exactly.
            #[allow(clippy::cast_possible_truncation)]
            mask: (cap - 1) as u64,
            write: WritePos { v: AtomicU64::new(0) },
        }
    }

    /// Number of slots (the lap distance): a consumer trailing the producer by more
    /// than this is lapped and will see an overrun.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Consume the ring into the single [`Producer`] and a [`RingHandle`]. The
    /// `Producer` is **not** `Clone`, enforcing the single-writer contract at the
    /// type level; the `RingHandle` (a shared `Arc`) hands out independent
    /// consumers.
    #[must_use]
    pub fn split(self) -> (Producer<W>, RingHandle<W>) {
        let ring = Arc::new(self);
        (Producer { ring: ring.clone() }, RingHandle { ring })
    }

    /// SINGLE-PRODUCER push. **Wait-free**; never blocks on consumers (overwrites on
    /// wrap). Called only through [`Producer::push`], which is the sole writer.
    fn push(&self, rec: [u64; W]) {
        let p = self.write.v.load(Relaxed); // single producer owns the write position
        #[allow(clippy::cast_possible_truncation)] // p & mask <= mask < cap, fits usize
        let slot = &self.slots[(p & self.mask) as usize];

        // (P1) Mark the slot busy at position `p` BEFORE overwriting any word. The
        //      Release fence orders this stamp store ahead of the word stores, so a
        //      consumer mid-read of the PRIOR generation observes the stamp change
        //      and detects the overwrite (its `s2` check fails -> overrun) rather
        //      than reading torn words.
        slot.stamp.store(p | WRITING, Relaxed);
        fence(Release);

        // (P2) Overwrite the payload (Relaxed atomics: no data race; published by P3).
        for (word, &r) in slot.words.iter().zip(rec.iter()) {
            word.store(r, Relaxed);
        }

        // (P3) Publish ready at position `p` (Release): a consumer whose Acquire load
        //      of the stamp sees `p` is guaranteed to see these word stores.
        slot.stamp.store(p, Release);

        // (P4) Advance the write position (Release): a consumer's Acquire load of
        //      `write` establishes that positions < the new value are published.
        self.write.v.store(p.wrapping_add(1), Release);
    }
}

/// The single writer. `Send + !Clone` — exactly one exists per ring, which is the
/// single-producer contract enforced by the type system. `push` takes `&mut self`,
/// so the producer cannot even be shared with itself across threads.
#[derive(Debug)]
pub struct Producer<const W: usize> {
    ring: Arc<SpmcRing<W>>,
}

impl<const W: usize> Producer<W> {
    /// Append `rec` at the next position. **Wait-free** — overwrites the oldest slot
    /// on wrap and never inspects or waits on any consumer.
    pub fn push(&mut self, rec: [u64; W]) {
        self.ring.push(rec);
    }

    /// The next position the producer will write (number of records pushed so far).
    #[must_use]
    pub fn position(&self) -> u64 {
        self.ring.write.v.load(Relaxed)
    }
}

/// A cloneable handle that mints independent [`Consumer`]s. Holds a shared `Arc` to
/// the ring; cloning the handle does NOT create a second producer (only [`split`]
/// makes the one `Producer`).
///
/// [`split`]: SpmcRing::split
#[derive(Debug, Clone)]
pub struct RingHandle<const W: usize> {
    ring: Arc<SpmcRing<W>>,
}

impl<const W: usize> RingHandle<W> {
    /// A consumer that joins *live*: its cursor starts at the producer's current
    /// write position, so it sees only records pushed from now on.
    #[must_use]
    pub fn consumer(&self) -> Consumer<W> {
        // Acquire pairs with (P4): everything published before this write position is
        // visible, though a live joiner will not read those older records.
        let cursor = self.ring.write.v.load(Acquire);
        Consumer { ring: self.ring.clone(), cursor }
    }

    /// A consumer that starts at the **oldest still-resident** record, so it replays
    /// as much history as the ring currently holds (up to `capacity` records).
    #[must_use]
    pub fn consumer_from_oldest(&self) -> Consumer<W> {
        let w = self.ring.write.v.load(Acquire);
        let oldest = w.saturating_sub(self.ring.capacity() as u64);
        Consumer { ring: self.ring.clone(), cursor: oldest }
    }
}

/// The outcome of one [`Consumer::try_recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recv<const W: usize> {
    /// A clean, untorn record at the consumer's current position.
    Item([u64; W]),
    /// The consumer has caught up to the producer; nothing new yet.
    Empty,
    /// The consumer was lapped: `skipped` records were overwritten before it could
    /// read them. The cursor has resynced to the oldest resident record; the next
    /// `try_recv` resumes there.
    Overrun { skipped: u64 },
}

/// One independent reader. Owns a private `cursor` (the next position it wants) and
/// shares the ring via `Arc`. Many consumers read the same broadcast stream without
/// coordinating; none can stall the producer.
#[derive(Debug)]
pub struct Consumer<const W: usize> {
    ring: Arc<SpmcRing<W>>,
    cursor: u64,
}

impl<const W: usize> Consumer<W> {
    /// Try to read the record at the current cursor. Returns [`Recv::Item`] on a
    /// clean read (and advances), [`Recv::Empty`] if caught up, or [`Recv::Overrun`]
    /// if lapped (and resyncs). Never blocks; never returns a torn value. The full
    /// ordering argument is in the module doc comment; each step is named below.
    pub fn try_recv(&mut self) -> Recv<W> {
        let w = self.ring.write.v.load(Acquire); // (R0) how far the producer has advanced
        if self.cursor >= w {
            return Recv::Empty; // caught up to the producer
        }

        #[allow(clippy::cast_possible_truncation)] // cursor & mask <= mask < cap, fits usize
        let slot = &self.ring.slots[(self.cursor & self.ring.mask) as usize];

        // (R1) Acquire pairs with (P3): if the stamp reads exactly `cursor`, the
        //      generation-`cursor` word stores are visible. Any other value means a
        //      WRITING overwrite is in progress or the slot already holds a later
        //      generation (we have been lapped). `s1 < cursor` cannot occur while
        //      `cursor < w`, since position `cursor` has been published.
        let s1 = slot.stamp.load(Acquire);
        if s1 != self.cursor {
            return self.resync();
        }

        // (R2) Read the payload (Relaxed atomics: race-free; consistency proven by the
        //      s2 re-check). A straddling overwrite cannot corrupt memory — only the
        //      logical value, which (R3) discards.
        let mut rec = [0u64; W];
        for (r, word) in rec.iter_mut().zip(slot.words.iter()) {
            *r = word.load(Relaxed);
        }

        // (R3) Acquire fence: order the word loads BEFORE the s2 stamp load, so s2
        //      cannot be hoisted above them. If the producer began overwriting
        //      generation `cursor` during (R2), it set the WRITING bit / bumped the
        //      stamp first ((P1), ordered ahead of its word writes by the P1 fence),
        //      so s2 != cursor and the (possibly torn) value is discarded.
        fence(Acquire);
        let s2 = slot.stamp.load(Relaxed);
        if s2 != self.cursor {
            return self.resync();
        }

        self.cursor += 1;
        Recv::Item(rec) // clean: every word is exactly generation `cursor`
    }

    /// Resync after detecting an overrun: jump the cursor to the oldest record still
    /// resident in the ring and report how many positions were skipped.
    ///
    /// Re-loads the write position FRESHLY (an Acquire load, not the `(R0)` snapshot
    /// from the caller). This matters under heavy contention: between `(R0)` and the
    /// `(R1)`/`(R3)` overwrite detection the producer can lap us by a full
    /// `capacity`, so the `(R0)` snapshot may already be `capacity` or more behind the
    /// true write position. Computing `oldest` from that stale value could place it
    /// *behind* our cursor, and `saturating_sub` would then report `skipped == 0`
    /// while the cursor jumped backward — re-delivering already-seen positions. A
    /// fresh load guarantees `oldest >= cursor` (reaching resync means the slot was
    /// overwritten to generation `>= cursor + capacity`, so the producer's current
    /// write position is `>= cursor + capacity`, hence `oldest = w - capacity >=
    /// cursor`): the cursor is monotonic and `skipped` is exact. Caught by the §5
    /// overrun-detection stress test; loom's tiny model could not reach a full-`cap`
    /// lap inside the `(R0)`→`(R1)` window.
    fn resync(&mut self) -> Recv<W> {
        let w = self.ring.write.v.load(Acquire); // fresh write position (see above)
        let oldest = w.saturating_sub(self.ring.capacity() as u64); // oldest resident position
        let skipped = oldest.saturating_sub(self.cursor);
        self.cursor = oldest;
        Recv::Overrun { skipped }
    }

    /// The next position this consumer will attempt to read.
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    /// Witness payload: every word is a deterministic function of the position, so a
    /// torn record (words from two generations) is detectable.
    fn witness<const W: usize>(pos: u64) -> [u64; W] {
        core::array::from_fn(|k| pos * W as u64 + k as u64)
    }

    fn check_witness<const W: usize>(pos: u64, rec: [u64; W]) {
        for (k, &v) in rec.iter().enumerate() {
            assert_eq!(v, pos * W as u64 + k as u64, "torn record at pos {pos}");
        }
    }

    // The §3.1 static assertion, surfaced as a test too: a slot is whole cache lines.
    const _: () = assert!(size_of::<Slot<1>>() % 64 == 0);
    const _: () = assert!(size_of::<Slot<4>>() % 64 == 0);
    const _: () = assert!(size_of::<Slot<8>>() % 64 == 0);

    #[test]
    fn slot_is_whole_cache_lines() {
        assert_eq!(size_of::<Slot<1>>() % 64, 0);
        assert_eq!(size_of::<Slot<4>>() % 64, 0);
        assert_eq!(size_of::<Slot<8>>() % 64, 0);
    }

    #[test]
    fn empty_when_caught_up() {
        let (_producer, handle) = SpmcRing::<2>::with_capacity(4).split();
        let mut c = handle.consumer();
        assert_eq!(c.try_recv(), Recv::Empty);
    }

    #[test]
    fn push_recv_round_trip() {
        let (mut producer, handle) = SpmcRing::<4>::with_capacity(8).split();
        let mut c = handle.consumer();
        for pos in 0..5 {
            producer.push(witness::<4>(pos));
        }
        for pos in 0..5 {
            match c.try_recv() {
                Recv::Item(rec) => check_witness(pos, rec),
                other => panic!("expected Item at pos {pos}, got {other:?}"),
            }
        }
        assert_eq!(c.try_recv(), Recv::Empty);
    }

    #[test]
    fn wrap_past_capacity_with_fast_consumer_no_overrun() {
        // A consumer that recvs after every push never trails by more than 1, so it
        // sees every record in order with zero overruns despite many wraps.
        let (mut producer, handle) = SpmcRing::<2>::with_capacity(4).split();
        let mut c = handle.consumer();
        for pos in 0..100 {
            producer.push(witness::<2>(pos));
            match c.try_recv() {
                Recv::Item(rec) => check_witness(pos, rec),
                other => panic!("expected Item at pos {pos}, got {other:?}"),
            }
        }
        assert_eq!(c.try_recv(), Recv::Empty);
    }

    #[test]
    fn stalled_consumer_detects_overrun_then_resumes_in_order() {
        // Single-thread simulation of a lapped consumer: push capacity + k records
        // while the consumer is stalled, then drain. Capacity 4, push 4 + 3 = 7.
        const CAP: usize = 4;
        const EXTRA: u64 = 3;
        let total = CAP as u64 + EXTRA;
        let (mut producer, handle) = SpmcRing::<2>::with_capacity(CAP).split();
        let mut c = handle.consumer(); // joins at cursor 0

        for pos in 0..total {
            producer.push(witness::<2>(pos));
        }

        // The consumer is at cursor 0 but positions 0..EXTRA were overwritten. The
        // oldest resident position is total - CAP = EXTRA.
        match c.try_recv() {
            Recv::Overrun { skipped } => assert_eq!(skipped, EXTRA, "exact skip count"),
            other => panic!("expected Overrun, got {other:?}"),
        }

        // After resync the remaining CAP records arrive in order.
        for pos in EXTRA..total {
            match c.try_recv() {
                Recv::Item(rec) => check_witness(pos, rec),
                other => panic!("expected Item at pos {pos}, got {other:?}"),
            }
        }
        assert_eq!(c.try_recv(), Recv::Empty);
    }

    #[test]
    fn live_consumer_starts_at_current_position() {
        let (mut producer, handle) = SpmcRing::<2>::with_capacity(8).split();
        producer.push(witness::<2>(0));
        producer.push(witness::<2>(1));
        // Joining live now: cursor starts at 2, so positions 0 and 1 are not seen.
        let mut c = handle.consumer();
        assert_eq!(c.cursor(), 2);
        producer.push(witness::<2>(2));
        match c.try_recv() {
            Recv::Item(rec) => check_witness(2, rec),
            other => panic!("expected Item at pos 2, got {other:?}"),
        }
    }

    #[test]
    fn from_oldest_replays_resident_history() {
        let (mut producer, handle) = SpmcRing::<2>::with_capacity(4).split();
        for pos in 0..6 {
            producer.push(witness::<2>(pos));
        }
        // Capacity 4, 6 pushed: oldest resident is position 2.
        let mut c = handle.consumer_from_oldest();
        assert_eq!(c.cursor(), 2);
        for pos in 2..6 {
            match c.try_recv() {
                Recv::Item(rec) => check_witness(pos, rec),
                other => panic!("expected Item at pos {pos}, got {other:?}"),
            }
        }
        assert_eq!(c.try_recv(), Recv::Empty);
    }

    #[test]
    fn two_consumers_are_independent() {
        let (mut producer, handle) = SpmcRing::<1>::with_capacity(8).split();
        let mut a = handle.consumer();
        let mut b = handle.consumer();
        for pos in 0..3 {
            producer.push(witness::<1>(pos));
        }
        // Drain a fully; b has not advanced.
        for pos in 0..3 {
            assert_eq!(a.try_recv(), Recv::Item(witness::<1>(pos)));
        }
        assert_eq!(a.try_recv(), Recv::Empty);
        assert_eq!(b.cursor(), 0);
        for pos in 0..3 {
            assert_eq!(b.try_recv(), Recv::Item(witness::<1>(pos)));
        }
    }
}


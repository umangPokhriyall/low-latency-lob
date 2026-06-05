//! `engine` — the assembled, pinned end-to-end hot path (Phase 8).
//!
//! This crate composes the verified parts into one pipeline and changes none of
//! them: the frozen [`book`] core, the loom-verified [`sync`] seqlock and SPMC
//! broadcast ring, and (for the demo / benchmark) the deterministic [`feed`]. It
//! provides only the *logic* of one producer step and one consumer step —
//!
//! ```text
//! producer:  book.apply(ev) -> seqlock.store(top) -> ring.push(pack(ev))
//! consumer:  ring.try_recv() -> { Item: unpack+light work | Overrun: seqlock resync | Empty }
//! ```
//!
//! Threads, pinning, pacing and timing live in `bench` (Benchmark 7), keeping
//! `engine` a clean, deterministic library. The book impl is selected by
//! monomorphization ([`Engine<B>`]), never `dyn`; the hot path inlines `apply`.
//! The crate is `#![forbid(unsafe_code)]`, preserving the workspace zero-`unsafe`
//! capstone — packing is explicit integer arithmetic (see [`pack`]).
#![forbid(unsafe_code)]

pub mod pack;

use std::sync::Arc;

use book::{BookEvent, OrderBook, Px, Qty};
use sync::{Consumer, Producer, Recv, RingHandle, SeqLock, SpmcRing, TopOfBook};

use crate::pack::{W, pack, unpack};

/// Type-level constructor for a pipeline driven by book impl `B`. Never holds a
/// value at runtime; [`Engine::new`] wires the parts and returns the producer side
/// plus a [`EngineHandle`] that mints independent consumers. Monomorphized over the
/// four frozen impls — no `dyn` anywhere in the hot path.
#[derive(Debug)]
pub struct Engine<B: OrderBook> {
    _b: core::marker::PhantomData<B>,
}

impl<B: OrderBook> Engine<B> {
    /// Wire `book(B::default)` + a fresh seqlock + an SPMC ring of `ring_capacity`
    /// slots; return the single [`EngineProducer`] and a cloneable [`EngineHandle`].
    ///
    /// # Panics
    /// Panics if `ring_capacity` is not a power of two (the ring's contract).
    #[must_use]
    // `Engine<B>` is a type-level namespace, never instantiated; `new` is the spec's
    // factory that splits the wired pipeline into its producer and consumer handles.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(ring_capacity: usize) -> (EngineProducer<B>, EngineHandle) {
        let top = Arc::new(SeqLock::new(TopOfBook::default()));
        let (out, ring) = SpmcRing::<W>::with_capacity(ring_capacity).split();
        let producer = EngineProducer { book: B::default(), top: Arc::clone(&top), out, stamp: 0 };
        (producer, EngineHandle { ring, top })
    }
}

/// The single producer step: apply to the frozen book, publish the new
/// top-of-book to the seqlock, broadcast the event through the ring. One thread
/// owns this (the seqlock and ring are single-writer); the bench pins it to a core.
#[derive(Debug)]
pub struct EngineProducer<B: OrderBook> {
    book: B,
    top: Arc<SeqLock>,
    out: Producer<W>,
    stamp: u64,
}

impl<B: OrderBook> EngineProducer<B> {
    /// One producer step. **Hot path; no allocation.** Applies `ev`, stores the
    /// resulting top-of-book under a fresh monotonic `stamp`, and pushes the packed
    /// event onto the broadcast ring.
    #[inline]
    pub fn process(&mut self, ev: &BookEvent) {
        self.book.apply(ev);
        let (bp, bq) = self.book.best_bid().unwrap_or((Px::ZERO, Qty::ZERO));
        let (ap, aq) = self.book.best_ask().unwrap_or((Px::ZERO, Qty::ZERO));
        self.stamp += 1;
        self.top.store(TopOfBook {
            bid_px: bp.ticks(),
            bid_qty: bq.lots(),
            ask_px: ap.ticks(),
            ask_qty: aq.lots(),
            stamp: self.stamp,
        });
        self.out.push(pack(ev));
    }

    /// The producer's frozen book (read-only) — for final-state consistency checks.
    #[must_use]
    pub fn book(&self) -> &B {
        &self.book
    }

    /// Number of events processed so far (the published seqlock `stamp`).
    #[must_use]
    pub fn stamp(&self) -> u64 {
        self.stamp
    }

    /// The current top-of-book as published to the seqlock (the producer's view).
    #[must_use]
    pub fn top_of_book(&self) -> TopOfBook {
        self.top.load()
    }
}

/// What one [`EngineConsumer::poll`] observed. (`BookEvent` is not `PartialEq`, so
/// neither is `Observed`; tests `match` on it rather than compare it.)
#[derive(Debug, Clone, Copy)]
pub enum Observed {
    /// A clean, untorn event delivered in order.
    Event(BookEvent),
    /// The producer lapped this consumer: `skipped` events were overwritten before
    /// it could read them. Derived state was rebased from the seqlock `snapshot`.
    Overrun { skipped: u64, snapshot: TopOfBook },
    /// Caught up to the producer; nothing new to read.
    Idle,
}

/// One independent consumer step: drain the ring, doing light derived work on each
/// event, and **resync from the seqlock on overrun**. Many of these read the same
/// broadcast stream from private cursors; none can stall the producer.
#[derive(Debug)]
pub struct EngineConsumer {
    inbox: Consumer<W>,
    top: Arc<SeqLock>,
    /// Events cleanly delivered to this consumer so far.
    pub seen: u64,
    /// Times this consumer was lapped and resynced from the seqlock.
    pub resyncs: u64,
    /// Last derived mid-price (from the most recent overrun resync snapshot).
    pub last_mid: i64,
}

impl EngineConsumer {
    /// One consumer step. On a clean record it unpacks, counts it, and returns the
    /// event for the caller's light work; on overrun it loads the consistent
    /// seqlock snapshot, rebases `last_mid`, and reports the skip; otherwise idle.
    ///
    /// # Panics
    /// Panics if a delivered record fails to unpack. The engine producer only emits
    /// valid records via [`pack`], so this is a broken-invariant assertion (torn or
    /// corrupt ring word), never an expected branch.
    #[inline]
    pub fn poll(&mut self) -> Observed {
        match self.inbox.try_recv() {
            Recv::Item(rec) => {
                let ev = unpack(&rec).expect("engine producer emits valid events");
                self.seen += 1;
                Observed::Event(ev)
            }
            Recv::Overrun { skipped } => {
                let snap = self.top.load(); // resync derived state from the consistent snapshot
                self.last_mid = (snap.bid_px + snap.ask_px) / 2;
                self.resyncs += 1;
                Observed::Overrun { skipped, snapshot: snap }
            }
            Recv::Empty => Observed::Idle,
        }
    }

    /// This consumer's current view of the live top-of-book (a seqlock load).
    #[must_use]
    pub fn snapshot(&self) -> TopOfBook {
        self.top.load()
    }

    /// The next ring position this consumer will attempt to read.
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.inbox.cursor()
    }
}

/// A cloneable handle that mints independent [`EngineConsumer`]s sharing the one
/// ring and seqlock. Cloning it does not create a second producer.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    ring: RingHandle<W>,
    top: Arc<SeqLock>,
}

impl EngineHandle {
    /// Mint a consumer that joins *live*: its cursor starts at the producer's
    /// current write position, so it sees only events pushed from now on.
    #[must_use]
    pub fn consumer(&self) -> EngineConsumer {
        EngineConsumer {
            inbox: self.ring.consumer(),
            top: Arc::clone(&self.top),
            seen: 0,
            resyncs: 0,
            last_mid: 0,
        }
    }
}

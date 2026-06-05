//! Phase 8 §4 — end-to-end correctness of the assembled pipeline.
//!
//! These tests compose the frozen `book`, the verified `sync` primitives, and a
//! deterministic `feed` corpus through the public `engine` API only. They prove the
//! four properties §4 requires:
//!   1. ordered, complete delivery for keeping-up consumers (single- and multi-thread),
//!   2. valid (untorn) seqlock snapshots at quiescence,
//!   3. overrun -> seqlock resync (resyncs > 0, post-resync delivery still ordered),
//!   4. final-state consistency vs an independently-computed replay.

use std::sync::Arc;
use std::thread;

use book::{BTreeBook, BookEvent, OrderBook, Px, Qty, Side};
use engine::{Engine, Observed};
use feed::synthetic::{GenConfig, Profile, generate};

/// A deterministic steady corpus. Steady advances `ts` by >= 1000 ns every flow
/// event, but the leading `Clear` + ladder share `start_ts`, so `ts` is monotonic
/// (non-decreasing), not strictly increasing; `seq` is dense (0..N) and is the
/// strict ordering witness.
fn corpus(events: usize) -> Vec<BookEvent> {
    generate(&GenConfig {
        profile: Profile::Steady,
        seed: 7,
        events,
        mid: Px(65_000),
        band: 64,
        max_qty: Qty(1_024),
        start_ts: 0,
    })
}

/// `seq` strictly increasing by exactly one (contiguous), `ts` non-decreasing.
fn assert_ordered(seqs: &[u64], tss: &[u64]) {
    for w in seqs.windows(2) {
        assert_eq!(w[1], w[0] + 1, "seq must be contiguous and strictly increasing");
    }
    for w in tss.windows(2) {
        assert!(w[1] >= w[0], "ts must be monotonic (non-decreasing)");
    }
}

/// §4.1 — single-threaded interleaved drive: a consumer on a ring larger than the
/// corpus is never lapped, so it observes every event exactly once, in order, untorn.
#[test]
fn ordered_complete_delivery_single_thread() {
    let evs = corpus(5_000);
    // Capacity > corpus length => the producer can never lap the consumer.
    let (mut producer, handle) = Engine::<BTreeBook>::new(8_192);
    let mut consumer = handle.consumer();

    let mut seqs = Vec::with_capacity(evs.len());
    let mut tss = Vec::with_capacity(evs.len());
    for ev in &evs {
        producer.process(ev);
        if let Observed::Event(got) = consumer.poll() {
            seqs.push(got.seq);
            tss.push(got.ts);
        }
    }
    while let Observed::Event(got) = consumer.poll() {
        seqs.push(got.seq);
        tss.push(got.ts);
    }

    assert_eq!(seqs.len(), evs.len(), "every event delivered exactly once");
    assert_eq!(seqs.first().copied(), Some(0));
    assert_ordered(&seqs, &tss);
    assert_eq!(consumer.resyncs, 0, "a roomy ring must never overrun");
}

/// §4.1 — small threaded run: a pinned-style producer thread streams the whole
/// corpus while several independent consumer threads drain concurrently. The ring
/// holds more than the whole corpus, so no consumer can be lapped regardless of
/// scheduling; each must still see every event in order.
#[test]
fn ordered_complete_delivery_threaded() {
    const K: usize = 3;
    let evs = Arc::new(corpus(20_000));
    let n = evs.len();

    // Power-of-two capacity strictly greater than the corpus => no possible overrun.
    let (mut producer, handle) = Engine::<BTreeBook>::new(1 << 16);

    // Mint all consumers BEFORE the producer starts, so each cursor begins at 0.
    let consumers: Vec<_> = (0..K).map(|_| handle.consumer()).collect();

    let workers: Vec<_> = consumers
        .into_iter()
        .map(|mut c| {
            thread::spawn(move || {
                let mut seqs = Vec::with_capacity(n);
                let mut tss = Vec::with_capacity(n);
                while seqs.len() < n {
                    match c.poll() {
                        Observed::Event(ev) => {
                            seqs.push(ev.seq);
                            tss.push(ev.ts);
                        }
                        Observed::Overrun { .. } => unreachable!("ring > corpus: no overrun"),
                        Observed::Idle => std::hint::spin_loop(),
                    }
                }
                (seqs, tss, c.resyncs)
            })
        })
        .collect();

    let prod = {
        let evs = Arc::clone(&evs);
        thread::spawn(move || {
            for ev in evs.iter() {
                producer.process(ev);
            }
        })
    };

    prod.join().expect("producer thread");
    for w in workers {
        let (seqs, tss, resyncs) = w.join().expect("consumer thread");
        assert_eq!(seqs.len(), n, "consumer saw every event");
        assert_eq!(seqs.first().copied(), Some(0));
        assert_ordered(&seqs, &tss);
        assert_eq!(resyncs, 0, "no overrun on an oversized ring");
    }
}

/// §4.2 — snapshot validity at quiescence: after draining, the consumer's seqlock
/// load equals the producer's published top-of-book, which equals the frozen book's
/// own best bid/ask. No torn snapshot is ever returned (the seqlock guarantees it).
#[test]
fn seqlock_snapshot_matches_book_at_quiescence() {
    let evs = corpus(5_000);
    let (mut producer, handle) = Engine::<BTreeBook>::new(8_192);
    let mut consumer = handle.consumer();

    for ev in &evs {
        producer.process(ev);
        let _ = consumer.poll();
    }
    while let Observed::Event(_) = consumer.poll() {}

    let snap = consumer.snapshot();
    assert_eq!(snap, producer.top_of_book(), "consumer and producer share one seqlock");
    assert_eq!(snap.stamp, evs.len() as u64, "stamp counts every processed event");

    let book = producer.book();
    let (bp, bq) = book.best_bid().unwrap_or((Px::ZERO, Qty::ZERO));
    let (ap, aq) = book.best_ask().unwrap_or((Px::ZERO, Qty::ZERO));
    assert_eq!((snap.bid_px, snap.bid_qty), (bp.ticks(), bq.lots()));
    assert_eq!((snap.ask_px, snap.ask_qty), (ap.ticks(), aq.lots()));
}

/// §4.3 — overrun -> resync: a stalled consumer on a tiny ring is lapped, detects
/// the overrun, resyncs from a valid seqlock snapshot, and resumes in order. The
/// resync count is > 0 and post-resync delivery is contiguous up to the last event.
#[test]
fn overrun_triggers_seqlock_resync_then_ordered() {
    const CAP: usize = 8;
    let evs = corpus(5_000);
    let (mut producer, handle) = Engine::<BTreeBook>::new(CAP);
    let mut consumer = handle.consumer(); // cursor 0

    // Stall the consumer: push the whole corpus first, lapping it many times over.
    for ev in &evs {
        producer.process(ev);
    }

    // Now drain. Every gap shows up as an Overrun that resyncs from the seqlock; the
    // clean events between resyncs must still arrive in strictly increasing order.
    let mut seqs = Vec::new();
    let mut tss = Vec::new();
    let mut overrun_snapshots = 0u64;
    loop {
        match consumer.poll() {
            Observed::Event(ev) => {
                seqs.push(ev.seq);
                tss.push(ev.ts);
            }
            Observed::Overrun { snapshot, .. } => {
                // The resync snapshot is a real, consistent top-of-book.
                assert!(snapshot.stamp > 0, "resync snapshot must be a published state");
                assert!(snapshot.ask_px >= snapshot.bid_px, "bid must not cross ask");
                overrun_snapshots += 1;
            }
            Observed::Idle => break,
        }
    }

    assert!(consumer.resyncs > 0, "a tiny ring must lap the stalled consumer");
    assert_eq!(consumer.resyncs, overrun_snapshots, "every overrun resynced");
    assert!(!seqs.is_empty(), "the consumer still delivers the resident tail");
    assert_ordered(&seqs, &tss);
    // The last resident record is the final event; delivery resumes correctly.
    assert_eq!(seqs.last().copied(), Some(evs.len() as u64 - 1), "tail reaches the end");

    // last_mid was derived from the most recent resync snapshot.
    let snap = producer.top_of_book();
    assert_eq!(consumer.last_mid, (snap.bid_px + snap.ask_px) / 2);
}

/// §4.4 — final-state consistency: after the producer applies the whole corpus, its
/// book's observable state matches an independently-computed reference replay.
#[test]
fn final_state_matches_independent_replay() {
    let evs = corpus(10_000);
    let (mut producer, _handle) = Engine::<BTreeBook>::new(8_192);
    for ev in &evs {
        producer.process(ev);
    }

    let mut reference = BTreeBook::default();
    for ev in &evs {
        reference.apply(ev);
    }

    let book = producer.book();
    assert_eq!(book.best_bid(), reference.best_bid(), "best_bid diverged");
    assert_eq!(book.best_ask(), reference.best_ask(), "best_ask diverged");
    for side in [Side::Bid, Side::Ask] {
        assert_eq!(book.depth(side), reference.depth(side), "depth diverged");
    }
    assert_eq!(book.last_trade(), reference.last_trade(), "last_trade diverged");
    assert_eq!(producer.stamp(), evs.len() as u64, "one stamp per processed event");
}

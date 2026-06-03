//! The differential correctness oracle — the load-bearing artifact of Phase 2.
//!
//! This is an INTEGRATION test: it exercises only `book`'s public API, so it
//! validates the `OrderBook` contract at exactly the surface a consumer sees.
//! The contract it enforces (§6.1): for ANY sequence of `BookEvent`s, `BTreeBook`,
//! `SortedVecBook`, and `RevVecBook` produce IDENTICAL observable state after
//! every event. Internal representation may differ; observable behaviour may not.
//! A divergence is a correctness bug in at least one impl and BLOCKS the freeze.
//!
//! Every failure prints `seed` + event index + the diverging field, so any failure
//! reproduces from a single line. No wall-clock, no threads, no unseeded randomness:
//! the generator is a hand-rolled seeded `SplitMix64` (zero third-party deps).

mod common;

use book::{BTreeBook, BookEvent, OrderBook, Px, Qty, RevVecBook, Side, SortedVecBook};

// ---------------------------------------------------------------------------
// 6.2 The observable snapshot
// ---------------------------------------------------------------------------

/// The full public surface of a book at one instant. Comparing the COMPLETE
/// ladder (not just the top level) is deliberate: it catches an impl with the
/// right set of levels in the wrong order, and a `top_n` that mis-orders/-counts.
#[derive(PartialEq, Eq, Debug)]
struct Obs {
    best_bid: Option<(Px, Qty)>,
    best_ask: Option<(Px, Qty)>,
    depth_bid: usize,
    depth_ask: usize,
    bids: Vec<(Px, Qty)>, // FULL ladder, best-first
    asks: Vec<(Px, Qty)>, // FULL ladder, best-first
    last_trade: Option<(Px, Qty, Side)>,
}

fn observe<B: OrderBook>(b: &B) -> Obs {
    let (db, da) = (b.depth(Side::Bid), b.depth(Side::Ask));
    let mut bids = vec![(Px::ZERO, Qty::ZERO); db];
    let mut asks = vec![(Px::ZERO, Qty::ZERO); da];
    let nb = b.top_n(Side::Bid, &mut bids);
    let na = b.top_n(Side::Ask, &mut asks);
    assert_eq!(nb, db, "top_n(Bid)={nb} but depth(Bid)={db}");
    assert_eq!(na, da, "top_n(Ask)={na} but depth(Ask)={da}");
    Obs {
        best_bid: b.best_bid(),
        best_ask: b.best_ask(),
        depth_bid: db,
        depth_ask: da,
        bids,
        asks,
        last_trade: b.last_trade(),
    }
}

// ---------------------------------------------------------------------------
// 6.3 Deterministic randomness (hand-rolled, zero deps)
// ---------------------------------------------------------------------------

struct SplitMix64(u64);
impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n // modulo bias is fine for a fuzzer
    }
}

// ---------------------------------------------------------------------------
// 6.4 The generator — biased toward the hard cases
// ---------------------------------------------------------------------------

const PRICE_BASE: i64 = 10_000;
const PRICE_BAND: i64 = 64; // prices in [BASE-BAND, BASE+BAND]
const MAX_QTY: i64 = 50;

/// A price drawn uniformly from the narrow band `[BASE-BAND, BASE+BAND)`, biasing
/// the fuzzer toward same-price collisions, updates, and removals.
fn rand_px(rng: &mut SplitMix64) -> Px {
    let band = u64::try_from(2 * PRICE_BAND).unwrap();
    let off = i64::try_from(rng.below(band)).unwrap();
    Px(PRICE_BASE - PRICE_BAND + off)
}

/// A quantity in `[lo, hi]` (inclusive). `lo=0` makes `qty==0` (removal) reachable.
fn rand_qty(rng: &mut SplitMix64, lo: i64, hi: i64) -> Qty {
    let span = u64::try_from(hi - lo + 1).unwrap();
    Qty(lo + i64::try_from(rng.below(span)).unwrap())
}

fn gen_event(rng: &mut SplitMix64, seq: u64) -> BookEvent {
    let roll = rng.below(1000);
    if roll < 5 {
        // 0.5% Clear
        BookEvent::clear(seq, seq)
    } else if roll < 55 {
        // 5% Trade (must not touch ladders); qty in [1, MAX_QTY]
        let side = if rng.below(2) == 0 { Side::Bid } else { Side::Ask };
        BookEvent::trade(seq, seq, side, rand_px(rng), rand_qty(rng, 1, MAX_QTY))
    } else {
        // ~94.5% Level; qty in [0, MAX_QTY] so qty==0 removes
        let side = if rng.below(2) == 0 { Side::Bid } else { Side::Ask };
        BookEvent::level(seq, seq, side, rand_px(rng), rand_qty(rng, 0, MAX_QTY))
    }
}

// ---------------------------------------------------------------------------
// 6.5 Agreement assertion + driving helpers
// ---------------------------------------------------------------------------

fn assert_agree(seed: u64, k: u64, a: &BTreeBook, b: &SortedVecBook, c: &RevVecBook) {
    let (oa, ob, oc) = (observe(a), observe(b), observe(c));
    assert_eq!(oa, ob, "BTree vs SortedVec diverged at seed={seed} k={k}");
    assert_eq!(oa, oc, "BTree vs RevVec   diverged at seed={seed} k={k}");
}

/// Drive all three impls through `events`, asserting full agreement after EVERY
/// event. `tag` identifies the scenario in any failure message. Used by every
/// non-random test, where exhaustive per-event checking is cheap.
fn drive_and_check(tag: u64, events: &[BookEvent]) {
    let mut a = BTreeBook::default();
    let mut b = SortedVecBook::default();
    let mut c = RevVecBook::default();
    for (k, ev) in events.iter().enumerate() {
        a.apply(ev);
        b.apply(ev);
        c.apply(ev);
        assert_agree(tag, k as u64, &a, &b, &c);
    }
}

// ---------------------------------------------------------------------------
// Required tests
// ---------------------------------------------------------------------------

// 1. The exact Phase-1 hand-verified scenario, replayed across all three impls.
#[test]
fn oracle_shared_scenario() {
    drive_and_check(0, &common::shared_scenario());
}

// 2. 8 seeds × 50_000 generated events. Cheap per-event best_bid/best_ask check;
//    full ladder agreement every 64 events and at the end. On failure the printed
//    seed + index reproduce it exactly. `BOOK_ORACLE_ITERS` overrides the count.
#[test]
fn oracle_randomized() {
    const SEEDS: [u64; 8] = [1, 2, 3, 5, 8, 13, 21, 0xDEAD_BEEF];
    let iters: u64 = std::env::var("BOOK_ORACLE_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);

    for &seed in &SEEDS {
        let mut rng = SplitMix64::new(seed);
        let mut a = BTreeBook::default();
        let mut b = SortedVecBook::default();
        let mut c = RevVecBook::default();
        for k in 0..iters {
            let ev = gen_event(&mut rng, k);
            a.apply(&ev);
            b.apply(&ev);
            c.apply(&ev);
            // Cheap per-event check on the hot read path.
            assert_eq!(a.best_bid(), b.best_bid(), "best_bid BTree/Sorted seed={seed} k={k}");
            assert_eq!(a.best_bid(), c.best_bid(), "best_bid BTree/Rev    seed={seed} k={k}");
            assert_eq!(a.best_ask(), b.best_ask(), "best_ask BTree/Sorted seed={seed} k={k}");
            assert_eq!(a.best_ask(), c.best_ask(), "best_ask BTree/Rev    seed={seed} k={k}");
            // Full ladder agreement periodically (exhaustive every-event would be slow).
            if k.is_multiple_of(64) {
                assert_agree(seed, k, &a, &b, &c);
            }
        }
        assert_agree(seed, iters, &a, &b, &c);
    }
}

// 3. Negative and extreme prices on both sides: ordering must be integer-correct
//    across the whole i64 range.
#[test]
fn oracle_negative_and_extreme_prices() {
    let prices = [Px(-5), Px(0), Px(i64::MAX - 1), Px(i64::MIN + 1)];
    let mut events = Vec::new();
    let mut seq = 0u64;
    for &px in &prices {
        for side in [Side::Bid, Side::Ask] {
            events.push(BookEvent::level(seq, seq, side, px, Qty(1 + i64::try_from(seq % 7).unwrap())));
            seq += 1;
        }
    }
    // Then remove a couple of the extremes to exercise removal at the boundaries.
    events.push(BookEvent::level(seq, seq, Side::Bid, Px(i64::MIN + 1), Qty(0)));
    seq += 1;
    events.push(BookEvent::level(seq, seq, Side::Ask, Px(i64::MAX - 1), Qty(0)));
    drive_and_check(3, &events);
}

// 4. Crossed book: bids driven above asks. A transient crossing is legal for a
//    dumb container — it must STORE crossed state, not police it.
#[test]
fn oracle_crossed_book() {
    let events = [
        BookEvent::level(1, 1, Side::Ask, Px(100), Qty(3)),
        BookEvent::level(2, 2, Side::Ask, Px(101), Qty(4)),
        // Bid above the best ask: crossed.
        BookEvent::level(3, 3, Side::Bid, Px(105), Qty(2)),
        BookEvent::level(4, 4, Side::Bid, Px(102), Qty(1)),
        // More asks below the best bid: still crossed, deeper.
        BookEvent::level(5, 5, Side::Ask, Px(99), Qty(5)),
        BookEvent::level(6, 6, Side::Bid, Px(110), Qty(7)),
    ];
    drive_and_check(4, &events);
}

// 5. Remove-absent is a no-op: qty=0 on an empty book and on absent prices.
//    Depths stay correct; all three agree.
#[test]
fn oracle_remove_absent_is_noop() {
    let events = [
        // qty=0 on a wholly empty book.
        BookEvent::level(1, 1, Side::Bid, Px(100), Qty(0)),
        BookEvent::level(2, 2, Side::Ask, Px(200), Qty(0)),
        // Build a small ladder.
        BookEvent::level(3, 3, Side::Bid, Px(100), Qty(5)),
        BookEvent::level(4, 4, Side::Bid, Px(99), Qty(3)),
        BookEvent::level(5, 5, Side::Ask, Px(101), Qty(4)),
        // Remove prices that are not present (between, below, above).
        BookEvent::level(6, 6, Side::Bid, Px(98), Qty(0)),
        BookEvent::level(7, 7, Side::Bid, Px(50), Qty(0)),
        BookEvent::level(8, 8, Side::Ask, Px(500), Qty(0)),
    ];
    drive_and_check(5, &events);
}

// 6. Clear-then-rebuild: build one ladder, Clear, rebuild a DIFFERENT ladder.
#[test]
fn oracle_clear_then_rebuild() {
    let events = [
        BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
        BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
        BookEvent::level(3, 3, Side::Ask, Px(101), Qty(4)),
        BookEvent::trade(4, 4, Side::Bid, Px(101), Qty(2)),
        BookEvent::clear(5, 5),
        // A different ladder, including a fresh trade.
        BookEvent::level(6, 6, Side::Bid, Px(200), Qty(8)),
        BookEvent::level(7, 7, Side::Ask, Px(205), Qty(9)),
        BookEvent::level(8, 8, Side::Ask, Px(203), Qty(1)),
        BookEvent::trade(9, 9, Side::Ask, Px(203), Qty(1)),
    ];
    drive_and_check(6, &events);
}

// 7. Reallocation churn: insert enough distinct levels to force several Vec
//    growths, then remove a strided subset. All three must agree throughout.
#[test]
fn oracle_realloc_churn() {
    const N: i64 = 600; // 16 B/level => well past one 4 KiB page
    let mut events = Vec::new();
    let mut seq = 0u64;
    // Interleave both sides, prices spread out so they are all distinct levels.
    for k in 0..N {
        events.push(BookEvent::level(seq, seq, Side::Bid, Px(10_000 - k), Qty(k + 1)));
        seq += 1;
        events.push(BookEvent::level(seq, seq, Side::Ask, Px(10_001 + k), Qty(k + 1)));
        seq += 1;
    }
    // Remove every third level on each side (a strided subset forcing memmoves).
    for k in (0..N).step_by(3) {
        events.push(BookEvent::level(seq, seq, Side::Bid, Px(10_000 - k), Qty(0)));
        seq += 1;
        events.push(BookEvent::level(seq, seq, Side::Ask, Px(10_001 + k), Qty(0)));
        seq += 1;
    }
    drive_and_check(7, &events);
}

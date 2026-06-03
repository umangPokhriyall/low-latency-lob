// FROZEN — book v1 (Phase 2). Do not modify.
// The `book` crate is the sans-IO core. New order-book implementations are
// ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
// satisfy the frozen `OrderBook` trait without changing this file. If a later
// phase appears to need a change here, the design is wrong — STOP and ask.
// See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.

//! `RevVecBook` — price levels in a contiguous `Vec`, stored BEST-FIRST and
//! located by linear scan from index 0. The cache/branch-friendly arm of the
//! Phase 4 shootout: hot (top-of-book) updates scan 1–2 cache lines with a
//! loop-predictable branch and a sequential access pattern; best access is O(1).
//! INVARIANTS after every `apply`: bids strictly DESCENDING, asks strictly
//! ASCENDING, no duplicate prices, best at index 0 on both sides.

use crate::{BookEvent, EventKind, OrderBook, Px, Qty, Side};

#[derive(Default, Debug)]
pub struct RevVecBook {
    bids: Vec<(Px, Qty)>, // strictly descending; best = [0]
    asks: Vec<(Px, Qty)>, // strictly ascending;  best = [0]
    last_trade: Option<(Px, Qty, Side)>,
}

/// `ascending` = true for asks (lowest-first), false for bids (highest-first).
/// Scan from the best end to the first slot where `px` belongs.
fn update_best_first(vec: &mut Vec<(Px, Qty)>, px: Px, qty: Qty, ascending: bool) {
    let at = vec
        .iter()
        .position(|&(p, _)| if ascending { p >= px } else { p <= px });
    match at {
        Some(i) if vec[i].0 == px => {
            if qty.is_zero() {
                vec.remove(i);
            } else {
                vec[i].1 = qty;
            }
        }
        Some(i) => {
            if !qty.is_zero() {
                vec.insert(i, (px, qty));
            }
        }
        None => {
            if !qty.is_zero() {
                vec.push((px, qty));
            }
        }
    }
}

impl OrderBook for RevVecBook {
    #[inline]
    fn apply(&mut self, ev: &BookEvent) {
        match ev.kind {
            EventKind::Level => match ev.side {
                Side::Bid => update_best_first(&mut self.bids, ev.px, ev.qty, false),
                Side::Ask => update_best_first(&mut self.asks, ev.px, ev.qty, true),
            },
            EventKind::Trade => self.last_trade = Some((ev.px, ev.qty, ev.side)),
            EventKind::Clear => {
                self.bids.clear();
                self.asks.clear();
                self.last_trade = None;
            }
        }
    }

    #[inline]
    fn best_bid(&self) -> Option<(Px, Qty)> {
        self.bids.first().copied()
    }

    #[inline]
    fn best_ask(&self) -> Option<(Px, Qty)> {
        self.asks.first().copied()
    }

    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize {
        let src = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        let mut n = 0;
        for (slot, lvl) in out.iter_mut().zip(src.iter()) {
            *slot = *lvl;
            n += 1;
        }
        n
    }

    #[inline]
    fn depth(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    #[inline]
    fn last_trade(&self) -> Option<(Px, Qty, Side)> {
        self.last_trade
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a fresh `RevVecBook` through an event slice and return it.
    fn run(events: &[BookEvent]) -> RevVecBook {
        let mut b = RevVecBook::default();
        for ev in events {
            b.apply(ev);
        }
        b
    }

    /// The full best-first ladder of `side`, read through the public `top_n`
    /// surface. For `RevVecBook` this is a straight forward copy of storage.
    fn ladder(b: &RevVecBook, side: Side) -> Vec<(Px, Qty)> {
        let mut out = vec![(Px::ZERO, Qty::ZERO); b.depth(side)];
        let n = b.top_n(side, &mut out);
        assert_eq!(n, b.depth(side), "top_n count must equal depth");
        out
    }

    /// Assert a side's internal storage is strictly descending, duplicate-free.
    fn assert_strictly_descending(vec: &[(Px, Qty)]) {
        for w in vec.windows(2) {
            assert!(w[0].0 > w[1].0, "not strictly descending / has duplicate: {vec:?}");
        }
    }

    /// Assert a side's internal storage is strictly ascending, duplicate-free.
    fn assert_strictly_ascending(vec: &[(Px, Qty)]) {
        for w in vec.windows(2) {
            assert!(w[0].0 < w[1].0, "not strictly ascending / has duplicate: {vec:?}");
        }
    }

    // 1. Best is always at index 0 on both sides after churn.
    #[test]
    fn best_is_at_index_zero_after_churn() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(98), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(102), Qty(7)),
            BookEvent::level(4, 4, Side::Bid, Px(100), Qty(9)), // update in place
            BookEvent::level(5, 5, Side::Bid, Px(98), Qty(0)),  // remove
            BookEvent::level(6, 6, Side::Bid, Px(101), Qty(1)),
            BookEvent::level(7, 7, Side::Ask, Px(110), Qty(4)),
            BookEvent::level(8, 8, Side::Ask, Px(105), Qty(2)),
            BookEvent::level(9, 9, Side::Ask, Px(108), Qty(6)),
            BookEvent::level(10, 10, Side::Ask, Px(105), Qty(0)), // remove
        ]);
        // Best lives at storage index 0 on both sides; `best_*` reads `[0]`.
        assert_eq!(b.bids.first().copied(), b.best_bid());
        assert_eq!(b.asks.first().copied(), b.best_ask());
        assert_eq!(b.best_bid(), Some((Px(102), Qty(7)))); // highest bid
        assert_eq!(b.best_ask(), Some((Px(108), Qty(6)))); // lowest ask
        assert_eq!(b.depth(Side::Bid), 3); // 102,101,100
        assert_eq!(b.depth(Side::Ask), 2); // 108,110
    }

    // 2. Descending (bids) / ascending (asks) invariants after by-hand churn,
    //    and the best-first ladder reads correctly through top_n.
    #[test]
    fn directional_invariants_hold_after_churn() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(99), Qty(1)),
            BookEvent::level(2, 2, Side::Bid, Px(103), Qty(2)),
            BookEvent::level(3, 3, Side::Bid, Px(97), Qty(3)),
            BookEvent::level(4, 4, Side::Bid, Px(101), Qty(4)),
            BookEvent::level(5, 5, Side::Bid, Px(95), Qty(5)),
        ]);
        assert_strictly_descending(&b.bids);
        // Bids are read best-first (highest price first) — a plain forward copy.
        assert_eq!(
            ladder(&b, Side::Bid),
            vec![
                (Px(103), Qty(2)),
                (Px(101), Qty(4)),
                (Px(99), Qty(1)),
                (Px(97), Qty(3)),
                (Px(95), Qty(5)),
            ]
        );

        let a = run(&[
            BookEvent::level(1, 1, Side::Ask, Px(110), Qty(1)),
            BookEvent::level(2, 2, Side::Ask, Px(104), Qty(2)),
            BookEvent::level(3, 3, Side::Ask, Px(108), Qty(3)),
            BookEvent::level(4, 4, Side::Ask, Px(102), Qty(4)),
        ]);
        assert_strictly_ascending(&a.asks);
        // Asks are read best-first (lowest price first) — a plain forward copy.
        assert_eq!(
            ladder(&a, Side::Ask),
            vec![
                (Px(102), Qty(4)),
                (Px(104), Qty(2)),
                (Px(108), Qty(3)),
                (Px(110), Qty(1)),
            ]
        );
    }

    // 3. New-best, mid-ladder, and new-worst insertions land at the correct
    //    index — the §5 worked insertion check, as assertions on storage.
    //    Bids `[101,100,99]` (descending) is the fixture for all three cases.
    #[test]
    fn worked_insertion_check_bids() {
        let base = [
            BookEvent::level(1, 1, Side::Bid, Px(101), Qty(1)),
            BookEvent::level(2, 2, Side::Bid, Px(100), Qty(2)),
            BookEvent::level(3, 3, Side::Bid, Px(99), Qty(3)),
        ];

        // Mid-ladder, same price: position(p<=100) = index 1, vec[1].0 == 100
        // => update in place. Order unchanged.
        let mut b = run(&base);
        b.apply(&BookEvent::level(4, 4, Side::Bid, Px(100), Qty(9)));
        assert_eq!(
            b.bids,
            vec![(Px(101), Qty(1)), (Px(100), Qty(9)), (Px(99), Qty(3))]
        );
        assert_strictly_descending(&b.bids);

        // New best: position(p<=102) = 0, vec[0].0 != 102 => insert at 0.
        let mut b = run(&base);
        b.apply(&BookEvent::level(4, 4, Side::Bid, Px(102), Qty(7)));
        assert_eq!(
            b.bids,
            vec![(Px(102), Qty(7)), (Px(101), Qty(1)), (Px(100), Qty(2)), (Px(99), Qty(3))]
        );
        assert_eq!(b.best_bid(), Some((Px(102), Qty(7))));
        assert_strictly_descending(&b.bids);

        // New worst: position(p<=98) = None => push to the back.
        let mut b = run(&base);
        b.apply(&BookEvent::level(4, 4, Side::Bid, Px(98), Qty(4)));
        assert_eq!(
            b.bids,
            vec![(Px(101), Qty(1)), (Px(100), Qty(2)), (Px(99), Qty(3)), (Px(98), Qty(4))]
        );
        assert_strictly_descending(&b.bids);
    }

    // 3b. The mirror of the worked check on the ask side (ascending): new-best
    //     inserts at 0, mid-ladder updates in place, new-worst pushes.
    #[test]
    fn worked_insertion_check_asks() {
        let base = [
            BookEvent::level(1, 1, Side::Ask, Px(99), Qty(1)),
            BookEvent::level(2, 2, Side::Ask, Px(100), Qty(2)),
            BookEvent::level(3, 3, Side::Ask, Px(101), Qty(3)),
        ];

        // Mid-ladder, same price: position(p>=100) = 1, vec[1].0 == 100 => update.
        let mut a = run(&base);
        a.apply(&BookEvent::level(4, 4, Side::Ask, Px(100), Qty(9)));
        assert_eq!(
            a.asks,
            vec![(Px(99), Qty(1)), (Px(100), Qty(9)), (Px(101), Qty(3))]
        );
        assert_strictly_ascending(&a.asks);

        // New best (lowest): position(p>=98) = 0, != 98 => insert at 0.
        let mut a = run(&base);
        a.apply(&BookEvent::level(4, 4, Side::Ask, Px(98), Qty(7)));
        assert_eq!(
            a.asks,
            vec![(Px(98), Qty(7)), (Px(99), Qty(1)), (Px(100), Qty(2)), (Px(101), Qty(3))]
        );
        assert_eq!(a.best_ask(), Some((Px(98), Qty(7))));
        assert_strictly_ascending(&a.asks);

        // New worst (highest): position(p>=102) = None => push.
        let mut a = run(&base);
        a.apply(&BookEvent::level(4, 4, Side::Ask, Px(102), Qty(4)));
        assert_eq!(
            a.asks,
            vec![(Px(99), Qty(1)), (Px(100), Qty(2)), (Px(101), Qty(3)), (Px(102), Qty(4))]
        );
        assert_strictly_ascending(&a.asks);
    }

    // 4. Reallocation churn: insert > 1 page of levels, remove half, assert
    //    best-first ladder correctness (same shape as the SortedVecBook test).
    #[test]
    fn realloc_churn_preserves_ladder() {
        // 4 KiB page / 16 B per level = 256 levels per page; exceed it comfortably.
        const N: usize = 600;
        let mut b = RevVecBook::default();
        // Insert ascending prices on the ask side (best-first storage means each
        // new higher price is pushed to the back — forces Vec growth).
        for k in 0..N {
            let k = i64::try_from(k).unwrap();
            b.apply(&BookEvent::level(1, 1, Side::Ask, Px(10_000 + k), Qty(k + 1)));
        }
        assert_eq!(b.depth(Side::Ask), N);
        assert_strictly_ascending(&b.asks);
        assert_eq!(b.best_ask(), Some((Px(10_000), Qty(1)))); // lowest = [0]

        // Remove every even-priced level (a strided subset).
        for k in (0..N).step_by(2) {
            let k = i64::try_from(k).unwrap();
            b.apply(&BookEvent::level(0, 0, Side::Ask, Px(10_000 + k), Qty(0)));
        }
        assert_eq!(b.depth(Side::Ask), N / 2);
        assert_strictly_ascending(&b.asks);
        // Lowest surviving ask is the first odd offset, k=1.
        assert_eq!(b.best_ask(), Some((Px(10_001), Qty(2))));
        // Every surviving price is odd-offset; verify via the full best-first ladder.
        let full = ladder(&b, Side::Ask);
        for (i, (px, _)) in full.iter().enumerate() {
            let i = i64::try_from(i).unwrap();
            assert_eq!(px.ticks(), 10_000 + (2 * i + 1));
        }
    }
}

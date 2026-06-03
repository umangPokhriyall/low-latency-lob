//! `SortedVecBook` — price levels in a contiguous `Vec`, kept strictly ascending
//! by price on both sides, located by binary search. The binary-search arm of the
//! Phase 4 shootout; contrast `RevVecBook`'s linear scan. INVARIANTS hold after
//! every `apply`: each side is strictly ascending with no duplicate prices.

use crate::{BookEvent, EventKind, OrderBook, Px, Qty, Side};

#[derive(Default, Debug)]
pub struct SortedVecBook {
    bids: Vec<(Px, Qty)>, // strictly ascending; best (highest) = last()
    asks: Vec<(Px, Qty)>, // strictly ascending; best (lowest)  = first()
    last_trade: Option<(Px, Qty, Side)>,
}

/// Both sides are ascending, so one routine serves both.
/// Absolute semantics: qty == 0 removes; otherwise insert-or-replace.
fn update_ascending(vec: &mut Vec<(Px, Qty)>, px: Px, qty: Qty) {
    match vec.binary_search_by_key(&px, |&(p, _)| p) {
        Ok(i) => {
            if qty.is_zero() {
                vec.remove(i);
            } else {
                vec[i].1 = qty;
            }
        }
        Err(i) => {
            if !qty.is_zero() {
                vec.insert(i, (px, qty));
            }
        }
    }
}

impl OrderBook for SortedVecBook {
    #[inline]
    fn apply(&mut self, ev: &BookEvent) {
        match ev.kind {
            EventKind::Level => {
                let side = match ev.side {
                    Side::Bid => &mut self.bids,
                    Side::Ask => &mut self.asks,
                };
                update_ascending(side, ev.px, ev.qty);
            }
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
        self.bids.last().copied()
    }

    #[inline]
    fn best_ask(&self) -> Option<(Px, Qty)> {
        self.asks.first().copied()
    }

    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize {
        let mut n = 0;
        match side {
            Side::Bid => {
                for (slot, lvl) in out.iter_mut().zip(self.bids.iter().rev()) {
                    *slot = *lvl;
                    n += 1;
                }
            }
            Side::Ask => {
                for (slot, lvl) in out.iter_mut().zip(self.asks.iter()) {
                    *slot = *lvl;
                    n += 1;
                }
            }
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

    /// Drive a fresh `SortedVecBook` through an event slice and return it.
    fn run(events: &[BookEvent]) -> SortedVecBook {
        let mut b = SortedVecBook::default();
        for ev in events {
            b.apply(ev);
        }
        b
    }

    /// The full best-first ladder of `side`, read through the public `top_n`
    /// surface (no internal-representation peeking beyond the test accessor).
    fn ladder(b: &SortedVecBook, side: Side) -> Vec<(Px, Qty)> {
        let mut out = vec![(Px::ZERO, Qty::ZERO); b.depth(side)];
        let n = b.top_n(side, &mut out);
        assert_eq!(n, b.depth(side), "top_n count must equal depth");
        out
    }

    /// Assert a side's *internal* storage is strictly ascending and duplicate-free.
    /// `bids`/`asks` are stored ascending; this is the load-bearing invariant.
    fn assert_strictly_ascending(vec: &[(Px, Qty)]) {
        for w in vec.windows(2) {
            assert!(w[0].0 < w[1].0, "not strictly ascending / has duplicate: {vec:?}");
        }
    }

    // 1. Ascending invariant + duplicate-freedom after a churn of inserts/updates/removes.
    #[test]
    fn ascending_invariant_holds_after_churn() {
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
        // Stored ascending on both sides.
        assert_strictly_ascending(&b.bids);
        assert_strictly_ascending(&b.asks);
        // best (highest) bid = last(); best (lowest) ask = first().
        assert_eq!(b.best_bid(), Some((Px(102), Qty(7))));
        assert_eq!(b.best_ask(), Some((Px(108), Qty(6))));
        assert_eq!(b.depth(Side::Bid), 3); // 100,101,102
        assert_eq!(b.depth(Side::Ask), 2); // 108,110
    }

    // 2. Binary-search insert position: out-of-order prices yield a sorted ladder via top_n.
    #[test]
    fn out_of_order_inserts_yield_sorted_ladder() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(99), Qty(1)),
            BookEvent::level(2, 2, Side::Bid, Px(103), Qty(2)),
            BookEvent::level(3, 3, Side::Bid, Px(97), Qty(3)),
            BookEvent::level(4, 4, Side::Bid, Px(101), Qty(4)),
            BookEvent::level(5, 5, Side::Bid, Px(95), Qty(5)),
        ]);
        // Bids are read best-first (highest price first).
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
        // Asks are read best-first (lowest price first).
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

    // 3. Removal via qty=0 shifts correctly; removing an absent price is a no-op.
    #[test]
    fn removal_shifts_and_absent_removal_is_noop() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(98), Qty(2)),
            BookEvent::level(4, 4, Side::Bid, Px(99), Qty(0)), // remove middle -> shift
            BookEvent::level(5, 5, Side::Bid, Px(50), Qty(0)), // absent -> no-op
        ]);
        assert_strictly_ascending(&b.bids);
        assert_eq!(b.depth(Side::Bid), 2);
        assert_eq!(ladder(&b, Side::Bid), vec![(Px(100), Qty(5)), (Px(98), Qty(2))]);

        // Remove on an empty side is also a no-op.
        let e = run(&[BookEvent::level(1, 1, Side::Ask, Px(123), Qty(0))]);
        assert_eq!(e.depth(Side::Ask), 0);
        assert_eq!(e.best_ask(), None);
    }

    // 4. Reallocation churn: insert > 1 page of levels, remove half, assert ladder correctness.
    #[test]
    fn realloc_churn_preserves_ladder() {
        // 4 KiB page / 16 B per level = 256 levels per page; exceed it comfortably.
        const N: usize = 600;
        let mut b = SortedVecBook::default();
        // Insert ascending prices on the ask side (forces growth as the Vec reallocates).
        for k in 0..N {
            let k = i64::try_from(k).unwrap();
            b.apply(&BookEvent::level(1, 1, Side::Ask, Px(10_000 + k), Qty(k + 1)));
        }
        assert_eq!(b.depth(Side::Ask), N);
        assert_strictly_ascending(&b.asks);
        assert_eq!(b.best_ask(), Some((Px(10_000), Qty(1))));

        // Remove every even-priced level (a strided subset).
        for k in (0..N).step_by(2) {
            let k = i64::try_from(k).unwrap();
            b.apply(&BookEvent::level(0, 0, Side::Ask, Px(10_000 + k), Qty(0)));
        }
        assert_eq!(b.depth(Side::Ask), N / 2);
        assert_strictly_ascending(&b.asks);
        // Lowest surviving ask is the first odd offset, k=1.
        assert_eq!(b.best_ask(), Some((Px(10_001), Qty(2))));
        // Every surviving price is odd-offset; verify via the full ladder.
        let full = ladder(&b, Side::Ask);
        for (i, (px, _)) in full.iter().enumerate() {
            let i = i64::try_from(i).unwrap();
            assert_eq!(px.ticks(), 10_000 + (2 * i + 1));
        }
    }
}

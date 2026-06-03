// FROZEN — book v1 (Phase 2). Do not modify.
// The `book` crate is the sans-IO core. New order-book implementations are
// ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
// satisfy the frozen `OrderBook` trait without changing this file. If a later
// phase appears to need a change here, the design is wrong — STOP and ask.
// See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.

//! `BTreeBook` — the baseline price-level book over `std::collections::BTreeMap`.
//! Pointer-chasing, per-node allocation, O(log n) with poor cache constants:
//! the slow baseline the sorted-Vec, reverse-Vec, and flat-array variants
//! (Phases 2 and 5) must beat. Its job is to be measured against and to anchor
//! the Phase 2 differential correctness oracle.

use crate::{BookEvent, EventKind, OrderBook, Px, Qty, Side};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct BTreeBook {
    /// Highest key = best bid; iterate in reverse for best-first.
    bids: BTreeMap<Px, Qty>,
    /// Lowest key = best ask; iterate forward for best-first.
    asks: BTreeMap<Px, Qty>,
    last_trade: Option<(Px, Qty, Side)>,
}

impl BTreeBook {
    #[inline]
    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Px, Qty> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }
}

/// Copy up to `out.len()` `(Px, Qty)` pairs from a best-first iterator; return the count.
fn fill<'a, I>(it: I, out: &mut [(Px, Qty)]) -> usize
where
    I: Iterator<Item = (&'a Px, &'a Qty)>,
{
    let mut n = 0;
    for (&p, &q) in it.take(out.len()) {
        out[n] = (p, q);
        n += 1;
    }
    n
}

impl OrderBook for BTreeBook {
    #[inline]
    fn apply(&mut self, ev: &BookEvent) {
        match ev.kind {
            EventKind::Level => {
                let book = self.side_mut(ev.side);
                if ev.qty.is_zero() {
                    book.remove(&ev.px);
                } else {
                    book.insert(ev.px, ev.qty); // absolute set: last write wins
                }
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
        self.bids.iter().next_back().map(|(&p, &q)| (p, q))
    }

    #[inline]
    fn best_ask(&self) -> Option<(Px, Qty)> {
        self.asks.iter().next().map(|(&p, &q)| (p, q))
    }

    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize {
        match side {
            Side::Bid => fill(self.bids.iter().rev(), out),
            Side::Ask => fill(self.asks.iter(), out),
        }
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

    /// Drive a fresh `BTreeBook` through an event slice and return it.
    fn run(events: &[BookEvent]) -> BTreeBook {
        let mut b = BTreeBook::default();
        for ev in events {
            b.apply(ev);
        }
        b
    }

    /// The hand-verified Phase 1 scenario, up to (but excluding) the terminal
    /// `Clear`. Kept as a standalone builder because Phase 2's differential
    /// oracle reuses it verbatim as a fixture. Sequence numbers are monotonic
    /// for realism; the book does not police them (see phase1-spec §2.6).
    fn hand_verified_events() -> Vec<BookEvent> {
        vec![
            BookEvent::clear(0, 0),                              // no-op on empty
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(101), Qty(2)),
            BookEvent::level(4, 4, Side::Ask, Px(103), Qty(4)),
            BookEvent::level(5, 5, Side::Ask, Px(102), Qty(1)),
            BookEvent::trade(6, 6, Side::Ask, Px(102), Qty(1)),  // last-trade only
            BookEvent::level(7, 7, Side::Ask, Px(102), Qty(0)),  // remove ask 102
            BookEvent::level(8, 8, Side::Bid, Px(101), Qty(0)),  // remove bid 101
        ]
    }

    // 1. Empty book: no best, zero depth, no last trade, top_n writes nothing.
    #[test]
    fn empty_book_observable_state() {
        let b = BTreeBook::default();
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.best_ask(), None);
        assert_eq!(b.depth(Side::Bid), 0);
        assert_eq!(b.depth(Side::Ask), 0);
        assert_eq!(b.last_trade(), None);
        let mut buf = [(Px::ZERO, Qty::ZERO); 4];
        assert_eq!(b.top_n(Side::Bid, &mut buf), 0);
        assert_eq!(b.top_n(Side::Ask, &mut buf), 0);
    }

    // 2. Single bid / single ask reflected with correct qty.
    #[test]
    fn single_level_each_side() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Ask, Px(101), Qty(7)),
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.best_ask(), Some((Px(101), Qty(7))));
        assert_eq!(b.depth(Side::Bid), 1);
        assert_eq!(b.depth(Side::Ask), 1);
    }

    // 3. Ordering: best bid = highest px, best ask = lowest px.
    #[test]
    fn best_is_highest_bid_and_lowest_ask() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(98), Qty(1)),
            BookEvent::level(2, 2, Side::Bid, Px(100), Qty(2)),
            BookEvent::level(3, 3, Side::Bid, Px(99), Qty(3)),
            BookEvent::level(4, 4, Side::Ask, Px(105), Qty(4)),
            BookEvent::level(5, 5, Side::Ask, Px(103), Qty(5)),
            BookEvent::level(6, 6, Side::Ask, Px(104), Qty(6)),
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(2))));
        assert_eq!(b.best_ask(), Some((Px(103), Qty(5))));
    }

    // 4. Absolute set / last-write-wins at the same px.
    #[test]
    fn absolute_set_last_write_wins() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(100), Qty(9)),
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(9))));
        assert_eq!(b.depth(Side::Bid), 1);
    }

    // 5. Removal: qty == 0 removes; removing a missing level is a no-op.
    #[test]
    fn removal_and_missing_removal_is_noop() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(100), Qty(0)), // remove existing
            BookEvent::level(4, 4, Side::Bid, Px(50), Qty(0)),  // remove missing: no-op
        ]);
        assert_eq!(b.depth(Side::Bid), 1);
        assert_eq!(b.best_bid(), Some((Px(99), Qty(3))));
    }

    // 6. Trade isolation: updates last_trade, leaves levels and depths untouched.
    #[test]
    fn trade_does_not_mutate_levels() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Ask, Px(101), Qty(4)),
            BookEvent::trade(3, 3, Side::Ask, Px(101), Qty(2)),
        ]);
        assert_eq!(b.last_trade(), Some((Px(101), Qty(2), Side::Ask)));
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.best_ask(), Some((Px(101), Qty(4))));
        assert_eq!(b.depth(Side::Bid), 1);
        assert_eq!(b.depth(Side::Ask), 1);
    }

    // 7. top_n with a buffer shorter than depth: returns out.len(), best-first.
    #[test]
    fn top_n_short_buffer() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(98), Qty(1)),
        ]);
        let mut buf = [(Px::ZERO, Qty::ZERO); 2];
        assert_eq!(b.top_n(Side::Bid, &mut buf), 2);
        assert_eq!(buf, [(Px(100), Qty(5)), (Px(99), Qty(3))]);
    }

    // 8. top_n with a buffer longer than depth: returns depth, tail untouched.
    #[test]
    fn top_n_long_buffer_leaves_tail() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Ask, Px(101), Qty(4)),
            BookEvent::level(2, 2, Side::Ask, Px(102), Qty(6)),
        ]);
        let sentinel = (Px(-1), Qty(-1));
        let mut buf = [sentinel; 5];
        assert_eq!(b.top_n(Side::Ask, &mut buf), 2);
        assert_eq!(buf[0], (Px(101), Qty(4)));
        assert_eq!(buf[1], (Px(102), Qty(6)));
        assert_eq!(&buf[2..], &[sentinel; 3]); // tail untouched
    }

    // 9. Clear empties both sides and drops the last trade.
    #[test]
    fn clear_resets_everything() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Ask, Px(101), Qty(4)),
            BookEvent::trade(3, 3, Side::Ask, Px(101), Qty(2)),
            BookEvent::clear(4, 4),
        ]);
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.best_ask(), None);
        assert_eq!(b.last_trade(), None);
        assert_eq!(b.depth(Side::Bid), 0);
        assert_eq!(b.depth(Side::Ask), 0);
    }

    // 10. The mandatory hand-verified scenario, asserted exactly as specified.
    #[test]
    fn hand_verified_scenario() {
        let events = hand_verified_events();
        let mut b = BTreeBook::default();

        // Clear on empty: no-op.
        b.apply(&events[0]);
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.best_ask(), None);

        // Level Bid 100 = 5 -> best_bid (100,5)
        b.apply(&events[1]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));

        // Level Bid 99 = 3 -> best_bid (100,5), depth(Bid)=2
        b.apply(&events[2]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.depth(Side::Bid), 2);

        // Level Bid 101 = 2 -> best_bid (101,2), depth(Bid)=3
        b.apply(&events[3]);
        assert_eq!(b.best_bid(), Some((Px(101), Qty(2))));
        assert_eq!(b.depth(Side::Bid), 3);

        // Level Ask 103 = 4 -> best_ask (103,4)
        b.apply(&events[4]);
        assert_eq!(b.best_ask(), Some((Px(103), Qty(4))));

        // Level Ask 102 = 1 -> best_ask (102,1), depth(Ask)=2
        b.apply(&events[5]);
        assert_eq!(b.best_ask(), Some((Px(102), Qty(1))));
        assert_eq!(b.depth(Side::Ask), 2);

        // Trade Ask 102 = 1 -> last_trade (102,1,Ask); depths unchanged (Bid=3, Ask=2)
        b.apply(&events[6]);
        assert_eq!(b.last_trade(), Some((Px(102), Qty(1), Side::Ask)));
        assert_eq!(b.depth(Side::Bid), 3);
        assert_eq!(b.depth(Side::Ask), 2);

        // Level Ask 102 = 0 -> best_ask (103,4), depth(Ask)=1
        b.apply(&events[7]);
        assert_eq!(b.best_ask(), Some((Px(103), Qty(4))));
        assert_eq!(b.depth(Side::Ask), 1);

        // Level Bid 101 = 0 -> best_bid (100,5), depth(Bid)=2
        b.apply(&events[8]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.depth(Side::Bid), 2);

        // top_n(Bid, buf[3]) == 2 and buf[..2] == [(100,5),(99,3)]
        let mut buf = [(Px::ZERO, Qty::ZERO); 3];
        assert_eq!(b.top_n(Side::Bid, &mut buf), 2);
        assert_eq!(&buf[..2], &[(Px(100), Qty(5)), (Px(99), Qty(3))]);

        // top_n(Ask, buf[1]) == 1 and buf[..1] == [(103,4)]
        let mut buf1 = [(Px::ZERO, Qty::ZERO); 1];
        assert_eq!(b.top_n(Side::Ask, &mut buf1), 1);
        assert_eq!(&buf1[..1], &[(Px(103), Qty(4))]);

        // Clear -> best_bid None, best_ask None, last_trade None, depths 0
        b.apply(&BookEvent::clear(9, 9));
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.best_ask(), None);
        assert_eq!(b.last_trade(), None);
        assert_eq!(b.depth(Side::Bid), 0);
        assert_eq!(b.depth(Side::Ask), 0);
    }
}

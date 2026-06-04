//! `FlatBook` — price levels in a flat array indexed by tick offset from a base.
//! O(1) update by direct index, immune to depth and touch locality. Cost: memory
//! ~ price SPAN, plus a best-removal probe and a sparse-ladder `top_n` scan.
//! Applicable to BOUNDED price ranges only (see phase5-spec §2.6). This is the
//! single impl added after the book freeze; it is NOT marked FROZEN.

// Every tick<->index conversion below is bounded by `MAX_SPAN` (< 2^23 ticks):
// indices live in `[0, len)` with `len <= MAX_SPAN`, so the casts never lose
// sign, wrap, or truncate on 32- or 64-bit targets. The pedantic cast lints
// cannot see that invariant; allow them module-wide with this rationale.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::{BookEvent, EventKind, OrderBook, Px, Qty, Side};

/// Hard cap on representable span (ticks). Beyond it the flat array is the wrong
/// structure; panic loudly rather than allocate unbounded memory.
const MAX_SPAN: usize = 8 * 1024 * 1024; // 8M ticks -> two 64 MiB arrays
/// Half-span pre-allocated around the first observed price (keeps recenters rare).
const INIT_HALF_SPAN: usize = 4096;

#[derive(Default, Debug)]
pub struct FlatBook {
    base: i64,                   // tick at index 0 (valid once `inited`)
    inited: bool,
    bid_qty: Vec<Qty>,           // bid_qty[i] = qty at tick (base + i); ZERO = empty
    ask_qty: Vec<Qty>,           // parallel ask-side array
    best_bid_idx: Option<usize>, // highest occupied bid index
    best_ask_idx: Option<usize>, // lowest occupied ask index
    bid_depth: usize,            // count of occupied bid slots (O(1) `depth`)
    ask_depth: usize,            // count of occupied ask slots
    last_trade: Option<(Px, Qty, Side)>,
}

impl FlatBook {
    /// The allocated price span in ticks — the length of each per-side array.
    /// Total memory is `2 * allocated_span_ticks() * size_of::<Qty>()` bytes (two
    /// parallel arrays). Zero before lazy init. Exposed so the Phase 5
    /// memory-vs-speed tradeoff is sourced (`bench/results/flat_memory.csv`), not
    /// asserted; it reads internal state and so cannot live outside this module.
    #[must_use]
    pub fn allocated_span_ticks(&self) -> usize {
        self.bid_qty.len()
    }

    /// Ensure the flat arrays cover `px`. Lazily initializes around the first
    /// observed price; thereafter recenters/grows (phase5-spec §2.5) when `px`
    /// falls outside `[base, base + len)`. Panics if the required span exceeds
    /// `MAX_SPAN` (the bounded-range contract, §2.6).
    fn ensure_range(&mut self, px: Px) {
        let p = px.ticks();
        if !self.inited {
            let base = p - INIT_HALF_SPAN as i64;
            let len = 2 * INIT_HALF_SPAN + 1; // covers [p - HALF, p + HALF]
            self.base = base;
            self.bid_qty = vec![Qty::ZERO; len];
            self.ask_qty = vec![Qty::ZERO; len];
            self.inited = true;
            return;
        }

        let old_len = self.bid_qty.len() as i64;
        let old_lo = self.base;
        let old_hi = old_lo + old_len; // exclusive
        if p >= old_lo && p < old_hi {
            return;
        }

        // Recenter / grow: cover the union of the existing range with `px`, plus
        // a margin so a run of moves in one direction does not re-grow each step.
        let margin = INIT_HALF_SPAN as i64;
        let new_lo = old_lo.min(p - margin);
        let new_hi = old_hi.max(p + 1 + margin);
        let new_len = (new_hi - new_lo) as usize;
        assert!(
            new_len <= MAX_SPAN,
            "FlatBook span exceeds MAX_SPAN; the flat array is for bounded ranges \
             — use a tree/Vec book for unbounded ranges"
        );

        let shift = (old_lo - new_lo) as usize; // index where old[0] lands
        let old_len = old_len as usize;
        let mut new_bid = vec![Qty::ZERO; new_len];
        let mut new_ask = vec![Qty::ZERO; new_len];
        new_bid[shift..shift + old_len].copy_from_slice(&self.bid_qty);
        new_ask[shift..shift + old_len].copy_from_slice(&self.ask_qty);
        self.bid_qty = new_bid;
        self.ask_qty = new_ask;
        self.base = new_lo;
        // Cached bests are offsets into the array; the base moved by `shift`.
        self.best_bid_idx = self.best_bid_idx.map(|b| b + shift);
        self.best_ask_idx = self.best_ask_idx.map(|b| b + shift);
    }
}

impl OrderBook for FlatBook {
    #[inline]
    fn apply(&mut self, ev: &BookEvent) {
        match ev.kind {
            EventKind::Level => {
                self.ensure_range(ev.px);
                let i = (ev.px.ticks() - self.base) as usize;
                match ev.side {
                    Side::Bid => {
                        let was_occupied = !self.bid_qty[i].is_zero();
                        self.bid_qty[i] = ev.qty;
                        if ev.qty.is_zero() {
                            if was_occupied {
                                self.bid_depth -= 1;
                                if self.best_bid_idx == Some(i) {
                                    // Probe toward the worse (lower) bid price.
                                    self.best_bid_idx =
                                        (0..i).rev().find(|&j| !self.bid_qty[j].is_zero());
                                }
                            }
                        } else {
                            if !was_occupied {
                                self.bid_depth += 1;
                            }
                            if self.best_bid_idx.is_none_or(|b| i > b) {
                                self.best_bid_idx = Some(i);
                            }
                        }
                    }
                    Side::Ask => {
                        let was_occupied = !self.ask_qty[i].is_zero();
                        self.ask_qty[i] = ev.qty;
                        if ev.qty.is_zero() {
                            if was_occupied {
                                self.ask_depth -= 1;
                                if self.best_ask_idx == Some(i) {
                                    // Probe toward the worse (higher) ask price.
                                    let len = self.ask_qty.len();
                                    self.best_ask_idx =
                                        (i + 1..len).find(|&j| !self.ask_qty[j].is_zero());
                                }
                            }
                        } else {
                            if !was_occupied {
                                self.ask_depth += 1;
                            }
                            if self.best_ask_idx.is_none_or(|b| i < b) {
                                self.best_ask_idx = Some(i);
                            }
                        }
                    }
                }
            }
            EventKind::Trade => self.last_trade = Some((ev.px, ev.qty, ev.side)),
            EventKind::Clear => {
                // Zero content but RETAIN capacity (no churn); base/inited kept.
                self.bid_qty.fill(Qty::ZERO);
                self.ask_qty.fill(Qty::ZERO);
                self.best_bid_idx = None;
                self.best_ask_idx = None;
                self.bid_depth = 0;
                self.ask_depth = 0;
                self.last_trade = None;
            }
        }
    }

    #[inline]
    fn best_bid(&self) -> Option<(Px, Qty)> {
        self.best_bid_idx
            .map(|i| (Px(self.base + i as i64), self.bid_qty[i]))
    }

    #[inline]
    fn best_ask(&self) -> Option<(Px, Qty)> {
        self.best_ask_idx
            .map(|i| (Px(self.base + i as i64), self.ask_qty[i]))
    }

    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize {
        let mut n = 0;
        match side {
            Side::Bid => {
                // Walk from the best (highest index) toward lower indices.
                if let Some(best) = self.best_bid_idx {
                    let mut i = best as i64;
                    while i >= 0 && n < out.len() {
                        let q = self.bid_qty[i as usize];
                        if !q.is_zero() {
                            out[n] = (Px(self.base + i), q);
                            n += 1;
                        }
                        i -= 1;
                    }
                }
            }
            Side::Ask => {
                // Walk from the best (lowest index) toward higher indices.
                if let Some(best) = self.best_ask_idx {
                    let len = self.ask_qty.len();
                    let mut i = best;
                    while i < len && n < out.len() {
                        let q = self.ask_qty[i];
                        if !q.is_zero() {
                            out[n] = (Px(self.base + i as i64), q);
                            n += 1;
                        }
                        i += 1;
                    }
                }
            }
        }
        n
    }

    #[inline]
    fn depth(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bid_depth,
            Side::Ask => self.ask_depth,
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

    fn run(events: &[BookEvent]) -> FlatBook {
        let mut b = FlatBook::default();
        for ev in events {
            b.apply(ev);
        }
        b
    }

    fn ladder(b: &FlatBook, side: Side) -> Vec<(Px, Qty)> {
        let mut out = vec![(Px::ZERO, Qty::ZERO); b.depth(side)];
        let n = b.top_n(side, &mut out);
        assert_eq!(n, b.depth(side), "top_n count must equal depth");
        out
    }

    // Direct-index update, cached best, and the removal probe toward the worse side.
    #[test]
    fn best_and_probe_after_churn() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(98), Qty(3)),
            BookEvent::level(3, 3, Side::Bid, Px(102), Qty(7)),
            BookEvent::level(4, 4, Side::Bid, Px(102), Qty(0)), // remove best -> probe to 100
            BookEvent::level(5, 5, Side::Ask, Px(110), Qty(4)),
            BookEvent::level(6, 6, Side::Ask, Px(105), Qty(2)),
            BookEvent::level(7, 7, Side::Ask, Px(105), Qty(0)), // remove best -> probe to 110
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.best_ask(), Some((Px(110), Qty(4))));
        assert_eq!(b.depth(Side::Bid), 2); // 100, 98
        assert_eq!(b.depth(Side::Ask), 1); // 110
        assert_eq!(
            ladder(&b, Side::Bid),
            vec![(Px(100), Qty(5)), (Px(98), Qty(3))]
        );
    }

    // Update-in-place of an existing level keeps depth and best unchanged.
    #[test]
    fn update_in_place_is_o1() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(2, 2, Side::Bid, Px(100), Qty(9)),
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(9))));
        assert_eq!(b.depth(Side::Bid), 1);
    }

    // Remove-absent on an empty and a non-empty book is a no-op.
    #[test]
    fn remove_absent_is_noop() {
        let b = run(&[
            BookEvent::level(1, 1, Side::Bid, Px(100), Qty(0)), // empty book
            BookEvent::level(2, 2, Side::Bid, Px(100), Qty(5)),
            BookEvent::level(3, 3, Side::Bid, Px(99), Qty(0)), // absent price
        ]);
        assert_eq!(b.best_bid(), Some((Px(100), Qty(5))));
        assert_eq!(b.depth(Side::Bid), 1);
    }

    // Clear retains allocated capacity (no churn) while resetting observable state.
    #[test]
    fn clear_retains_capacity() {
        let mut b = run(&[BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5))]);
        let cap_bid = b.bid_qty.capacity();
        let cap_ask = b.ask_qty.capacity();
        b.apply(&BookEvent::clear(2, 2));
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.depth(Side::Bid), 0);
        assert_eq!(b.bid_qty.capacity(), cap_bid);
        assert_eq!(b.ask_qty.capacity(), cap_ask);
        assert!(b.inited, "Clear keeps the book initialized");
        // Rebuild works against the retained arrays.
        b.apply(&BookEvent::level(3, 3, Side::Ask, Px(105), Qty(2)));
        assert_eq!(b.best_ask(), Some((Px(105), Qty(2))));
    }
}

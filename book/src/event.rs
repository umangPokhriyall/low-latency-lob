// FROZEN — book v1 (Phase 2). Do not modify.
// The `book` crate is the sans-IO core. New order-book implementations are
// ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
// satisfy the frozen `OrderBook` trait without changing this file. If a later
// phase appears to need a change here, the design is wrong — STOP and ask.
// See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.

//! L2 price-level event model. Layout is locked here so the Phase 3 corpus can
//! memory-map a flat `[BookEvent]` and the replay iterator can hand out
//! references with zero copying. See phase1-spec §2.1–2.2 for the L2 rationale.

use crate::{Px, Qty};

/// Book side. `repr(u8)` so it packs into the flat `BookEvent` record.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    Bid = 0,
    Ask = 1,
}

impl Side {
    #[inline]
    #[must_use]
    pub const fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// What an event does to the book.
///
/// `Level` is an **absolute** aggregate-quantity update at `(side, px)`:
/// `qty == 0` removes the level, any other value replaces it (last write wins).
/// `Trade` is an execution print: it updates the last-trade cache only and does
/// not mutate price levels. `Clear` resets the book to empty.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    Level = 0,
    Trade = 1,
    Clear = 2,
}

/// One feed event. `repr(C)`, `Copy`, fixed 40-byte layout (locked for the corpus).
/// For `Clear`, `px`/`qty`/`side` are ignored. For `Trade`, `side` is the aggressor side.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BookEvent {
    pub seq: u64,
    pub ts: u64,
    pub px: Px,
    pub qty: Qty,
    pub side: Side,
    pub kind: EventKind,
}

impl BookEvent {
    /// Absolute level update at `(side, px)`. `qty == Qty::ZERO` removes the level.
    #[inline]
    #[must_use]
    pub const fn level(seq: u64, ts: u64, side: Side, px: Px, qty: Qty) -> Self {
        Self { seq, ts, px, qty, side, kind: EventKind::Level }
    }

    /// Execution print; `aggressor` is the taker side.
    #[inline]
    #[must_use]
    pub const fn trade(seq: u64, ts: u64, aggressor: Side, px: Px, qty: Qty) -> Self {
        Self { seq, ts, px, qty, side: aggressor, kind: EventKind::Trade }
    }

    /// Reset the book to empty (snapshot boundary / start of stream).
    #[inline]
    #[must_use]
    pub const fn clear(seq: u64, ts: u64) -> Self {
        Self { seq, ts, px: Px::ZERO, qty: Qty::ZERO, side: Side::Bid, kind: EventKind::Clear }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn book_event_layout_is_locked() {
        // The Phase 3 corpus mmaps a flat [BookEvent]; this size/align is a contract.
        assert_eq!(size_of::<BookEvent>(), 40);
        assert_eq!(align_of::<BookEvent>(), 8);
    }

    #[test]
    fn side_opposite() {
        assert_eq!(Side::Bid.opposite(), Side::Ask);
        assert_eq!(Side::Ask.opposite(), Side::Bid);
    }

    #[test]
    fn constructors_set_kind_and_fields() {
        let l = BookEvent::level(1, 10, Side::Bid, Px(100), Qty(5));
        assert_eq!(l.kind, EventKind::Level);
        assert_eq!((l.side, l.px, l.qty), (Side::Bid, Px(100), Qty(5)));

        let t = BookEvent::trade(2, 20, Side::Ask, Px(101), Qty(2));
        assert_eq!(t.kind, EventKind::Trade);
        assert_eq!(t.side, Side::Ask);

        let c = BookEvent::clear(3, 30);
        assert_eq!(c.kind, EventKind::Clear);
    }
}

//! Shared integration-test fixtures for the differential oracle.
//!
//! This module lives under `tests/` (so it sees only `book`'s public API) and is
//! pulled in by `oracle.rs` via `mod common;`. It holds the Phase-1 hand-verified
//! scenario, lifted verbatim from `btree.rs`'s standalone regression builder, so
//! all three impls can be driven through the exact same sequence. (`src/` cannot
//! see `tests/`, so `btree.rs` keeps its own inline copy — accepted duplication.)

use book::{BookEvent, Px, Qty, Side};

/// The exact Phase-1 hand-verified event sequence: clear-on-empty, a small ladder
/// build on both sides, a trade print, then two `qty=0` removals. Sequence numbers
/// are monotonic for realism; the book does not police them.
pub fn shared_scenario() -> Vec<BookEvent> {
    vec![
        BookEvent::clear(0, 0), // no-op on empty
        BookEvent::level(1, 1, Side::Bid, Px(100), Qty(5)),
        BookEvent::level(2, 2, Side::Bid, Px(99), Qty(3)),
        BookEvent::level(3, 3, Side::Bid, Px(101), Qty(2)),
        BookEvent::level(4, 4, Side::Ask, Px(103), Qty(4)),
        BookEvent::level(5, 5, Side::Ask, Px(102), Qty(1)),
        BookEvent::trade(6, 6, Side::Ask, Px(102), Qty(1)), // last-trade only
        BookEvent::level(7, 7, Side::Ask, Px(102), Qty(0)), // remove ask 102
        BookEvent::level(8, 8, Side::Bid, Px(101), Qty(0)), // remove bid 101
    ]
}

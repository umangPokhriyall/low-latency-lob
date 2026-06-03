// FROZEN — book v1 (Phase 2). Do not modify.
// The `book` crate is the sans-IO core. New order-book implementations are
// ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
// satisfy the frozen `OrderBook` trait without changing this file. If a later
// phase appears to need a change here, the design is wrong — STOP and ask.
// See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.

//! The `OrderBook` trait: one abstraction, many implementations (Phases 1, 2, 5).

use crate::{BookEvent, Px, Qty, Side};

/// An L2 price-level order book. Implementations differ only in the price-level
/// container; the Phase 2 differential oracle requires that, for any event
/// sequence, every implementation produces identical observable state.
///
/// Hot-path contract for every impl:
/// - `apply` performs no heap allocation and no I/O.
/// - read methods write into caller-provided buffers rather than allocating.
/// - consumers drive impls by monomorphization (`fn run<B: OrderBook>`), never
///   via `dyn OrderBook` (see phase1-spec §2.4).
pub trait OrderBook: Default {
    /// Apply one event, mutating book state.
    fn apply(&mut self, ev: &BookEvent);

    /// Best (highest) bid as `(price, aggregate qty)`, or `None` if the side is empty.
    fn best_bid(&self) -> Option<(Px, Qty)>;

    /// Best (lowest) ask as `(price, aggregate qty)`, or `None` if the side is empty.
    fn best_ask(&self) -> Option<(Px, Qty)>;

    /// Write up to `out.len()` levels of `side`, best-first, into `out`;
    /// return the number of levels written.
    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize;

    /// Number of resident price levels on `side`.
    fn depth(&self, side: Side) -> usize;

    /// Last trade seen as `(price, qty, aggressor)`, if any.
    fn last_trade(&self) -> Option<(Px, Qty, Side)>;
}

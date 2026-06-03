// FROZEN — book v1 (Phase 2). Do not modify.
// The `book` crate is the sans-IO core. New order-book implementations are
// ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
// satisfy the frozen `OrderBook` trait without changing this file. If a later
// phase appears to need a change here, the design is wrong — STOP and ask.
// The single permitted future edit is the two-line wiring of the Phase 5 `FlatBook`.
// See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.

//! `book` — the sans-IO limit-order-book core (the `core` of the kickoff brief,
//! renamed to avoid shadowing Rust's built-in `core` crate).
//!
//! INVARIANTS (locked in Phase 0, enforced for the life of the repo):
//! - No floating point anywhere in this crate. Prices and quantities are integers.
//! - No I/O, no async, no allocation in the hot path, no third-party dependencies.
//! - The float-string -> integer-tick conversion happens exactly ONCE, at the
//!   recorder edge (Phase 3). Nothing downstream of the corpus ever sees a float.
#![forbid(unsafe_code)]

mod book;
mod btree;
mod event;
mod price;
mod rev_vec;
mod sorted_vec;

pub use book::OrderBook;
pub use btree::BTreeBook;
pub use event::{BookEvent, EventKind, Side};
pub use price::{Px, Qty};
pub use rev_vec::RevVecBook;
pub use sorted_vec::SortedVecBook;

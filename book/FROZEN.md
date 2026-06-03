# `book` v1 — FROZEN

**Frozen:** 2026-06-03, as the final act of Phase 2, after the differential oracle
(`tests/oracle.rs`) went green across the hand-verified scenario, 8 seeds × 50,000
generated events, and every enumerated adversarial edge case.

**Tag:** `book-v1-frozen`

`book` is the sans-IO limit-order-book core — the stable contract every later phase
(the Phase 4 sweep, the Phase 6/7 primitives, the Phase 8 engine) builds on. It is now
immutable, exactly as the Rust-Tcp-Server `core` drove all eleven server models unchanged.

## Frozen files (immutable)

| File | Role |
|---|---|
| `src/price.rs`      | tick types `Px(i64)` / `Qty(i64)` (the contract) |
| `src/event.rs`      | `Side` / `EventKind` / `BookEvent` L2 event model (the contract) |
| `src/book.rs`       | the `OrderBook` trait (the contract) |
| `src/btree.rs`      | `BTreeBook` — `BTreeMap` baseline impl |
| `src/sorted_vec.rs` | `SortedVecBook` — ascending Vec, binary-search impl |
| `src/rev_vec.rs`    | `RevVecBook` — best-first Vec, linear-scan impl |
| `src/lib.rs`        | crate root + module wiring + re-exports |
| `tests/common/mod.rs` | shared `shared_scenario()` fixture |
| `tests/oracle.rs`   | the differential correctness oracle |

## Implementations behind the frozen `OrderBook` trait

- `BTreeBook` — `std::collections::BTreeMap` baseline (pointer-chasing, the slow anchor).
- `SortedVecBook` — contiguous `Vec`, both sides ascending, located by binary search.
- `RevVecBook` — contiguous `Vec`, best-first storage, located by linear scan.

The oracle proves these three are **observationally identical** after every event for any
input sequence; internal representation differs, observable behaviour does not.

## The additive-only rule

The **only** permitted future modification to `book` is Phase 5's flat-array
implementation:

1. a **new** file `src/flat.rs` (`FlatBook`),
2. **exactly two lines** added to `src/lib.rs` — `mod flat;` and `pub use flat::FlatBook;`,
3. an extension of `tests/oracle.rs` to drive the fourth impl through the same oracle.

No existing file's logic may change. If a later phase appears to require any other edit to
`book`, the design is wrong — **STOP and ask the owner.**

## Continuous protection

The oracle is an integration test, so **every later phase's `cargo test` green gate
re-runs it** on every build. A frozen-code regression — or a new impl that diverges from the
contract — cannot pass CI silently. The freeze is enforced by the test suite, not by trust.

# phase1-spec.md — Event Model + `OrderBook` Trait + BTreeMap Baseline

*Authoritative spec for Phase 1, split into two autonomous Claude Code sessions. Subordinate to `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md`, and the root `CLAUDE.md`. One session = one (or more) green commit(s) = explicit STOP. Nothing here authorizes Phase 2 or later.*

---

## 0. What Phase 1 delivers

The `book` crate gains its event vocabulary, its central abstraction, and its first concrete order book:

- `Side`, `EventKind`, `BookEvent` — the frozen-after-Phase-2 event model (L2, fixed `repr(C)` layout for the Phase 3 corpus).
- `OrderBook` trait — the one abstraction the four implementations (Phases 1/2/5) satisfy.
- `BTreeBook` — the baseline implementation over `std::collections::BTreeMap`, fully unit-tested against a hand-verified sequence.

After Phase 1, `book` still has **zero third-party dependencies** and `#![forbid(unsafe_code)]` still holds. `BTreeBook` is the slow baseline that the Vec and flat-array variants must later beat; it exists to be measured against, and to anchor the Phase 2 differential correctness oracle.

---

## 1. Preconditions — read before touching anything

1. `docs/specs/kickoff-brief.md` §2.1 (the shootout) and §3 (DoD).
2. `docs/specs/phase0-spec.md` §6 (the locked tick types) and §10 (Phase 1 was an explicit non-goal of Phase 0 — now it is in scope).
3. Root `CLAUDE.md` (the ban list, the freeze rule, session discipline).
4. The current `book/src/lib.rs` (contains `Px`, `Qty` from Phase 0).

---

## 2. Binding architectural decisions (read in full; these are not suggestions)

### 2.1 The book is **L2 (price-level aggregated)**, not L3 (per-order)
The shootout in the kickoff brief compares **price-level containers** (`BTreeMap<Px, Qty>` → sorted `Vec` → reverse-sorted `Vec` + linear scan → flat price-tick array). An L3 (market-by-order) book would force every implementation to additionally carry an `OrderId → (Px, Side)` index; that index — not the price-level container — would dominate the hot path and confound the crossover measurement that is the entire point of the artifact. We build L2. A price level holds a single aggregate `Qty`. (Rejected: L3/MBO — higher nominal realism, but it wrecks the clean container comparison and balloons scope. Not justified by the thesis.)

### 2.2 Event vocabulary is `Level / Trade / Clear`, not `Add / Cancel / Modify / Trade`
For an L2 absolute-quantity book, the brief's Add/Cancel/Modify collapse into one **absolute** `Level` update: "the new aggregate quantity at `(side, px)` is `qty`; `qty == 0` removes the level." This is exactly what venue diff-depth feeds (Binance, our Phase 3 recorder target) emit. Keeping Add/Cancel/Modify as separate variants would force the recorder to *infer* the operation from quantity deltas — stateful and error-prone. `Trade` is retained (execution print; updates a last-trade cache, does not mutate levels). `Clear` resets the book at a snapshot boundary. (Rejected: delta semantics — some feeds send deltas, but absolute is simpler and matches our target; delta→absolute conversion, if ever needed, happens once at the recorder edge.)

### 2.3 `apply` takes `&BookEvent` (by reference)
The Phase 3 replay iterator yields references into the memory-mapped corpus. By-ref `apply` is zero-copy on the hot path; by-value would copy 40 bytes per event in the tight loop for no benefit.

### 2.4 The harness drives implementations by **monomorphization**, never `dyn`
Every consumer of `OrderBook` in a measured path (the Phase 2 oracle, the Phase 4 harness, the Phase 8 engine) uses generics — `fn run<B: OrderBook>(...)` — so the compiler monomorphizes and inlines `apply`. A `dyn OrderBook` vtable indirection in the hot loop is disqualifying. The trait is designed for static dispatch; object safety is a non-goal.

### 2.5 Freeze timeline
The event model and the `OrderBook` trait are **provisionally locked** at the end of Phase 1 and **frozen** only after the Phase 2 differential oracle passes. Phase 2 (adding two more impls) is the last window in which a forced trait change is permitted. Plan as if frozen; if Phase 2 reveals a genuinely required signature change, that is the only allowed exception, and it must be recorded.

### 2.6 The book is a dumb container
The L2 book stores whatever levels it is told to store. It does **not** police crossed books (bid ≥ ask can occur transiently in real feeds), does not validate monotonic sequence numbers, and does not reject negative prices. Validation belongs at the recorder edge (Phase 3), not in the hot apply loop. Keep `apply` branch-light.

---

## 3. Target module layout (end of Phase 1)

```
book/src/
├── lib.rs       # crate root: #![forbid(unsafe_code)], module decls, public re-exports
├── price.rs     # Px, Qty (relocated from Phase-0 lib.rs; representation UNCHANGED)
├── event.rs     # Side, EventKind, BookEvent          [Session 1.1]
├── book.rs      # OrderBook trait                       [Session 1.1]
└── btree.rs     # BTreeBook impl                        [Session 1.2]
```

Relocating `Px`/`Qty` into `price.rs` is recommended hygiene as the crate grows; it is a non-breaking move (representation locked in Phase 0 stays identical, tests move with it). **Hard requirement:** the public paths `book::Px`, `book::Qty`, `book::Side`, `book::EventKind`, `book::BookEvent`, `book::OrderBook`, `book::BTreeBook` must all resolve via re-export. If you prefer to leave `Px`/`Qty` in `lib.rs`, that is acceptable provided the public paths hold.

---

## 4. SESSION 1.1 — Event model + `OrderBook` trait

**Goal:** lock the L2 event vocabulary and the central abstraction, with type-level tests. No order book implementation in this session.

### 4.1 Deliverables
- `book/src/event.rs` — `Side`, `EventKind`, `BookEvent` + constructors + a layout test.
- `book/src/book.rs` — the `OrderBook` trait.
- `book/src/price.rs` — `Px`/`Qty` relocated (optional but recommended).
- `book/src/lib.rs` — module decls + re-exports.

### 4.2 Reference implementation — `book/src/event.rs`

```rust
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
```

### 4.3 Reference implementation — `book/src/book.rs`

```rust
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
```

### 4.4 Reference — `book/src/lib.rs`

```rust
//! `book` — sans-IO L2 limit-order-book core (the kickoff brief's `core`).
#![forbid(unsafe_code)]

mod book;
mod event;
mod price;

pub use book::OrderBook;
pub use event::{BookEvent, EventKind, Side};
pub use price::{Px, Qty};
```

### 4.5 Session 1.1 green gate (every box required)
- [ ] `cargo build --workspace --all-targets` passes.
- [ ] `cargo test --workspace` passes (Px/Qty tests + the three event tests).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] Public paths resolve: a throwaway `cargo doc -p book --no-deps` (or a `use book::{Side, EventKind, BookEvent, OrderBook};` in a test) confirms the re-exports.
- [ ] Grep gate still holds (no `f64`/`tokio`/`redis`/… in tracked `.rs`).
- [ ] `#![forbid(unsafe_code)]` still present in `book`.

### 4.6 Commit + STOP
Single commit:
```
feat(book): L2 event model (Side/EventKind/BookEvent) + OrderBook trait
```
(If you relocated `Px`/`Qty`, a preceding `refactor(book): move tick types into price.rs` commit is acceptable — but only at a green state.)

**STOP. Do not implement `BTreeBook`. That is Session 1.2.**

---

## 5. SESSION 1.2 — `BTreeBook` baseline + behavioral test suite

**Goal:** the first concrete `OrderBook`, proven correct against a hand-verified sequence. This is the baseline the Vec/flat-array variants must beat and the oracle reference for Phase 2.

### 5.1 Deliverables
- `book/src/btree.rs` — `BTreeBook` + the behavioral test suite.
- `book/src/lib.rs` — add `mod btree;` and `pub use btree::BTreeBook;`.

### 5.2 Reference implementation — `book/src/btree.rs`

```rust
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
```

### 5.3 Required behavioral tests (in `btree.rs`)

Implement **all** of the following. The hand-verified scenario is mandatory and is reused verbatim as the Phase 2 oracle fixture, so keep it as a standalone `const`/helper that returns the event vector.

1. **Empty book:** `best_bid`/`best_ask` are `None`; `depth(Bid)==0`, `depth(Ask)==0`; `last_trade` is `None`; `top_n` returns 0.
2. **Single bid / single ask:** best reflects it with correct qty.
3. **Ordering:** multiple bids → `best_bid` is the highest px; multiple asks → `best_ask` is the lowest px.
4. **Absolute set / last-write-wins:** two `Level` updates at the same px → the later qty is observed.
5. **Removal:** `Level` with `qty == 0` removes the level; removing a non-existent level is a no-op (no panic, depth unchanged).
6. **Trade isolation:** a `Trade` event updates `last_trade` and leaves both sides' levels and depths untouched.
7. **`top_n` short buffer:** buffer smaller than depth returns `out.len()` and fills best-first in order.
8. **`top_n` long buffer:** buffer larger than depth returns `depth` and leaves the tail of `out` untouched.
9. **Clear:** after `Clear`, both sides empty, `last_trade` is `None`.
10. **Hand-verified scenario (mandatory, exact):**

```
empty
Clear                          -> empty (no-op)
Level Bid 100 = 5              -> best_bid (100,5)
Level Bid  99 = 3             -> best_bid (100,5),  depth(Bid)=2
Level Bid 101 = 2             -> best_bid (101,2),  depth(Bid)=3
Level Ask 103 = 4             -> best_ask (103,4)
Level Ask 102 = 1             -> best_ask (102,1),  depth(Ask)=2
Trade Ask 102 = 1              -> last_trade (102,1,Ask); depths unchanged (Bid=3, Ask=2)
Level Ask 102 = 0             -> best_ask (103,4),  depth(Ask)=1
Level Bid 101 = 0             -> best_bid (100,5),  depth(Bid)=2
top_n(Bid, buf[3]) == 2 and buf[..2] == [(100,5),(99,3)]
top_n(Ask, buf[1]) == 1 and buf[..1] == [(103,4)]
Clear                          -> best_bid None, best_ask None, last_trade None, depths 0
```

### 5.4 Session 1.2 green gate (every box required)
- [ ] `cargo build --workspace --all-targets` passes.
- [ ] `cargo test --workspace` passes (all ten behavioral tests + everything from 1.1).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] Grep gate still holds; `#![forbid(unsafe_code)]` still present in `book`.
- [ ] `book` still has zero third-party dependencies (`cargo tree -p book` shows only `book`).

### 5.5 Commit + STOP
Single commit at the green gate:
```
feat(book): BTreeMap baseline order book + hand-verified behavioral suite
```
**Optional checkpoint commit** (only if the session approaches your token/time window after the impl compiles green but before the full suite is done — never commit red):
```
feat(book): BTreeBook impl over BTreeMap (behavioral suite follows)
```
then resume with:
```
test(book): hand-verified BTreeBook scenario + edge cases
```

**STOP. Do not begin Phase 2 (sorted-Vec / reverse-Vec impls or the differential oracle).**

---

## 6. Phase 1 Definition of Done (after both sessions)

- [ ] `book` exposes `Side`, `EventKind`, `BookEvent` (L2, `repr(C)`, 40-byte locked layout) and the `OrderBook` trait.
- [ ] `BTreeBook` implements `OrderBook` and passes all ten behavioral tests including the exact hand-verified scenario.
- [ ] Event model + trait are provisionally locked (final freeze after Phase 2's oracle).
- [ ] `book`: zero third-party deps; `#![forbid(unsafe_code)]`; grep gate clean.
- [ ] All three gates (build / test / clippy `-D warnings`) green at each commit.
- [ ] Two (or three, if a checkpoint was used) meaningful conventional-commit messages on `main`.

---

## 7. Non-goals (do NOT do in Phase 1)

- No `SortedVecBook`, no `RevVecBook`, no flat-array book (Phases 2, 5).
- No differential oracle (Phase 2).
- No `dyn OrderBook`, no benchmarking, no `bench/` code, no numbers (Phase 4).
- No `feed`/corpus/recorder work, no tokio, no serde (Phase 3).
- No seqlock, no SPMC ring (Phases 6, 7).
- No third-party dependency added to `book`.
- No crossed-book validation, no sequence-gap detection in `apply` (the book is a dumb container; validation is Phase 3's recorder-edge job).

If a task feels like it belongs to a later phase, it does. Leave it.

---

## 8. Per-session completion report (print before each STOP)

Three lines: (1) files added/changed and the public items now exported; (2) the gate results (build/test/clippy) and test count; (3) the commit hash(es). Then STOP.

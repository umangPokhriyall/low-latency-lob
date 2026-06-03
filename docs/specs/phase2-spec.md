# book (core) — Phase 2 Specification: Vec Implementations, the Differential Oracle, and the Freeze

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md`, `docs/specs/phase1-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 2 spec.** Phase 1 (commit `6eccea5`) delivered the L2 event model, the `OrderBook` trait, and the `BTreeBook` baseline.
**Scope:** the second and third order-book implementations — `SortedVecBook` (binary search) and `RevVecBook` (best-first + linear scan) — the differential correctness oracle that proves all three are observationally identical, and the **freeze of the `book` crate**.
**Audience:** Claude Code. Authoritative. After this phase, `book` is immutable except for the single additive Phase 5 implementation.

---

## 1. Phase 2 in one paragraph

Phase 1 produced one order book; the kickoff brief's thesis requires four, behind one trait, so the only honest claim about which data structure wins is one the telemetry forces. Phase 2 adds the two contiguous-`Vec` implementations that make the search-strategy axis explicit — binary search over a sorted buffer versus linear scan over a best-first buffer — and then builds the artifact that makes the comparison trustworthy: a **differential oracle** that drives `BTreeBook`, `SortedVecBook`, and `RevVecBook` through the same event sequences and asserts byte-identical observable state after every event. Only once the oracle is green across the hand-verified scenario, tens of thousands of seeded random events, and the enumerated adversarial edge cases is the `book` crate **frozen** — it must thereafter drive the Phase 4 sweep, the Phase 6/7 primitives, and the Phase 8 engine unmodified, exactly as the Rust-Tcp-Server `core` drove all eleven server models unchanged.

### 1.1 Frozen / reused / what this phase freezes
- **The contract is reused unmodified.** `Px`, `Qty` (`price.rs`), `Side`/`EventKind`/`BookEvent` (`event.rs`), and the `OrderBook` trait (`book.rs`) are consumed by the two new impls without change. If a new impl appears to need a trait or event change, the impl is wrong — STOP and ask the owner. (Phase 2 is the last window in which a forced contract change is even discussable; default to "no.")
- **`BTreeBook` is reused unmodified** as the oracle's reference impl.
- **At the end of this phase, the entire `book` crate is FROZEN** (§9). The one permitted future edit is Phase 5's additive flat-array impl (a new file + two lines in `lib.rs`).

---

## 2. Workspace additions & dependencies

```
book/src/sorted_vec.rs     # NEW — SortedVecBook (binary search)
book/src/rev_vec.rs        # NEW — RevVecBook (best-first + linear scan)
book/src/lib.rs            # EDIT — add two mods + two re-exports
book/tests/common/mod.rs   # NEW — shared test fixtures (scenario + edge cases)
book/tests/oracle.rs       # NEW — the differential oracle (integration test)
book/FROZEN.md             # NEW — the freeze manifest (§9), written in the final session
```

**Dependency additions: none.** `book` remains at **zero third-party dependencies, including dev-dependencies.** The oracle's randomness is a hand-rolled, seeded `SplitMix64` (§6.3) — not `rand`, not `proptest`, not `quickcheck`. `#![forbid(unsafe_code)]` continues to hold. `cargo tree -p book` must still show only `book`.

---

## 3. Representation & memory layout (the design rationale)

The four implementations differ in exactly one thing: how a side's price levels are stored and located. Everything else — event dispatch, `Trade`/`Clear` handling, the `last_trade` cache — is identical structure. This section fixes the two new representations and the layout reasoning behind them. **All performance statements here are hypotheses, owned by Phase 4 (§7); Phase 2 asserts only correctness.**

### 3.1 The level element
A price level is `(Px, Qty)` = `(i64, i64)` = **16 bytes**, alignment 8. A 64-byte cache line therefore holds **exactly 4 levels**. A side is a single contiguous heap buffer (`Vec`), so *k* levels occupy `ceil(k/4)` cache lines with no pointer indirection and full hardware-prefetcher friendliness.

### 3.2 `SortedVecBook` — ascending, binary search
```
bids: Vec<(Px,Qty)>  strictly ASCENDING by price   -> best (highest) bid = last()
asks: Vec<(Px,Qty)>  strictly ASCENDING by price   -> best (lowest)  ask = first()
```
Both sides ascending lets one routine serve both. Locate by `binary_search_by_key`: O(log n) comparisons, but the access pattern is a **sequence of halving jumps** (index n/2, n/4, …) — for large *n* those jumps straddle different cache lines, and the comparisons are **data-dependent branches** the predictor cannot learn. Insert/remove is O(n) `memmove`. Best access is O(1) at a known end.

### 3.3 `RevVecBook` — best-first, linear scan
```
bids: Vec<(Px,Qty)>  strictly DESCENDING by price  -> best (highest) bid = [0]
asks: Vec<(Px,Qty)>  strictly ASCENDING  by price  -> best (lowest)  ask = [0]
```
Both sides store the **hot end at index 0**. Locate by linear scan from the front: O(k) where *k* is the distance from best to the touched level. For the dominant real-feed case — updates concentrated in the top few levels — the scan touches **1–2 cache lines**, runs a **loop-predictable branch**, and reads **sequentially** (ideal for the prefetcher). Insert/remove is O(n) `memmove`; best access is O(1) at index 0; and `top_n` is a straight forward copy on both sides (no reversal), so the read path is strictly simpler than `SortedVecBook`'s.

### 3.4 `BTreeBook` (baseline, already shipped) — for contrast
`std::collections::BTreeMap` nodes (B=6, up to 11 KV per node) are individually heap-allocated and scattered, so a lookup is a **pointer chase** of ~`log_11(n)` node hops, each a probable cache miss, and `best_bid`/`best_ask` (`iter().next_back()`/`next()`) are themselves O(log n) descents — a standing tax on the read path that Phase 8's seqlock snapshot will hammer.

### 3.5 Access-pattern summary (hypothesis table; Phase 4 confirms)
| Operation | `BTreeBook` | `SortedVecBook` | `RevVecBook` |
|---|---|---|---|
| update at top-of-book | log-depth pointer chase | log n halving jumps + branchy compare | 1–2 sequential steps, predictable branch |
| best bid/ask read | O(log n) descent | O(1) index | O(1) index `[0]` |
| insert new level | node ops, possible alloc | O(n) memmove | O(n) memmove |
| `top_n` (best-first) | ordered iter | bids need reversal | forward copy both sides |
| memory | scattered nodes + pointers | one contiguous buffer | one contiguous buffer |

---

## 4. `book/src/sorted_vec.rs` — `SortedVecBook`

Implement exactly this surface. Both sides ascending; one shared `update_ascending` routine; no copy-pasted per-side logic.

```rust
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
```

**Unit tests (in `sorted_vec.rs`, `#[cfg(test)]`):** run the shared scenario fixture (`common::shared_scenario`) — *no*, integration tests cannot import `common` from a unit module; instead the unit tests here cover Vec-specific properties, and the scenario is exercised via the oracle (§6). Required unit tests:
1. Ascending invariant holds after a churn of inserts/updates/removes (assert `bids`/`asks` windows are sorted and duplicate-free — expose via a `#[cfg(test)]`-only accessor or assert through `top_n`).
2. `binary_search` insert position correctness: inserting out-of-order prices yields a sorted ladder via `top_n`.
3. Removal via qty=0 shifts correctly; removing an absent price is a no-op.
4. Reallocation churn: insert > 1 page of levels, remove half, assert ladder correctness.

---

## 5. `book/src/rev_vec.rs` — `RevVecBook`

Best-first storage, linear-scan location, uniform forward `top_n`. One shared `update_best_first` routine parameterized by direction; no per-side copy-paste.

```rust
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
```

**Worked insertion check (include as a comment/test):** bids `[101,100,99]` (descending). Insert `px=100`→ `position(p<=100)` = index 1, `vec[1].0==100` ⇒ update in place. Insert `px=102` ⇒ `position(p<=102)`=0, `≠102` ⇒ insert at 0 → `[102,101,100,99]`. Insert `px=98` ⇒ `position(p<=98)`=`None` ⇒ push → `[101,100,99,98]`. All preserve descending order.

**Unit tests (in `rev_vec.rs`):**
1. Best is always at index 0 on both sides after churn.
2. Descending (bids) / ascending (asks) invariants hold after randomized-by-hand insert/update/remove.
3. New-best, mid-ladder, and new-worst insertions land at the correct index (the worked check above as assertions).
4. Reallocation churn correctness, same as §4.

---

## 6. The differential oracle — `book/tests/oracle.rs`

This is the load-bearing artifact of Phase 2. It is an **integration test** (it exercises only the public API), so it validates the `OrderBook` contract at exactly the surface a consumer sees.

### 6.1 The contract
For **any** sequence of `BookEvent`s, `BTreeBook`, `SortedVecBook`, and `RevVecBook` produce **identical observable state** after every event. Observable state is the full public surface: `best_bid`, `best_ask`, `depth(Bid)`, `depth(Ask)`, the complete best-first ladders via `top_n`, and `last_trade`. Internal representation may differ; observable behaviour may not. A divergence is a correctness bug in at least one impl and **blocks the freeze**.

### 6.2 The observable snapshot
```rust
#[derive(PartialEq, Eq, Debug)]
struct Obs {
    best_bid: Option<(Px, Qty)>,
    best_ask: Option<(Px, Qty)>,
    depth_bid: usize,
    depth_ask: usize,
    bids: Vec<(Px, Qty)>, // FULL ladder, best-first
    asks: Vec<(Px, Qty)>, // FULL ladder, best-first
    last_trade: Option<(Px, Qty, Side)>,
}

fn observe<B: OrderBook>(b: &B) -> Obs {
    let (db, da) = (b.depth(Side::Bid), b.depth(Side::Ask));
    let mut bids = vec![(Px::ZERO, Qty::ZERO); db];
    let mut asks = vec![(Px::ZERO, Qty::ZERO); da];
    let nb = b.top_n(Side::Bid, &mut bids);
    let na = b.top_n(Side::Ask, &mut asks);
    assert_eq!(nb, db, "top_n(Bid)={nb} but depth(Bid)={db}");
    assert_eq!(na, da, "top_n(Ask)={na} but depth(Ask)={da}");
    Obs { best_bid: b.best_bid(), best_ask: b.best_ask(),
          depth_bid: db, depth_ask: da, bids, asks, last_trade: b.last_trade() }
}
```
Comparing the **full ladder** (not just the top level) is deliberate: it catches an impl with the right set of levels in the wrong order, and a `top_n` that mis-orders or mis-counts.

### 6.3 Deterministic randomness (hand-rolled, zero deps)
```rust
struct SplitMix64(u64);
impl SplitMix64 {
    fn new(seed: u64) -> Self { Self(seed) }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 { self.next_u64() % n } // modulo bias is fine for a fuzzer
}
```

### 6.4 The generator
Bias the distribution toward the hard cases: a narrow price band (so collisions, updates, and removals at the same price are frequent), `qty == 0` reachable (removals), and rare `Trade`/`Clear`.
```rust
const PRICE_BASE: i64 = 10_000;
const PRICE_BAND: i64 = 64;   // prices in [BASE-BAND, BASE+BAND]
const MAX_QTY:    i64 = 50;

fn gen_event(rng: &mut SplitMix64, seq: u64) -> BookEvent {
    let band = (2 * PRICE_BAND) as u64;
    let roll = rng.below(1000);
    if roll < 5 {                       // 0.5% Clear
        BookEvent::clear(seq, seq)
    } else if roll < 55 {               // 5% Trade (must not touch ladders)
        let side = if rng.below(2) == 0 { Side::Bid } else { Side::Ask };
        let px = Px(PRICE_BASE - PRICE_BAND + rng.below(band) as i64);
        let qty = Qty(1 + rng.below(MAX_QTY as u64) as i64);
        BookEvent::trade(seq, seq, side, px, qty)
    } else {                            // ~94.5% Level (qty 0 => remove)
        let side = if rng.below(2) == 0 { Side::Bid } else { Side::Ask };
        let px = Px(PRICE_BASE - PRICE_BAND + rng.below(band) as i64);
        let qty = Qty(rng.below((MAX_QTY + 1) as u64) as i64);
        BookEvent::level(seq, seq, side, px, qty)
    }
}
```

### 6.5 Required tests
```rust
fn assert_agree(seed: u64, k: u64, a: &BTreeBook, b: &SortedVecBook, c: &RevVecBook) {
    let (oa, ob, oc) = (observe(a), observe(b), observe(c));
    assert_eq!(oa, ob, "BTree vs SortedVec diverged at seed={seed} k={k}");
    assert_eq!(oa, oc, "BTree vs RevVec   diverged at seed={seed} k={k}");
}
```
1. **`oracle_shared_scenario`** — replay `common::shared_scenario()` (the exact Phase-1 hand-verified sequence, lifted into `tests/common/mod.rs`) across all three; `assert_agree` after every event.
2. **`oracle_randomized`** — for each of `[1,2,3,5,8,13,21,0xDEAD_BEEF]`, run `ITERS = 50_000` generated events; cheap per-event check on `best_bid`/`best_ask`; full `assert_agree` every 64 events and at the end. On failure the seed + index reproduce it exactly. (`BOOK_ORACLE_ITERS` env override permitted for longer local runs; default committed at 50_000.)
3. **`oracle_negative_and_extreme_prices`** — feed `Px(-5)`, `Px(0)`, `Px(i64::MAX - 1)`, `Px(i64::MIN + 1)` on both sides; `assert_agree`. Proves ordering is integer-correct across the whole `i64` range.
4. **`oracle_crossed_book`** — drive bids above asks (transient crossing, legal for a dumb container); `assert_agree`. The book must store crossed state; it does not police it.
5. **`oracle_remove_absent_is_noop`** — qty=0 on empty and on absent prices; depths stay correct; `assert_agree`.
6. **`oracle_clear_then_rebuild`** — build, `Clear`, rebuild a different ladder; `assert_agree`.
7. **`oracle_realloc_churn`** — insert enough distinct levels to force several `Vec` growths, then remove a strided subset; `assert_agree`.

`tests/common/mod.rs` holds `pub fn shared_scenario() -> Vec<BookEvent>` plus any reused edge fixtures. (Accepted minor duplication: Phase 1's `btree.rs` keeps its own inline copy of the scenario as a standalone regression test; it is frozen as shipped and is not refactored to import the common fixture, since `src/` cannot see `tests/`.)

---

## 7. Performance hypotheses — to be falsified in Phase 4 (measure, never guess)

Phase 2 commits **no** performance numbers. The following are hypotheses the §3 layout reasoning produces; Phase 4's coordinated-omission-correct sweep confirms or kills each. They are recorded here so Phase 4 has a falsifiable target, not so they can be cited as results.

- **H1.** With updates concentrated in the top ~8 levels (the realistic feed case), `RevVecBook` dominates: an early-terminating sequential scan plus O(1) best access beats both binary search and the BTree pointer chase.
- **H2.** `SortedVecBook`'s binary search loses to `RevVecBook` at shallow/medium depth (branch mispredictions on data-dependent comparisons; cache jumps), and only overtakes it when touched levels are uniformly deep **and** *n* is large.
- **H3.** `BTreeBook` loses across realistic depths; it is competitive only at very large, uniformly-updated books, and pays a standing O(log n) tax on every best-bid/ask read.
- **H4.** The `RevVecBook`↔`SortedVecBook` crossover sits at some depth *D* and update-locality; locating *D* and plotting the interior-latency distributions on each side of it is Phase 4's headline artifact.

---

## 8. Engineering Standard — governs every file in this phase

1. **One abstraction, no copy-paste.** The three impls differ only in container + search. Per-side logic is a single direction-parameterized helper (`update_ascending`, `update_best_first`); `Trade`/`Clear`/`last_trade` handling is structurally identical across impls and must not drift.
2. **No allocation on the update path** beyond amortized `Vec` growth. `best_bid`/`best_ask`/`last_trade` allocate nothing; `top_n` writes only into the caller's buffer.
3. **No `unsafe` in `book`.** Safe indexing and iterators only; `#![forbid(unsafe_code)]` stays.
4. **No third-party dependency, including dev-deps.** Oracle randomness is the hand-rolled seeded `SplitMix64`. `cargo tree -p book` shows only `book`.
5. **Determinism and reproducibility.** Every oracle failure prints `seed` + event index + the diverging field, so any failure reproduces from one line. No wall-clock, no thread-nondeterminism, no unseeded randomness anywhere in tests.
6. **Correctness is the observable surface, checked across all impls** — never internal representation. The oracle is the definition of correct.
7. **The book is a dumb container.** No crossed-book policing, no sequence-gap detection, no negative-price rejection in `apply`. All impls must agree even on adversarial input.
8. **Measure, never guess.** Phase 2 states no performance claim as fact; §3/§7 reasoning is explicitly labelled hypothesis and deferred to Phase 4.
9. **The freeze is sacred (§9).** After the freeze commit, `book/src/*` is immutable except the single additive Phase 5 file. An apparent need to edit frozen code is a design error — STOP and ask the owner.
10. **Green-gate discipline.** `cargo build --workspace --all-targets`, `cargo test --workspace` (the oracle integration test included), and `cargo clippy --workspace --all-targets -- -D warnings` are all green before every commit. One session → one or more meaningful conventional commits → explicit STOP. Never commit red.

---

## 9. FREEZE `book`

Executed as the final act of Phase 2, **only after §6 is green**.

**What "frozen" means.** The files `book/src/price.rs`, `event.rs`, `book.rs`, `btree.rs`, `sorted_vec.rs`, `rev_vec.rs`, and the test files are immutable. The frozen `OrderBook` trait, event model, and tick types are the stable contract every later phase builds on. The **only** permitted future modification to `book` is Phase 5's flat-array implementation: a **new** file `book/src/flat.rs`, plus **exactly two lines** added to `book/src/lib.rs` (`mod flat;` and `pub use flat::FlatBook;`), plus an extension of `tests/oracle.rs` to include the fourth impl. No existing file's logic may change. If a later phase appears to require any other change, the design is wrong — STOP and ask.

**The freeze ritual (final session):**
1. Add this header to the top of every frozen `book/src/*.rs` file:
   ```rust
   // FROZEN — book v1 (Phase 2). Do not modify.
   // The `book` crate is the sans-IO core. New order-book implementations are
   // ADDITIVE (new file + new pub export + extend tests/oracle.rs) and must
   // satisfy the frozen `OrderBook` trait without changing this file. If a later
   // phase appears to need a change here, the design is wrong — STOP and ask.
   // See docs/specs/phase2-spec.md §9 and git tag `book-v1-frozen`.
   ```
   `lib.rs` gets the same header with one added line: *"The single permitted future edit is the two-line wiring of the Phase 5 `FlatBook`."*
2. Write `book/FROZEN.md`: the frozen-file manifest, the freeze date, the list of impls behind the trait, the additive-only rule, and the statement that `cargo test` re-runs the oracle on every build so any later drift is caught immediately.
3. Commit, then tag: `git tag -a book-v1-frozen -m "book core frozen after Phase 2 differential oracle"`.

**Continuous protection.** Because the oracle is an integration test, every later phase's `cargo test` green gate re-runs it; a frozen-code regression cannot pass CI silently.

---

## 10. Phase 2 Definition of Done

1. `SortedVecBook` (§4) and `RevVecBook` (§5) implemented behind the unmodified `OrderBook` trait, each with its impl-specific unit tests green; no copy-pasted per-side logic.
2. The contract files (`price.rs`, `event.rs`, `book.rs`) and `btree.rs` are byte-for-byte unchanged from Phase 1 (`git diff 6eccea5 -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs` is empty).
3. The differential oracle (§6) is green: shared scenario + 8 seeds × 50,000 events + all enumerated edge cases (negative/extreme prices, crossed book, remove-absent, clear-then-rebuild, realloc churn). Failures are reproducible from a printed seed+index.
4. `book` has zero third-party dependencies including dev-deps (`cargo tree -p book` shows only `book`); `#![forbid(unsafe_code)]` holds; the grep gate holds.
5. Performance: §7 hypotheses recorded as hypotheses; **no** performance numbers committed (those are Phase 4).
6. FREEZE executed (§9): freeze headers on all frozen files, `book/FROZEN.md` manifest present, `book-v1-frozen` tag created.
7. `CLAUDE.md` updated per Appendix A.
8. `cargo build` / `clippy -D warnings` / `test` clean at every commit; four meaningful conventional commits on `main`.

After Phase 2, `book` is finished and frozen. Next is Phase 3 (`feed`: recorder + replay corpus).

---

# Appendix A — `CLAUDE.md` update for Phase 2

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, the four-impl shootout, DoD culture
- docs/specs/phase0-spec.md    — workspace, tick types, guardrail
- docs/specs/phase1-spec.md    — event model, OrderBook trait, BTreeBook baseline
- docs/specs/phase2-spec.md    — CURRENT: SortedVecBook, RevVecBook, oracle, FREEZE

## Hard rules
1. `book` is FROZEN after Phase 2 (tag `book-v1-frozen`). Frozen files in
   book/src/ are immutable. The ONLY permitted future edit is Phase 5's additive
   FlatBook: new file book/src/flat.rs + two lines in lib.rs + extend
   tests/oracle.rs. No other change. Apparent need to edit frozen code = design
   error -> STOP and ask.
2. book has ZERO third-party dependencies, including dev-deps. No rand/proptest/
   quickcheck — oracle randomness is the in-repo seeded SplitMix64.
3. #![forbid(unsafe_code)] holds in book. No async, no I/O, no allocation on the
   update path beyond amortized Vec growth.
4. Correctness is defined by the differential oracle (observable trait surface
   across all impls), not by internal representation. The book is a dumb
   container: no crossed-book policing, no sequence validation.
5. Measure, never guess: commit NO performance numbers in book. Layout reasoning
   is hypothesis, owned by Phase 4.

## Scope discipline
Work ONLY on the given session. End with cargo build + clippy -D warnings + test
(oracle included) green, a meaningful commit, a listed change summary, and STOP.
```

---

# Appendix B — Claude Code execution plan (4 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | SortedVecBook | `book/src/sorted_vec.rs` (§4) + unit tests + lib wiring | build/clippy/test green; ascending invariants pass |
| 2 | RevVecBook | `book/src/rev_vec.rs` (§5) + unit tests + lib wiring | build/clippy/test green; best-first invariants pass |
| 3 | Differential oracle | `tests/common/mod.rs` + `tests/oracle.rs` (§6) | shared + 8×50k random + all edge cases green |
| 4 | FREEZE + DoD | freeze headers, `book/FROZEN.md`, tag, `CLAUDE.md` (§9, App. A) | DoD §10 verified item by item; tag created |

Sessions 1 and 2 are small and independent; keep them separate for clean commits and a safety margin. Session 3 is the subtle one — budget the window for it. Session 4 is a short ceremony that also runs the full DoD check.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase2-spec.md` §1–§4, §8. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement `book/src/sorted_vec.rs` exactly per §4 (ascending both sides, one shared `update_ascending`, binary search), wire `mod sorted_vec;` + `pub use sorted_vec::SortedVecBook;` into `lib.rs`, and add the §4 unit tests. Do not modify any frozen contract file or `btree.rs`. Run `cargo build --workspace --all-targets`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`. Commit `feat(book): SortedVecBook (binary-search sorted Vec)`. List changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase2-spec.md` §5, §8. Execute **Session 2 only**: implement `book/src/rev_vec.rs` exactly per §5 (best-first storage, one shared direction-parameterized `update_best_first`, linear scan, forward `top_n` both sides), wire it into `lib.rs`, add the §5 unit tests including the worked insertion assertions. Touch no frozen file. Run the three gates. Commit `feat(book): RevVecBook (best-first linear-scan Vec)`. List changes, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase2-spec.md` §6, §8. Execute **Session 3 only**: create `book/tests/common/mod.rs` (with `shared_scenario()`), and `book/tests/oracle.rs` implementing the differential oracle exactly per §6 — `SplitMix64`, `Obs`/`observe`, the generator, and all seven required tests (shared scenario; 8 seeds × 50_000 events; negative/extreme prices; crossed book; remove-absent; clear-then-rebuild; realloc churn). No third-party deps. Run the three gates; the oracle must be green. Commit `test(book): differential oracle across BTree/Sorted/Rev impls`. Report any divergences found and fixed, list changes, STOP.

**Session 4**
> Read `CLAUDE.md` and `phase2-spec.md` §9–§10, Appendix A. Execute **Session 4 only**: perform the freeze ritual — add the §9 FROZEN header to every `book/src/*.rs` file (and the lib.rs variant), write `book/FROZEN.md`, confirm `git diff 6eccea5 -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs` is empty, run the three gates, commit `chore(book): freeze book v1 (contract + 3 impls + oracle)`, then `git tag -a book-v1-frozen -m "book core frozen after Phase 2 differential oracle"`. Finally verify Phase 2 DoD §10 item by item and report each. STOP. The `book` crate is complete.

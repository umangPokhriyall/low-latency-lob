# book + bench — Phase 5 Specification: `FlatBook`, the Four-Way Oracle, the Re-Run, and the Final Crossover Verdict

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md`, `docs/specs/phase1-spec.md`, `docs/specs/phase2-spec.md`, `docs/specs/phase3-spec.md`, `docs/specs/phase4-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 5 spec.** It closes the order-book shootout.
**Scope:** the fourth and final order-book implementation — `FlatBook` (flat price-tick array, O(1) update) — added as the **single permitted edit to the frozen `book` crate**; the extension of the differential oracle to four impls; the re-run of the entire Phase 4 harness across all four; and the **final, honest crossover verdict** grounded in the surprising Phase 4 results.
**Audience:** Claude Code. Authoritative. This phase touches frozen code under a tightly-scoped exception (§3) and commits numbers under the §7 Writing Standard.

---

## 1. Phase 5 in one paragraph

The Phase 4 sweep refuted the tidy story. The crossover turned out **locality-gated, not depth-gated**, and — the result that matters — the real-market throughput **inverted** the synthetic ranking: on the recorded BTCUSDT sample `BTreeBook` sustained ~23 Mev/s while `RevVecBook` collapsed to ~4.5 Mev/s, because real books are deep and their touch distribution is not top-of-book-concentrated, which is precisely the regime that destroys a top-of-book linear scan. That collapse is the reason the flat array exists. Phase 5 adds `FlatBook` — price levels in a flat array indexed by tick offset, giving O(1) update independent of depth and locality at the cost of memory proportional to the price *span* and a best-removal probe — extends the differential oracle to prove it is observationally identical to the other three within its domain, re-runs the full harness across all four impls, and writes the final verdict that names which structure to use when, sourced entirely to the committed CSVs. After Phase 5 the shootout is complete and the `book` crate's story is settled.

### 1.1 What Phase 4 measured (the committed facts that drive this phase)
From `bench/results/` (cite these; do not restate as new claims):
- **Crossover is locality-gated** (`service_sweep.csv`): `D* = 256` under Concentrated touches, `D* = 2` under Uniform touches. Linear scan (rev) is near-free when touches sit at the top; it degrades to ~686 ns/op at depth 2048 under Uniform — a ~49× loss to binary search's ~14 ns.
- **Real-data inversion** (`throughput.csv`): synthetic-concentrated favors the Vecs (~76.7 Mev/s lead), but the real BTCUSDT sample inverts it — `BTreeBook` ~23.1 Mev/s vs `RevVecBook` ~4.5 Mev/s.
- **best_bid read tax is below the 7 ns clock floor** and not resolvable (`read_path.csv`); the tree's read cost only surfaces on `top_n_full` (btree ~3517 ns vs vec ~620 ns at depth 2048).
- **Saturation** (`sustained.csv`): all three sustain ~20 Mev/s and saturate ~50 Mev/s; the BTree tail jumps 108 ns → 1.71 ms past the knee.
- **Hypotheses:** H1 refined (rev tied with sorted at the floor; both beat btree shallow), **H2 refuted as stated** (the crossover is locality-gated, not large-n), H3 split (btree loses on apply; its best-access tax is below the floor), H4 confirmed.

These facts make `FlatBook` the structurally-correct answer to the inversion, and they define what the verdict must explain.

### 1.2 Frozen status & the single permitted exception
`book` is frozen (`book-v1-frozen`). Phase 5 makes **only** the additive edits the freeze explicitly carved out (phase2-spec §9):
1. **NEW** file `book/src/flat.rs` (the `FlatBook` impl).
2. **+2 lines** in `book/src/lib.rs`: `mod flat;` and `pub use flat::FlatBook;` (the exception named in lib.rs's freeze header).
3. **Extend** `book/tests/oracle.rs` to the four-way comparison + a `FlatBook` domain test (oracle extension is the permitted additive test action).
4. **Update** `book/FROZEN.md` to record `FlatBook` as the additive impl and the four-way oracle.
No other frozen file's logic changes. `git diff book-v1-frozen -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs book/src/sorted_vec.rs book/src/rev_vec.rs` MUST be empty. The bench changes are unrestricted (`bench` is not frozen).

---

## 2. `FlatBook` design — the flat price-tick array

### 2.1 Thesis
Index each side by `tick - base` into a contiguous array of `Qty`; a slot's value is the aggregate quantity at that tick (`Qty::ZERO` = empty). Update is then a single direct-indexed write — **O(1), independent of depth and of where in the book the touch lands.** This is immune to both Phase 4 killers (deep books, non-top-concentrated touches). The price paid is memory proportional to the price **span** (not the occupied-level count) and a probe when the current best is removed.

### 2.2 Representation
```rust
//! `FlatBook` — price levels in a flat array indexed by tick offset from a base.
//! O(1) update by direct index, immune to depth and touch locality. Cost: memory
//! ~ price SPAN, plus a best-removal probe and a sparse-ladder top_n scan.
//! Applicable to BOUNDED price ranges only (see §2.6). This is the single impl
//! added after the book freeze; it is NOT marked FROZEN.

use crate::{BookEvent, EventKind, OrderBook, Px, Qty, Side};

/// Hard cap on representable span (ticks). Beyond it the flat array is the wrong
/// structure; panic loudly rather than allocate unbounded memory.
const MAX_SPAN: usize = 8 * 1024 * 1024;   // 8M ticks -> two 64 MiB arrays
/// Half-span pre-allocated around the first observed price (keeps recenters rare).
const INIT_HALF_SPAN: usize = 4096;

#[derive(Default, Debug)]
pub struct FlatBook {
    base: i64,                    // tick at index 0 (valid once `inited`)
    inited: bool,
    bid_qty: Vec<Qty>,            // bid_qty[i] = qty at tick (base + i); ZERO = empty
    ask_qty: Vec<Qty>,            // parallel ask-side array
    best_bid_idx: Option<usize>,  // highest occupied bid index
    best_ask_idx: Option<usize>,  // lowest occupied ask index
    last_trade: Option<(Px, Qty, Side)>,
}
```
Two separate 8-byte/tick arrays (not one 16-byte combined array): a single-side scan (`best`, `top_n`) then touches only that side's array, denser per cache line. A tick may carry both a bid and an ask quantity simultaneously (crossed book is legal for a dumb container) — the parallel arrays handle that naturally.

### 2.3 Update (the O(1) hot path)
For a `Level` at `(side, px, qty)`:
1. `ensure_range(px)` — initialize on first use (set `base`, allocate `[ -INIT_HALF_SPAN, +INIT_HALF_SPAN ]` around `px`); else if `px` is outside `[base, base+len)`, **recenter/grow** to cover it (§2.5).
2. `i = (px - base) as usize`. Set `side_arr[i] = qty`.
3. Maintain the cached best:
   - **insert/raise** (`qty != 0`): bids — `if best_bid_idx.is_none() || i > best_bid_idx { best_bid_idx = Some(i) }`; asks — `… || i < best_ask_idx …`.
   - **remove** (`qty == 0`): clear the slot; if `i` was the cached best, **probe** toward the worse side for the next occupied slot (bids: scan `i-1 .. 0`; asks: scan `i+1 .. len`), updating or clearing the cached best. This probe is the flat array's characteristic cost and must be measured honestly, not hidden.

### 2.4 Reads
- `best_bid`/`best_ask`: O(1) from the cached index → `(Px(base + idx), arr[idx])`.
- `top_n(side, out)`: walk from the cached best toward the worse side, emitting occupied slots until `out` is full or the array end — bids decreasing index, asks increasing. Over a **sparse** span this scan visits empty slots and is the flat array's weak spot (contrast the Vecs' contiguous iteration; cf. Phase 4's `top_n_full` finding).
- `depth(side)`: maintain a running occupied-count per side (increment on first occupation of a slot, decrement on removal) so `depth` stays O(1) and matches the other impls exactly.

### 2.5 Recenter / grow (rare, amortized)
On an out-of-range `px`: compute the new span = union of `[base, base+len)` with `px`, plus a margin; if it exceeds `MAX_SPAN`, panic (§2.6). Otherwise allocate new arrays, copy existing quantities to their new offsets, update `base`, and shift the cached best indices by the base delta. O(span); rare because the initial half-span absorbs normal movement and the committed corpora are range-bounded.

### 2.6 The bounded-range tradeoff (state it; do not hide it)
The flat array is applicable to **bounded** price ranges only. A span exceeding `MAX_SPAN` panics with a clear message ("FlatBook span exceeds MAX_SPAN; the flat array is for bounded ranges — use a tree/Vec book for unbounded ranges"). This is the structure's defining limitation and a documented strength of the analysis, not a bug. Consequently `FlatBook` participates in the oracle **within its domain** (the bounded generator band); the unbounded extreme-`i64` test stays a three-impl test, and a dedicated `FlatBook` domain test (§4) covers its rebase behavior and its out-of-domain contract.

### 2.7 `Default` / `Clear`
`Default` = empty, `inited = false` (lazy init on the first `Level`). `Clear` zeroes the occupied content, resets both cached best indices and both depth counters to empty, and **retains** the allocated capacity (no churn); `inited`/`base` may be retained. A `Trade` on an empty book just sets `last_trade`.

---

## 3. The freeze-respecting edits (exact, verifiable)

1. Add `book/src/flat.rs` per §2.
2. Add to `book/src/lib.rs`, in the slot its freeze header authorizes, exactly:
   ```rust
   mod flat;
   pub use flat::FlatBook;
   ```
3. Extend `book/tests/oracle.rs` per §4.
4. Update `book/FROZEN.md`: note `FlatBook` added as the single additive impl; the oracle now covers four impls; `MAX_SPAN`/domain documented.

**Verification gate (run and report):** `git diff book-v1-frozen -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs book/src/sorted_vec.rs book/src/rev_vec.rs` is empty (frozen logic untouched); the only `book/` changes are `flat.rs` (new), the two `lib.rs` lines, `oracle.rs` (extended), `FROZEN.md` (updated).

---

## 4. Four-way differential oracle

Extend `book/tests/oracle.rs` (the permitted additive edit):
1. **Add `FlatBook` to every bounded-band test** — `oracle_shared_scenario`, `oracle_randomized` (all seeds × 50k), `oracle_crossed_book`, `oracle_remove_absent_is_noop`, `oracle_clear_then_rebuild`, `oracle_realloc_churn`. Extend `assert_agree` to compare `BTreeBook` vs each of `SortedVecBook`, `RevVecBook`, `FlatBook`. Generator band (`PRICE_BASE ± PRICE_BAND` = 10000 ± 64) is well inside `FlatBook`'s domain, so this is a direct four-way equality check.
2. **Keep `oracle_negative_and_extreme_prices` as a three-impl test** (BTree/Sorted/Rev) — `i64::MIN+1 .. i64::MAX-1` exceeds any flat array; `FlatBook` is out of domain by design (§2.6), and that is correct, not a failure.
3. **Add `flatbook_domain` tests:**
   - **Rebase correctness:** a sequence that deliberately crosses the initial half-span in both directions (first level near a `mid`, then levels at `mid + (INIT_HALF_SPAN + k)` and `mid - (INIT_HALF_SPAN + k)`), driving front-recenter and back-grow; assert `FlatBook` agrees with `BTreeBook` on the full observable ladder throughout.
   - **Out-of-domain contract:** a level whose span would exceed `MAX_SPAN` panics with the documented message (`#[should_panic]` or `catch_unwind`).

The oracle remains an integration test (public API only), zero third-party deps, seeded `SplitMix64`. It runs on every `cargo test`, so it permanently guards the four-way contract.

---

## 5. Harness re-run across four impls

`bench` is not frozen; wire `FlatBook` in and regenerate everything.
1. **Dispatch:** add `"flat" => run::<FlatBook>(…)` to every benchmark's impl match (`service.rs`, `read.rs`, `sustained.rs`, `throughput.rs`), `harness::for_impl`, and the CLI's impl list. Monomorphized — no `dyn`.
2. **Re-run all four benchmarks** across all four impls per the Phase 4 methodology (unchanged): `black_box`, measured-and-recorded clock floor, pinning, warmup, ≥1M samples/cell, CO-correct sustained loop. For `FlatBook` the service sweep must size the array to each depth's band (its memory grows with span, not depth — note this in provenance); the real-corpus throughput/sustained runs use the same `feed/corpus/btcusdt-sample.mdf`.
3. **Regenerate artifacts:** overwrite `service_sweep.csv`, `read_path.csv`, `sustained.csv`, `throughput.csv` with the four-impl data; re-export `.hgrm`s (now including flat at the crossover depths); rewrite `env.json` (fresh run; same provenance fields); re-render all plots as **four-series** figures via `bench plot` (each still citing its CSV).
4. **Add a flat-specific memory note:** record `FlatBook`'s allocated span (ticks and bytes) per benchmark configuration into `env.json` or a `flat_memory.csv`, so the memory-vs-speed tradeoff is sourced, not asserted.

---

## 6. Hypotheses for this phase

Carry H1–H4 (now settled by Phase 4) into the verdict, and test the new one:
- **H5 — `FlatBook`'s O(1) update is depth- and locality-independent.** Expected: a flat apply-latency curve across depth in **both** localities (no crossover with it), and a real-corpus throughput that **leads** the field — resolving the rev-collapse inversion — at the cost of (a) span-proportional memory and (b) a measurable best-removal probe and sparse `top_n` scan. To be **confirmed or refuted with sourced numbers** in §7. If `FlatBook` does *not* lead real throughput (e.g., recenter/probe/sparse-scan costs dominate on the real book), that is the honest finding and the verdict says so.

---

## 7. Final crossover verdict — supersede `bench/results/RESULTS.md`

Rewrite `RESULTS.md` from interim (3 impls) to the **complete four-impl verdict**, built **only** from the committed CSVs. It is the analytical close-out; the publishable `docs/BENCHMARKS.md` / README / x-thread remain Phase 10.

Required content:
1. **Environment & methodology** (from `env.json`): the CO definition, the measured clock floor, threats to validity (timer floor, governor — Phase 4 ran `powersave`; note it and whether Phase 5 re-ran under `performance`, single host, elision-checked).
2. **The four-way crossover** (`service_sweep.csv`): the locality-gated picture with `FlatBook`'s curve overlaid; `D*` per locality; the depth/locality region each impl owns.
3. **The real-data inversion, explained and resolved** — the headline. Use the committed numbers to (a) restate the inversion (btree ~23.1 vs rev ~4.5 Mev/s, `throughput.csv`), (b) **triangulate** the real touch distribution by placing rev's real throughput (~4.5 Mev/s ≈ ~222 ns/event) against its synthetic service costs (Concentrated ~tens of ns vs Uniform-deep ~686 ns) to argue real touches are moderately deep/spread, and (c) report `FlatBook`'s measured real throughput as the test of H5 — does O(1) update lead the real book? State the result, not a hope.
4. **The flat-array tradeoff** (`flat_memory.csv` + `read_path.csv`): O(1) update and O(1) best vs span-proportional memory and the sparse-`top_n`/best-probe cost — quantified.
5. **The "which structure when" matrix** — a sourced recommendation: shallow/top-concentrated → Vec (rev/sorted at the floor); deep/unbounded range → BTree; deep/bounded range (the real-feed case) → FlatBook (if H5 confirmed); with the numbers behind each cell.
6. **H1–H5 outcomes**, each confirmed/refuted/inconclusive with a sourced number.

**Writing Standard (reaffirmed):** declarative and sourced (every number cites its CSV inline, with units + conditions); no marketing words or emoji; honesty is the signal (a refuted H5 with a profile beats a flattering unsourced win); never blur service time and CO-correct response time; tables/plots for data, prose only for mechanism; state the exact `bench` command and `env.json` conditions per result.

---

## 8. Phase 5 Definition of Done

1. `book/src/flat.rs` implements `FlatBook: OrderBook` per §2 (O(1) direct-index update, cached best with removal probe, O(1) `depth`, recenter/grow, `MAX_SPAN` panic). `#![forbid(unsafe_code)]` holds in `book`.
2. Freeze respected (§3): only `flat.rs` (new), the two `lib.rs` lines, `oracle.rs` (extended), and `FROZEN.md` (updated) changed in `book/`; the six frozen-logic files are byte-identical to `book-v1-frozen` (diff empty, reported).
3. Four-way oracle (§4) green: all bounded tests compare four impls; the extreme test stays three-impl; `flatbook_domain` rebase + out-of-domain (`should_panic`) tests pass. Zero third-party deps in `book`.
4. Harness re-run (§5): `FlatBook` wired into every benchmark (no `dyn`); `service_sweep.csv`, `read_path.csv`, `sustained.csv`, `throughput.csv`, `.hgrm`s, `env.json`, and `flat_memory.csv` regenerated with four-impl data; four-series plots rendered, each citing its CSV.
5. `RESULTS.md` rewritten as the complete four-impl verdict per §7: locality-gated crossover with FlatBook, the real-data inversion explained and resolved with triangulated sourced numbers, the tradeoff quantified, the "which-structure-when" matrix, H1–H5 outcomes sourced. Obeys the Writing Standard.
6. Quarantine intact: `cargo tree -p bench` shows no `tokio`; `feed`/the frozen `book` logic unchanged.
7. `cargo build`/`clippy -D warnings`/`test` (the four-way oracle included) clean at every commit; meaningful conventional commits on `main`.

After Phase 5 the order-book shootout is complete: four implementations behind one frozen trait, proven identical, measured honestly, and judged with a sourced verdict. Next is Phase 6 (`sync`: the seqlock snapshot cell).

---

# Appendix A — `CLAUDE.md` update for Phase 5

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, the four-impl shootout, DoD culture
- docs/specs/phase0-spec.md    — workspace, tick types, guardrail
- docs/specs/phase1-spec.md    — event model, OrderBook trait, BTreeBook
- docs/specs/phase2-spec.md    — Vec impls, differential oracle, FREEZE (book-v1-frozen)
- docs/specs/phase3-spec.md    — feed: corpus, replay, synthetic, recorder
- docs/specs/phase4-spec.md    — bench harness, depth sweep, CO-correct study, crossover
- docs/specs/phase5-spec.md    — CURRENT: FlatBook, 4-way oracle, re-run, final verdict

## Hard rules
1. book is FROZEN. Phase 5 makes ONLY the carved-out additive edits: new
   book/src/flat.rs, +2 lines in lib.rs (mod flat; pub use flat::FlatBook;),
   extend book/tests/oracle.rs to 4-way + FlatBook domain test, update FROZEN.md.
   The six frozen-logic files stay byte-identical to book-v1-frozen (diff empty).
2. FlatBook is the flat price-tick array: O(1) direct-index update, cached best
   with removal probe, recenter on out-of-range, MAX_SPAN panic. BOUNDED ranges
   only — that tradeoff is documented, not hidden. #![forbid(unsafe_code)] holds.
3. Oracle: 4-way on the bounded band; the extreme-i64 test stays 3-impl (FlatBook
   out of domain by design); add flatbook_domain (rebase + should_panic over cap).
4. bench (not frozen): wire "flat" into every benchmark by monomorphization (no
   dyn). Re-run Phase 4 methodology unchanged; regenerate all CSVs/.hgrm/plots/
   env.json + flat_memory.csv with 4-impl data.
5. RESULTS.md becomes the FINAL 4-impl verdict, built ONLY from committed CSVs:
   the real-data inversion explained + resolved, the flat tradeoff quantified, a
   sourced which-structure-when matrix, H1-H5 outcomes. Not the Phase 10 writeup.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test),
commit, list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (3 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | FlatBook + 4-way oracle | `book/src/flat.rs`, lib +2, `oracle.rs` 4-way + domain tests, `FROZEN.md` | oracle 4-way + domain green; freeze diff empty |
| 2 | Harness re-run | "flat" wired into all benches; CSVs/.hgrm/plots/env.json/flat_memory regenerated | 4-impl artifacts committed; plots 4-series |
| 3 | Final verdict + DoD | `RESULTS.md` rewritten (§7); DoD §8 verified | verdict sourced + Writing-Standard-clean; DoD checked |

Session 1 is the delicate one — it touches frozen code under the §3 exception; the freeze-diff gate is its guardrail. Session 2 is a mechanical re-run. Session 3 is the analytical close-out.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase5-spec.md` §1–§4, §6. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement `book/src/flat.rs` (`FlatBook`) exactly per §2 — flat per-side `Vec<Qty>` indexed by `tick - base`, O(1) direct-index update, cached best with the removal probe, O(1) `depth` via occupied counters, lazy init + recenter/grow, `MAX_SPAN` panic, `Clear` retaining capacity. Add the two permitted lines to `book/src/lib.rs`. Extend `book/tests/oracle.rs` per §4 (FlatBook in every bounded test via a four-way `assert_agree`; keep the extreme-`i64` test three-impl; add `flatbook_domain` rebase + `should_panic`-over-cap tests). Update `book/FROZEN.md`. Keep `#![forbid(unsafe_code)]`; zero third-party deps in `book`. Then run and REPORT `git diff book-v1-frozen -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs book/src/sorted_vec.rs book/src/rev_vec.rs` (must be empty). Run the three gates. Commit `feat(book): FlatBook flat price-tick array + four-way oracle`. List changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase5-spec.md` §5, and Phase 4's methodology §3. Execute **Session 2 only**: wire `"flat" => run::<FlatBook>(…)` into every benchmark (`service`/`read`/`sustained`/`throughput`), `harness::for_impl`, and the CLI impl list — monomorphized, no `dyn`. Re-run all four benchmarks across all four impls under the unchanged Phase 4 methodology (`black_box`, recorded clock floor, pin, warmup, ≥1M samples/cell, CO-correct sustained loop); prefer the `performance` governor and record whichever was active. Regenerate `service_sweep.csv`, `read_path.csv`, `sustained.csv`, `throughput.csv`, the `.hgrm` exports, `env.json`, and a new `flat_memory.csv` (FlatBook's allocated span in ticks+bytes per config). Re-render all plots as four-series via `bench plot`. Run the three gates. Commit `feat(bench): re-run four-impl sweep incl. FlatBook`. List changes + FlatBook's real-corpus throughput vs the others, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase5-spec.md` §1.1, §6, §7. Execute **Session 3 only**: rewrite `bench/results/RESULTS.md` as the complete four-impl verdict per §7, built ONLY from the committed CSVs — the locality-gated crossover with FlatBook overlaid; the real-data inversion restated, triangulated (rev's ~4.5 Mev/s ≈ ~222 ns/event vs its synthetic Concentrated/Uniform service costs → characterize the real touch distribution), and resolved with FlatBook's measured real throughput (H5 confirmed or refuted — state the number); the flat tradeoff quantified from `flat_memory.csv`/`read_path.csv`; the sourced which-structure-when matrix; H1–H5 outcomes each with a sourced number. Obey the Writing Standard; re-read RESULTS.md against it and fix violations. Confirm the freeze diff is still empty and `feed/src` unchanged. Run the three gates. Then verify Phase 5 DoD §8 item by item and report each. Commit `docs(bench): final four-way crossover verdict`. STOP. The order-book shootout is complete.
```

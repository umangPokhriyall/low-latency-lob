# bench — Phase 4 Specification: The Coordinated-Omission-Correct Harness, the Depth Sweep, and the Crossover

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md`, `docs/specs/phase1-spec.md`, `docs/specs/phase2-spec.md`, `docs/specs/phase3-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 4 spec.** `book` is frozen (`book-v1-frozen`); `feed` is complete and provides deterministic integer-tick corpora.
**Scope:** the `bench` crate — the measurement harness and the first committed numbers: the service-time **depth sweep** that produces the BTreeMap-vs-Vec **crossover plot**, the read-path cost, the open-loop **coordinated-omission-correct** sustained-feed response-time study, end-to-end throughput, the plots, and an interim findings doc. Measured across the **three current impls** (`BTreeBook`, `SortedVecBook`, `RevVecBook`); the fourth (`FlatBook`) and the final verdict are Phase 5.
**Audience:** Claude Code. Authoritative. This phase commits numbers — every number obeys the §3 methodology and the §10 Writing Standard.

---

## 1. Phase 4 in one paragraph

Phase 2 produced four falsifiable hypotheses (H1–H4) about which order-book structure wins where; Phase 4 builds the instrument that confirms or kills them and commits the first real numbers. The instrument has two jobs that must not be conflated. First, **service time**: the raw cost of one `apply` as a function of book depth and touch locality — pure compute, no arrival process — which produces the crossover plot that is the artifact STATE.md calls the highest-signal of the five. Second, **response time under load**: replaying a corpus at a defined arrival rate through a single pinned consumer and measuring, coordinated-omission-correctly, how the tail degrades as the feed outpaces the book — the "can it keep up" story. Getting nanosecond-scale measurement right (timer overhead, dead-code elision, warmup, frequency scaling, pinning) is the actual difficulty and the actual signal; a wrong number stated confidently is worse than no number. After Phase 4 the harness exists and the three-way story is committed; Phase 5 adds `FlatBook` and writes the final verdict.

### 1.1 Frozen / reused
- **`book` is frozen and reused unmodified.** The harness drives impls by **monomorphization** (`fn run<B: OrderBook>`), never `dyn OrderBook` (a vtable in the measured loop is disqualifying — see phase1-spec §2.4). Impl selection is a `match impl_name { … => run::<ConcreteBook>(…) }` dispatch.
- **`feed` is reused at default features** (no async). `bench` depends on `feed` and links **no** `tokio`.
- **No `book`/`feed` source is touched.** All new code lives in `bench/`.

---

## 2. Workspace additions & dependencies

```
bench/src/main.rs            # EDIT  — subcommand dispatch (service | read | sustained | throughput | plot | all)
bench/src/clock.rs           # NEW   — low-overhead TSC-backed clock + measured overhead
bench/src/recorder.rs        # NEW   — HdrHistogram wrapper: record_ns, percentiles, export
bench/src/harness.rs         # NEW   — pin, warmup, black_box discipline, impl dispatch
bench/src/workload.rs        # NEW   — build-book-at-depth, touch-locality generators
bench/src/benches/service.rs # NEW   — Benchmark 1: service-time depth sweep (THE crossover)
bench/src/benches/read.rs    # NEW   — Benchmark 2: read-path cost vs depth
bench/src/benches/sustained.rs # NEW — Benchmark 3: CO-correct sustained-feed response time
bench/src/benches/throughput.rs # NEW— Benchmark 4: end-to-end replay throughput
bench/src/plot.rs            # NEW   — plotters rendering from the committed CSVs
bench/results/*.csv          # NEW   — the committed source-of-truth numbers
bench/results/*.hgrm         # NEW   — HdrHistogram percentile-distribution exports
bench/results/plots/*.svg    # NEW   — rendered figures (each cites its CSV)
bench/results/env.json       # NEW   — measurement environment manifest (provenance)
bench/results/RESULTS.md     # NEW   — interim findings (3 impls), numbers-only, sourced
```

### 2.1 `bench/Cargo.toml` — dependency allowlist (pin exact versions via `cargo add`)
```toml
[dependencies]
book = { path = "../book" }
feed = { path = "../feed" }          # default features -> no async
hdrhistogram = "*"                   # CO-correct high-dynamic-range latency recording
quanta       = "*"                   # TSC-backed low-overhead clock, SAFE API (no rdtsc unsafe)
core_affinity = "*"                  # thread pinning, SAFE API
plotters     = { version = "*", default-features = false, features = ["svg_backend", "line_series"] }
```
`bench` keeps `#![forbid(unsafe_code)]` — `quanta` and `core_affinity` wrap the unsafe internally, so the harness needs none. (Rejected: raw `core::arch::x86_64::_rdtsc` — would force `unsafe` into `bench` and is x86-only; `quanta` gives the same TSC resolution behind a safe, calibrated, cross-arch API. Rejected for plotting: Python/matplotlib — version drift breaks reproducibility; gnuplot — external toolchain. `plotters` keeps the whole pipeline inside `cargo`. CSVs remain the citeable source of truth regardless.)
CLI parsing may be hand-rolled or `clap`; keep it minimal.

### 2.2 Quarantine check (DoD gate)
`cargo tree -p bench` contains `book`, `feed`, `hdrhistogram`, `quanta`, `core_affinity`, `plotters` — and **no `tokio`/`tokio-tungstenite`/`serde_json`**. The async stack stays behind `feed`'s `recorder` feature, which `bench` does not enable.

---

## 3. Measurement methodology & hygiene (the rigor that makes the numbers real)

This section is binding. A number produced in violation of it does not get committed. These are the threats a senior reader checks first.

### 3.1 What "coordinated-omission-correct" means for an in-process apply loop
The naive trap: a loop that waits for each `apply` to finish before starting the next *omits* the latency that backed-up events should have suffered — it measures service time and mislabels it response time. The correction (Gil Tene): when events arrive on a **schedule**, the latency of event *i* is measured from its **scheduled arrival time**, not from when the consumer happened to reach it.
```
scheduled_i = base + i * (1e9 / rate_eps)          // synthetic fixed-rate schedule, ns
   (or)      = base + (event_i.ts - corpus_first_ts) / speed   // real-arrival replay
latency_i   = completion_i - scheduled_i           // CO-CORRECT: vs schedule, not vs spin-end
```
When the book falls behind, `completion_i > scheduled_i` by the accumulated lag, and `latency_i` captures it. This single subtraction is the difference between an honest tail and a fictional one. **Benchmark 3 records `latency = completion - scheduled`; never `completion - apply_start`.** (Service-time Benchmarks 1/2/4 have no arrival process and therefore no CO — they measure the operation itself, and must say so explicitly.)

### 3.2 Timer overhead — measure it, report it, never hide it
At these timescales the clock read is a non-trivial fraction of the signal. The harness MUST, at startup, measure the clock's own read-read overhead over ≥100k iterations and record `clock_overhead_ns` into every CSV row and into `env.json`. Per-op timing uses the cheapest path (`quanta` raw counter + a single `delta_ns`), and the reader is told the floor. No overhead is silently subtracted; it is reported so the marginal cost is interpretable against it.

### 3.3 Dead-code elision — defeat it explicitly
The optimizer will delete an `apply` whose result is unused, producing absurd "0 ns" results. Every measured op wraps its inputs and outputs in `std::hint::black_box`:
```
let t0 = clock.raw();
black_box(book.apply(black_box(ev)));
let t1 = clock.raw();
rec.record(clock.delta_ns(t0, t1));
```
Read benchmarks `black_box` the returned value. A suspiciously-low result (below the measured `clock_overhead_ns` floor, or implausibly flat across depth) is treated as an elision bug, investigated, and fixed before any number is committed.

### 3.4 Warmup, steady state, pinning, frequency
- **Pin** the measuring thread to one core (`core_affinity`) so migrations don't pollute samples; record the core id.
- **Warm up** untimed before recording: run the workload long enough to warm I-cache/D-cache/branch predictor and let the CPU reach a steady frequency. Discard warmup samples.
- **Frequency scaling** dominates ns measurements. The harness reads and records the CPU governor (`/sys/.../scaling_governor`) and notes turbo state into `env.json`. The reader is told the governor; `performance` governor is recommended and the run records which was active. Do not pretend frequency was fixed if it was not — record what it was.
- **Sample counts:** ≥1,000,000 measured ops per (impl, depth, locality) cell for the service sweep; enough for stable p99.9. Record the actual sample count per row.

### 3.5 Determinism & provenance
Every benchmark consumes a deterministic input (a committed `.mdf` corpus or a seeded synthetic stream), so a run is reproducible up to machine timing noise. `env.json` captures: CPU model, logical core count, governor, kernel release, rustc version, `target-cpu` (native, per `.cargo/config.toml`), the git commit, the pinned core, `clock_overhead_ns`, and the SHA-256 (or length+first/last bytes if hashing is undesirable) of each corpus used. Every plot cites its source CSV; every CSV row carries its conditions.

---

## 4. The harness core

### 4.1 `clock.rs`
```rust
//! Low-overhead monotonic clock for ns-scale measurement, plus its own overhead.
use quanta::Clock;

pub struct BenchClock { clock: Clock, overhead_ns: u64 }

impl BenchClock {
    pub fn new() -> Self { /* build quanta::Clock; measure overhead (below) */ todo!() }
    #[inline] pub fn raw(&self) -> u64 { self.clock.raw() }
    #[inline] pub fn delta_ns(&self, a: u64, b: u64) -> u64 { self.clock.delta(a, b).as_nanos() as u64 }
    #[must_use] pub fn overhead_ns(&self) -> u64 { self.overhead_ns }
    /// Median read-read delta over >=100k iters (black_box the reads).
    fn measure_overhead(clock: &Clock) -> u64 { todo!() }
}
```

### 4.2 `recorder.rs`
```rust
//! HdrHistogram wrapper. Records ns latencies; emits percentiles + a .hgrm export.
use hdrhistogram::Histogram;

pub struct Recorder { hist: Histogram<u64> }
impl Recorder {
    pub fn new() -> Self { /* Histogram::new_with_bounds(1, 60_000_000_000, 3).unwrap() */ todo!() }
    #[inline] pub fn record(&mut self, ns: u64) { let _ = self.hist.record(ns.max(1)); }
    #[must_use] pub fn p(&self, q: f64) -> u64 { self.hist.value_at_quantile(q) }
    #[must_use] pub fn mean(&self) -> f64 { self.hist.mean() }
    #[must_use] pub fn max(&self) -> u64 { self.hist.max() }
    #[must_use] pub fn count(&self) -> u64 { self.hist.len() }
    /// Export the full percentile distribution for the interior-latency (log-y) plot.
    pub fn export_hgrm(&self, path: &Path) -> std::io::Result<()> { todo!() }
}
```
Percentiles always reported: p50, p90, p99, p99.9, max, mean, count.

### 4.3 `harness.rs`
```rust
//! Pinning, warmup, and the monomorphized impl dispatch (no dyn).
pub fn pin_to_core(core: usize) -> bool { /* core_affinity::set_for_current */ todo!() }

/// Run an untimed warmup pass, then return so the caller records the timed pass.
pub fn warmup<B: book::OrderBook>(/* build + replay untimed */) { todo!() }

/// Impl registry. Phase 5 adds `"flat" => f::<book::FlatBook>()`.
pub fn for_impl<R>(name: &str, f: impl FnOnce(&'static str) -> R) -> R { /* match name */ todo!() }
```
The dispatch pattern each benchmark uses (monomorphization, inlined `apply`):
```rust
match impl_name {
    "btree"  => run_service::<BTreeBook>(cfg),
    "sorted" => run_service::<SortedVecBook>(cfg),
    "rev"    => run_service::<RevVecBook>(cfg),
    other    => bail(other),
}
```

### 4.4 `workload.rs`
```rust
//! Construct a book at a target depth and generate touches at a controlled locality.
use book::{BookEvent, OrderBook, Px, Qty, Side};
use feed::rng::SplitMix64;

#[derive(Clone, Copy)]
pub enum Locality { Concentrated, Uniform } // top-of-book-biased vs flat across depth

/// Build a book with exactly `depth` levels per side around `mid` (untimed).
pub fn build_at_depth<B: OrderBook>(mid: Px, depth: usize) -> B { todo!() }

/// A price of an EXISTING level at an offset-from-best drawn per `Locality`
/// (Concentrated: geometric, mostly offsets 0..3; Uniform: uniform 0..depth).
pub fn touch_price(rng: &mut SplitMix64, mid: Px, depth: usize, side: Side, loc: Locality) -> Px { todo!() }
```

---

## 5. Benchmark 1 — service-time depth sweep (THE crossover artifact)

**Question:** how does the cost of one `apply` scale with book depth, for each impl, under realistic vs adversarial touch locality? This isolates the search-strategy axis (linear scan vs binary search vs tree descent) — the crossover.

**Procedure** for each `impl × Locality{Concentrated, Uniform} × depth ∈ {1,2,4,8,16,32,64,128,256,512,1024,2048}`:
1. `build_at_depth::<B>(mid, depth)` (untimed).
2. Warmup: ≥100k untimed touch-updates.
3. Timed: ≥1,000,000 `apply` of an in-place **update** `Level` event at `touch_price(..)` (existing level, new qty) — each individually timed (§3.2/3.3) into a `Recorder`. In-place update isolates *locate cost* (the crossover variable) from memmove.
4. Also record, as separate ops at the same depth, the **insert** (Level at a new in-band price → memmove) and **remove** (qty=0 at an existing price → memmove) costs, and a **trade_baseline** (a `Trade` apply — minimal work; the harness/dispatch floor).

**`bench/results/service_sweep.csv`**
```
impl,locality,depth,op,samples,clock_overhead_ns,mean_ns,p50_ns,p90_ns,p99_ns,p999_ns,max_ns
```
`op ∈ {update, insert, remove, trade_baseline}`. The **headline crossover plot** (§9) is `update` p50 and p99 vs depth, one series per impl, one figure per locality. Export `.hgrm` interior distributions at the crossover depth for each impl.

---

## 6. Benchmark 2 — read-path cost vs depth

**Question:** what does each impl charge on the read path — the path Phase 8's seqlock snapshot will hammer? Tests H3's "BTree pays a standing O(log n) best-access tax."

For each `impl × depth (same ladder)`: time ≥1,000,000 calls each of `best_bid` (expect O(1) Vec vs O(log n) BTree), `top_n(8)`, and `top_n(depth)` (full ladder copy). `black_box` the returned values.

**`bench/results/read_path.csv`**
```
impl,depth,op,samples,clock_overhead_ns,mean_ns,p50_ns,p99_ns,p999_ns,max_ns
```
`op ∈ {best_bid, top_n_8, top_n_full}`. Plot `best_bid` p50 vs depth (the read tax).

---

## 7. Benchmark 3 — sustained-feed, coordinated-omission-correct response time (the open-loop study)

**Question:** how fast a feed can each impl sustain, and how does the response-time tail degrade past saturation? This is where CO-correctness (§3.1) is demonstrated.

**Two schedules, both required:**
- **Real-arrival replay** of `feed/corpus/btcusdt-sample.mdf` at `speed = 1` (real ts) — demonstrates the book keeps up with a real feed comfortably (expected: apply is ns, arrivals are ms-scale).
- **Synthetic fixed-rate sweep** over each profile corpus (`steady/burst/flashcrash-s1-100k.mdf`) at `rate_eps ∈ {1e5, 1e6, 2e6, 5e6, 1e7, 2e7, 5e7, …}` increasing until saturation — finds the saturation rate and the tail-under-load.

**The CO-correct loop (exact):**
```rust
let base = clock.raw_as_ns();
for (i, ev) in corpus.events().iter().enumerate() {
    let scheduled = base + schedule_offset_ns(i, ev, rate_or_speed);
    let mut now = clock.now_ns();
    while now < scheduled { core::hint::spin_loop(); now = clock.now_ns(); } // busy-pace; no sleep
    black_box(book.apply(black_box(ev)));
    let done = clock.now_ns();
    rec.record(done.saturating_sub(scheduled));   // CO-CORRECT
}
```
**Saturation rule:** a rate is *saturated* when achieved throughput plateaus below target (the loop can no longer reach scheduled times) or p99 response climbs without bound as rate rises. Report each impl's **max sustainable rate** (highest rate with bounded p99).

**`bench/results/sustained.csv`**
```
impl,corpus,schedule,target_rate_eps,achieved_rate_eps,samples,clock_overhead_ns,resp_p50_ns,resp_p99_ns,resp_p999_ns,resp_max_ns,saturated
```
Plot: `resp_p99_ns` vs `target_rate_eps`, one series per impl (per profile) — the tail blow-up at saturation.

**Required harness unit test (the CO proof):** drive the CO loop with a deliberately slow synthetic op (`op_time > inter_arrival`); assert recorded latency for the *i*-th op grows ~`i*(op_time - interval)` (accumulating lag), proving the schedule-based correction records lag rather than hiding it. A loop that subtracts `apply_start` instead of `scheduled` fails this test.

---

## 8. Benchmark 4 — end-to-end replay throughput

**Question:** the simple headline — replay a whole corpus as fast as possible (no pacing), per impl, per corpus.

For each `impl × corpus`: warmup, then time the full `for ev in corpus.events() { black_box(book.apply(black_box(ev))); }` once over the resident slice; repeat ≥31 times; report median total and derived rate.

**`bench/results/throughput.csv`**
```
impl,corpus,events,runs,total_ns_median,events_per_sec,ns_per_event
```

---

## 9. Plotting & results artifacts (`plot.rs`)

`bench plot` reads the committed CSVs (the source of truth) and renders `bench/results/plots/*.svg` via `plotters`. No plot invents data; each is a view of a CSV, and its filename/caption names that CSV.
- `crossover_update_p50_concentrated.svg`, `..._p99_concentrated.svg`, and the `_uniform` pair — apply-update latency vs depth, one line per impl (log-x depth; the headline).
- `read_best_bid_vs_depth.svg` — the read tax.
- `sustained_p99_vs_rate_<profile>.svg` — the CO-correct tail blow-up.
- Interior-latency-distribution plots (log-y) from the `.hgrm` exports at the crossover depth.

`bench all` runs every benchmark then `plot`, writing `env.json` first. Commit CSVs, `.hgrm`s, SVGs, and `env.json`.

---

## 10. `bench/results/RESULTS.md` — interim findings (numbers only, fully sourced)

A committed findings document built **only** from the CSVs. It is **interim**: three impls, no `FlatBook` (Phase 5 adds it and writes the final verdict); it is **not** the publishable `docs/BENCHMARKS.md` (Phase 10). Structure: environment summary (from `env.json`); the methodology in three sentences incl. the CO definition and the `clock_overhead_ns` floor; a threats-to-validity paragraph (timer floor, governor, single-host, elision-checked); the crossover table + figures; the read-tax finding; the sustained max-rate table + tail figure; a surprises paragraph; and a data-provenance table mapping each claim/figure to its CSV.

**Writing Standard (governs RESULTS.md):**
1. Declarative, sourced: every number cites its CSV inline, e.g. "update p99 at depth=64, Concentrated: RevVec X ns vs SortedVec Y ns (`service_sweep.csv`)."
2. Units + conditions always: value + ns + depth + locality + (rate/corpus where relevant).
3. No marketing words, no emoji, no exclamation. The telemetry carries the weight.
4. Honesty is the signal: state where a hypothesis was wrong, what sits below the timer floor and is therefore not resolvable, what is unexplained.
5. Apples-to-apples: name the axis (service time vs CO-correct response time; never blur them).
6. Tables/plots for data; prose only for mechanism.
7. Reproducibility: state the exact `bench` command and the `env.json` conditions for every result.

---

## 11. Hypotheses under test (from phase2-spec §7 — now falsified or confirmed)

- **H1** (RevVec dominates at shallow/top-concentrated): tested by Benchmark 1, `Concentrated`, low–mid depth.
- **H2** (SortedVec overtakes RevVec only at deep/uniform touches at large n): tested by Benchmark 1, `Uniform`, high depth — locate the crossover depth `D*`.
- **H3** (BTree loses across realistic depths; pays an O(log n) best-access tax): tested by Benchmarks 1 and 2.
- **H4** (the RevVec↔SortedVec crossover sits at some `D*`/locality): the headline output — report `D*` per locality with the interior distributions on each side of it.
RESULTS.md states, per hypothesis, confirmed/refuted/inconclusive **with the sourced number**. A refuted hypothesis with a profile is a better result than a confirmed one without numbers.

---

## 12. Phase 4 Definition of Done

1. Harness core (`clock`, `recorder`, `harness`, `workload`) implemented; `#![forbid(unsafe_code)]` holds; clock overhead measured and recorded.
2. Benchmarks 1–4 implemented and run across the three impls; CSVs + `.hgrm`s committed to `bench/results/`.
3. Methodology honored: CO-correct loop records vs schedule (proven by the §7 unit test); `black_box` everywhere; warmup + pinning + governor recorded; ≥1M samples per service-sweep cell.
4. The crossover plot(s) rendered from `service_sweep.csv`; `D*` identified per locality.
5. Sustained study: real-arrival replay (keeps up) + synthetic rate sweep (saturation + tail) committed; max sustainable rate per impl reported.
6. `env.json` provenance written; every plot cites its CSV.
7. `RESULTS.md` complete, interim (3 impls), obeys the §10 Writing Standard, states H1–H4 outcomes with sourced numbers, explicitly defers `FlatBook` + final verdict to Phase 5.
8. Quarantine: `cargo tree -p bench` shows no `tokio`. `book`/`feed` unchanged (`git diff book-v1-frozen -- book/` empty; no `feed/src` change).
9. `cargo build`/`clippy -D warnings`/`test` clean at every commit; meaningful conventional commits on `main`.

After Phase 4 the harness exists and the three-way story is committed. Next is Phase 5 (`FlatBook` + the final four-way crossover verdict).

---

# Appendix A — `CLAUDE.md` update for Phase 4

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, the four-impl shootout, DoD culture
- docs/specs/phase0-spec.md    — workspace, tick types, guardrail
- docs/specs/phase1-spec.md    — event model, OrderBook trait, BTreeBook
- docs/specs/phase2-spec.md    — Vec impls, differential oracle, FREEZE (book-v1-frozen)
- docs/specs/phase3-spec.md    — feed: corpus, replay, synthetic, recorder
- docs/specs/phase4-spec.md    — CURRENT: bench harness, depth sweep, CO-correct study, crossover

## Hard rules
1. book + feed are FROZEN/done. All Phase 4 code lives in bench/. Drive impls by
   monomorphization (run::<ConcreteBook>) — NEVER dyn OrderBook in a measured loop.
2. MEASURE-NEVER-GUESS, operationalized:
   - black_box every measured op's inputs AND outputs (defeat elision).
   - Measure and RECORD clock overhead; never silently subtract it.
   - Pin the thread; warm up untimed; record the CPU governor. >=1M samples/cell.
   - Coordinated omission: response latency = completion - SCHEDULED arrival,
     never completion - apply_start. Service-time benches have no arrival process
     and must say so. Do not blur service time and response time.
3. bench deps: hdrhistogram, quanta, core_affinity, plotters. NO tokio (feed is
   used at default features). bench stays #![forbid(unsafe_code)].
4. Numbers are committed to bench/results/ as CSV (source of truth) + .hgrm + SVG +
   env.json. Plots cite their CSV. RESULTS.md is built ONLY from committed CSVs —
   invent nothing; this is interim (3 impls), not the Phase 10 BENCHMARKS.md.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test),
commit, list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (4 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | Harness core + depth sweep | `clock/recorder/harness/workload` + Benchmark 1 (§4–§5) | `service_sweep.csv` for 3 impls; harness unit tests green; elision-checked |
| 2 | Read path + throughput | Benchmarks 2 & 4 (§6, §8) | `read_path.csv`, `throughput.csv` committed |
| 3 | CO-correct sustained study | Benchmark 3 (§7) + the CO proof test | `sustained.csv` committed; CO unit test green |
| 4 | Plots + findings + DoD | `plot.rs`, SVGs, `env.json`, `RESULTS.md` (§9–§10) | plots cite CSVs; RESULTS.md obeys §10; DoD §12 verified |

Session 1 is the heavy, load-bearing one (the harness *is* the instrument). Session 3 is the subtle one — the CO loop and its proof test. Sessions 2 and 4 are lighter. Keep them separate for clean commits and a safety margin against the token window.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase4-spec.md` §1–§5, and the methodology §3 in full. Update `CLAUDE.md` per Appendix A. Add the §2.1 deps (pin exact versions via `cargo add`). Execute **Session 1 only**: implement `bench/src/{clock,recorder,harness,workload}.rs` per §4 and Benchmark 1 (`benches/service.rs`) per §5 — TSC clock with measured overhead, HdrHistogram recorder, thread pinning, untimed warmup, `black_box` on every measured op, monomorphized impl dispatch (no dyn). Run the service depth sweep across `btree`/`sorted`/`rev`, all depths, both localities, ≥1M samples/cell; write `bench/results/service_sweep.csv` and the crossover-depth `.hgrm` exports. Add harness unit tests (clock overhead is measurable and recorded; recorder percentiles correct on a known distribution; locality generator produces the intended offset distribution; an elision guard asserting measured update latency at depth 1 exceeds the clock floor). Keep `#![forbid(unsafe_code)]`; `cargo tree -p bench` shows no tokio. Run the three gates. Commit `feat(bench): measurement harness + service-time depth sweep`. List changes + the crossover headline (D* and the p99 numbers each side of it), STOP.

**Session 2**
> Read `CLAUDE.md` and `phase4-spec.md` §6, §8, §3. Execute **Session 2 only**: implement Benchmark 2 (`benches/read.rs`, §6) and Benchmark 4 (`benches/throughput.rs`, §8), reusing the harness core. `black_box` returned values; warmup; pin; record clock overhead. Run across the 3 impls; write `bench/results/read_path.csv` and `throughput.csv`. Run the three gates. Commit `feat(bench): read-path cost + end-to-end throughput`. List changes + the best_bid read-tax numbers and headline throughput, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase4-spec.md` §7, §3.1. Execute **Session 3 only**: implement Benchmark 3 (`benches/sustained.rs`) per §7 — the busy-paced CO-correct loop recording `completion - scheduled`, real-arrival replay of the BTCUSDT sample at speed=1, and the synthetic fixed-rate sweep over the three profiles to saturation. Add the §7 CO-proof unit test (slow op accumulates lag; a `completion - apply_start` loop must fail it). Write `bench/results/sustained.csv`; report each impl's max sustainable rate. Run the three gates. Commit `feat(bench): coordinated-omission-correct sustained-feed study`. List changes + max sustainable rates and the tail behaviour, STOP.

**Session 4**
> Read `CLAUDE.md` and `phase4-spec.md` §9–§12. Execute **Session 4 only**: implement `bench/src/plot.rs` (plotters; render the §9 SVGs strictly from the committed CSVs, each figure naming its CSV), wire `bench plot` and `bench all`, write `bench/results/env.json` (§3.5 provenance), and author `bench/results/RESULTS.md` per §10 — interim, 3 impls, every number sourced to a CSV, H1–H4 outcomes stated with numbers, `FlatBook`/final verdict explicitly deferred to Phase 5. Obey the §10 Writing Standard; re-read RESULTS.md against its 7 rules and fix violations. Confirm `git diff book-v1-frozen -- book/` is empty and `feed/src` is unchanged. Run the three gates. Then verify Phase 4 DoD §12 item by item and report each. Commit `docs(bench): crossover plots, env manifest, and interim results`. STOP.
```

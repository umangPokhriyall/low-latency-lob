# Phase 4 — interim results (three impls)

**Interim and incomplete by design.** This document reports the three-impl shootout
(`BTreeBook`, `SortedVecBook`, `RevVecBook`). `FlatBook` and the final four-way verdict
are **deferred to Phase 5**. This is not the publishable `docs/BENCHMARKS.md` (Phase 10).
Every number below is sourced to a committed CSV under `bench/results/`; nothing here is
computed by hand from anything else.

## Environment (`env.json`)

| field | value |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz |
| logical cores | 8 |
| CPU governor | `powersave` (turbo not pinned) |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0 |
| `target-cpu` | native |
| pinned core | 0 |
| clock read-read floor | 7 ns (`env.json`); 7 ns service/sustained, 9 ns read, per the CSV `clock_overhead_ns` column |
| git commit | recorded in `env.json` |

Corpora fingerprints (length + first/last 16 bytes) are in `env.json`. All four corpora are
deterministic (`feed` synthetic seed=1, or the one recorded BTCUSDT session), so a run is
reproducible up to machine timing noise.

## Methodology (three sentences)

The service-time benchmarks (1 `service`, 2 `read`, 4 `throughput`) time the operation itself
— individually-timed `apply`/read ops or whole-corpus replays — with **no arrival process and
therefore no coordinated omission**; they measure cost, not response time. The sustained study
(3 `sustained`) is open-loop and **coordinated-omission-correct**: events arrive on a schedule
and response latency is `completion − scheduled` (the scheduled arrival), never
`completion − apply_start`, so a book that falls behind accrues the backlog in its tail. Every
measured op wraps inputs and outputs in `black_box`, ≥1,000,000 samples are taken per
service-sweep cell, and the clock's own read-read floor (~7–9 ns) is reported and **never
subtracted** — values at or below it are not resolvable.

## Threats to validity

- **Timer floor (~7–9 ns).** `best_bid` and shallow-depth `update` results sit at or just
  above the floor; differences there are not resolvable and are reported as such, not as wins.
- **Governor.** The run used `powersave`, not the recommended `performance`, and turbo was not
  pinned. Frequency scaling adds noise; it surfaces as occasional single-run p99 outliers in
  the sustained sweep (e.g. `sorted`/`steady` profile `burst`, 2e6 eps: resp_p99 = 1,940,479 ns
  while every neighbouring rate is ~100 ns — `sustained.csv`). These are transient, not
  structural.
- **Single host.** All numbers are from the one i5-1135G7 above, built `target-cpu=native`;
  they are host-specific by design and do not transfer.
- **Elision.** Guarded in tests: `update` cost scales with depth for `RevVec`, `top_n_full`
  scales with depth, and the measured floor is checked against the clock floor; a flat or
  zero result would have failed those guards.

---

## The crossover (Benchmark 1, `service_sweep.csv`)

`update` is an in-place qty replace at an existing level: it isolates *locate* cost (linear
scan vs binary search vs tree descent) from memmove. Figures:
`plots/crossover_update_p50_concentrated.svg`, `..._p99_concentrated.svg`, and the `_uniform`
pair (apply-update latency vs depth, log-x, one line per impl).

### Concentrated (top-of-book-biased — the realistic case), `update` p50 / p99 (ns)

| depth | btree p50 | sorted p50 | rev p50 | btree p99 | sorted p99 | rev p99 |
|---|---|---|---|---|---|---|
| 8 | 16 | 8 | 8 | 21 | 9 | 9 |
| 64 | 17 | 9 | 8 | 23 | 10 | 9 |
| 128 | 19 | 9 | 8 | 24 | 10 | 9 |
| 256 | 27 | 14 | 18 | 36 | 15 | 26 |
| 2048 | 31 | 16 | 18 | 41 | 17 | 26 |

### Uniform (flat across depth — the adversarial deep-search case), `update` p50 / p99 (ns)

| depth | btree p50 | sorted p50 | rev p50 | btree p99 | sorted p99 | rev p99 |
|---|---|---|---|---|---|---|
| 2 | 16 | 11 | 16 | 19 | 12 | 20 |
| 64 | 29 | 9 | 37 | 33 | 9 | 54 |
| 256 | 36 | 9 | 99 | 42 | 10 | 166 |
| 1024 | 48 | 12 | 349 | 61 | 13 | 619 |
| 2048 | 50 | 14 | 686 | 59 | 15 | 1224 |

### `D*` (the RevVec↔SortedVec crossover depth)

- **Concentrated: `D*` = 256.** Below it RevVec ≤ SortedVec at the timer floor (depth 128:
  rev p50 8 ns vs sorted 9 ns); from 256 up RevVec is persistently worse (depth 256: rev p50
  18 ns vs sorted 14 ns; p99 26 ns vs 15 ns). The geometric touch distribution keeps most
  hits near the top until the working set outgrows L1 around 256 levels.
- **Uniform: `D*` = 2.** SortedVec overtakes RevVec immediately (depth 2: rev p50 16 ns vs
  sorted 11 ns) and the gap widens without bound (depth 2048: rev p50 686 ns vs sorted 14 ns).
  Binary search costs `O(log n)`; the reverse-array linear scan costs `O(n)` when touches are
  spread across the whole ladder.

Interior distributions either side of these depths are exported as
`service_update_{impl}_{loc}_d{D*}.hgrm` and plotted in
`plots/interior_update_concentrated_d256.svg` and `plots/interior_update_uniform_d2.svg`.

---

## The read tax (Benchmark 2, `read_path.csv`)

Figure: `plots/read_best_bid_vs_depth.svg`.

`best_bid` p50 is **flat at the timer floor** for all three impls at every depth — btree 12 ns
(depth 1) → 10 ns (depth 2048); sorted 11 → 10; rev 11 → 11 — against a 9 ns clock floor. The
hypothesised `O(log n)` BTree best-access tax is therefore **below the timer floor and not
resolvable** at this resolution (see H3).

Where the tree's structure does cost is the full-ladder copy. `top_n_full` p50 at depth 2048:
btree 3517 ns vs sorted 636 ns vs rev 604 ns (`read_path.csv`) — BTree's node-by-node
traversal is ~5.5× the contiguous-array copy of either Vec.

---

## End-to-end throughput (Benchmark 4, `throughput.csv`)

Whole-corpus replay, no pacing, median of 31 runs. **Service time, not response time.**

| corpus | btree (Mev/s) | sorted (Mev/s) | rev (Mev/s) |
|---|---|---|---|
| steady (100k) | 45.1 | 76.7 | 66.5 |
| burst (100k) | 46.6 | 75.2 | 66.3 |
| flashcrash (100k) | 52.1 | 76.2 | 74.1 |
| btcusdt-sample (13,765) | 23.1 | 15.9 | 4.5 |

On the synthetic profiles SortedVec leads (76.7 Mev/s steady), RevVec follows, BTree trails.
On the **real** BTCUSDT corpus the ranking inverts: BTree is fastest (23.1 Mev/s) and RevVec
falls to 4.5 Mev/s (222 ns/event) — the real feed's deeper, wider price movement turns
RevVec's in-band inserts/removes into long memmoves (see Surprises).

---

## Sustained, coordinated-omission-correct response time (Benchmark 3, `sustained.csv`)

Figures: `plots/sustained_p99_vs_rate_{steady,burst,flashcrash}.svg`. **Response time under a
schedule — not service time.**

### Real-arrival replay of the BTCUSDT sample at speed = 1

| impl | achieved (eps) | resp p50 (ns) | resp p99 (ns) | resp max (ns) | saturated |
|---|---|---|---|---|---|
| btree | 305 | 5067 | 89,919 | 99,071 | no |
| sorted | 305 | 5335 | 167,807 | 174,207 | no |
| rev | 305 | 10,383 | 603,135 | 687,103 | no |

All three keep up with the real feed (achieved 305 eps vs ~304 natural): arrivals are
ms-scale, applies are ns-scale, so the book idles ~99.99% of the time. RevVec carries the
widest response tail (603 µs p99) — consistent with its memmove cost on the deeper real book.

### Synthetic fixed-rate sweep — max sustainable rate (bounded-p99, achieved ≥ 90% of target)

| impl | steady | burst | flashcrash |
|---|---|---|---|
| btree | 20 Mev/s | 20 Mev/s | 20 Mev/s |
| sorted | 20 Mev/s | 20 Mev/s | 20 Mev/s |
| rev | 20 Mev/s | 20 Mev/s | 20 Mev/s |

All three sustain 20 Mev/s and saturate at the next rung, 50 Mev/s; the true knee is bracketed
between them (the ladder does not probe inside). The tail blow-up at saturation (steady, resp
p99):

| impl | 20 Mev/s (sustained) | 50 Mev/s (saturated) |
|---|---|---|
| btree | 108 ns | 1,714,175 ns |
| sorted | 81 ns | 1,231,871 ns |
| rev | 92 ns | 1,129,471 ns |

Past saturation the open-loop response tail jumps ~4 orders of magnitude — the accumulated
backlog the CO-correct subtraction is built to capture. A `completion − apply_start` loop would
have reported ~tens of ns here and hidden it (proven by the `co_correct_records_accumulating_lag`
harness test).

---

## Hypotheses (phase2-spec §7) — outcomes

- **H1 — RevVec dominates at shallow / top-concentrated: partially confirmed (refined).** At
  Concentrated low–mid depth RevVec sits at the timer floor (p50 8 ns, depths 8–128,
  `service_sweep.csv`) and beats BTree (16–19 ns), but it is **tied with** SortedVec (8–9 ns),
  not dominant over it. Confirmed vs BTree; inconclusive vs SortedVec (both at the floor).
- **H2 — SortedVec overtakes RevVec only at deep/uniform, large n: refuted as stated.** The
  *direction* holds (SortedVec wins under Uniform touches), but the *threshold* does not:
  SortedVec overtakes RevVec at `D*` = 2 under Uniform, not at large n (depth 2: rev p50 16 ns
  vs sorted 11 ns; depth 2048: 686 ns vs 14 ns, `service_sweep.csv`). The crossover is
  locality-dependent, not depth-gated.
- **H3 — BTree loses across realistic depths and pays an O(log n) best-access tax: split.**
  BTree loses on `update` at essentially every depth (Concentrated p50 11→31 ns, Uniform
  11→50 ns vs the Vecs, `service_sweep.csv`) and on synthetic throughput (45.1 vs 76.7 Mev/s
  steady, `throughput.csv`) — confirmed. The specific best-access tax is **inconclusive**:
  `best_bid` is flat ~10 ns ≈ the 9 ns clock floor for all impls (`read_path.csv`), so any
  `O(log n)` descent sits below the timer floor and cannot be measured here (the tree's cost
  does surface elsewhere — `top_n_full` 3517 ns vs ~620 ns at depth 2048).
- **H4 — the RevVec↔SortedVec crossover sits at some `D*`/locality: confirmed.** `D*` = 256
  (Concentrated), `D*` = 2 (Uniform) (`service_sweep.csv`), with interior distributions
  exported either side (`service_update_*_{concentrated_d256,uniform_d2}.hgrm`).

## Surprises

- **The best-access tax is below the timer floor.** H3's headline mechanism is real in theory
  but unmeasurable at ns resolution: `best_bid` reads ~10 ns for tree and array alike
  (`read_path.csv`). The honest result is "not resolved," not "refuted."
- **Real-corpus throughput inverts the synthetic ranking.** BTree is fastest on the recorded
  BTCUSDT feed (23.1 Mev/s) and RevVec is slowest by ~5× (4.5 Mev/s), the reverse of the
  synthetic profiles (`throughput.csv`). The real feed's depth and price dispersion penalise
  contiguous-array memmoves that the synthetic generator's tighter band did not exercise.
- **All three share one saturation point.** Despite 8 ns–686 ns service-time spreads, every
  impl maxes at 20 Mev/s and saturates at 50 Mev/s on every profile (`sustained.csv`): below
  saturation the schedule, not the per-op cost, governs the response tail, so the open-loop
  story is "when does saturation hit," not "which impl is faster per event."

## Data provenance

| claim / figure | source |
|---|---|
| crossover tables, `D*`, H2/H4 | `service_sweep.csv` |
| `crossover_update_{p50,p99}_{concentrated,uniform}.svg` | `service_sweep.csv` |
| `interior_update_{concentrated_d256,uniform_d2}.svg` | `service_update_*_{loc}_d{D*}.hgrm` |
| read tax, `top_n_full`, H3 best-access | `read_path.csv` |
| `read_best_bid_vs_depth.svg` | `read_path.csv` |
| throughput table, real-corpus inversion | `throughput.csv` |
| sustained tables, max rate, tail blow-up | `sustained.csv` |
| `sustained_p99_vs_rate_{profile}.svg` | `sustained.csv` |
| environment, corpora fingerprints | `env.json` |

## Reproducibility

```
# build host-specific (target-cpu=native via .cargo/config.toml)
cargo build --release -p bench

# per-benchmark (CSV is the source of truth):
./target/release/bench service    --core 0   # -> service_sweep.csv + .hgrm
./target/release/bench read       --core 0   # -> read_path.csv
./target/release/bench throughput --core 0   # -> throughput.csv
./target/release/bench sustained  --core 0   # -> sustained.csv  (~150 s; real replay is ~45 s/impl)
./target/release/bench plot                  # -> env.json + plots/*.svg from the committed CSVs

# or everything then plots:
./target/release/bench all --core 0
```

Conditions for every result above are the `env.json` manifest: i5-1135G7, `powersave`
governor, pinned core 0, `target-cpu=native`, clock floor ~7–9 ns. Phase 5 adds `FlatBook`
and writes the final four-way crossover verdict.

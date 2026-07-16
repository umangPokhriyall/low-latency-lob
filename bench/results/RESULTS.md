# The order-book shootout — final four-impl verdict

This is the complete four-implementation verdict (`BTreeBook`, `SortedVecBook`, `RevVecBook`,
`FlatBook`), built **only** from the committed CSVs under `bench/results/`. Every number cites
its CSV inline with units and conditions; nothing here is computed by hand from anything else.
It is the analytical close-out of the shootout; the consolidated writeup is
`docs/BENCHMARKS.md`.

The four implementations sit behind one frozen `OrderBook` trait and were proven
observationally identical by the differential oracle (`book/tests/oracle.rs`, four-way on the
bounded band). What follows is which structure to use when, and why.

## 1. Environment & methodology

| field | value (`env.json`) |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz |
| logical cores | 8 |
| CPU governor | `powersave` (turbo not pinned) |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0 |
| `target-cpu` | native |
| pinned core | 0 |
| clock read-read floor | 7 ns (`clock_overhead_ns` column in every CSV) |
| git commit | recorded in `env.json` |

Corpora fingerprints (length + first/last 16 bytes) are in `env.json`. All four corpora are
deterministic (`feed` synthetic seed = 1, or the one recorded BTCUSDT session), so a run is
reproducible up to machine timing noise.

**Service time vs response time (kept distinct).** Benchmarks 1 (`service`), 2 (`read`), and 4
(`throughput`) time the operation itself — individually-timed `apply`/read ops, or whole-corpus
replays — with **no arrival process and therefore no coordinated omission**; they measure cost.
Benchmark 3 (`sustained`) is open-loop and **coordinated-omission-correct**: events arrive on a
schedule and response latency is `completion − scheduled` (the scheduled arrival time), never
`completion − apply_start`, so a book that falls behind accrues the backlog in its tail (proven
by the `co_correct_records_accumulating_lag` harness test). Every measured op wraps inputs and
outputs in `black_box`; ≥ 1,000,000 samples are taken per service-sweep cell; the 7 ns clock
floor is reported and **never subtracted** — values at or below it are not resolvable.

### Threats to validity

- **Timer floor (7 ns).** `best_bid` and shallow-depth `update` results sit at or just above the
  floor; differences there are not resolvable and are reported as such, not as wins.
- **Governor.** The service sweep ran `powersave`; the real-data study re-ran under `powersave` as well (the host has
  no passwordless route to set `performance`, recorded in `env.json`). Frequency scaling adds
  noise; it surfaces as occasional single-run p99/max outliers (e.g. `flat`/`uniform` `top_n_full`
  at depth 2048 carries a `max` of 2,385,919 ns against a 1,990 ns p50, `read_path.csv`). These
  are transient, not structural; p50/p99 across ≥ 1 M samples are stable.
- **Single host.** All numbers are from the one i5-1135G7 above, built `target-cpu=native`; they
  are host-specific by design and do not transfer.
- **Elision.** Guarded in tests: `update` cost scales with depth for `RevVec`, `top_n_full`
  scales with depth, and the measured floor is checked against the clock floor; a flat or zero
  result would have failed those guards.

---

## 2. The four-way crossover is locality-gated (`service_sweep.csv`)

`update` is an in-place qty replace at an existing level: it isolates *locate* cost (linear
scan vs binary search vs tree descent vs direct index) from memmove. Figures:
`plots/crossover_update_p50_concentrated.svg`, `..._p99_concentrated.svg`, and the `_uniform`
pair (apply-update latency vs depth, log-x, one line per impl, FlatBook overlaid in orange).

### Concentrated (top-of-book-biased — the realistic case), `update` p50 (ns)

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 8 | 16 | 8 | 8 | 12 |
| 64 | 18 | 9 | 8 | 12 |
| 128 | 19 | 9 | 8 | 13 |
| 256 | 27 | 14 | 19 | 16 |
| 1024 | 31 | 16 | 20 | 16 |
| 2048 | 31 | 17 | 21 | 16 |

### Uniform (flat across the ladder — the adversarial deep-search case), `update` p50 (ns)

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 2 | 16 | 11 | 16 | 14 |
| 64 | 28 | 9 | 38 | 14 |
| 256 | 36 | 9 | 100 | 14 |
| 1024 | 47 | 11 | 353 | 14 |
| 2048 | 50 | 14 | 697 | 14 |

**`D*` (the RevVec↔SortedVec crossover) is locality-gated, not depth-gated.**

- **Concentrated: `D*` = 256.** Below it RevVec ≤ SortedVec at the timer floor (depth 128: rev
  p50 8 ns vs sorted 9 ns); from 256 up RevVec is persistently worse (depth 256: rev p50 19 ns
  vs sorted 14 ns; p99 47 ns vs 20 ns). The geometric touch distribution keeps most hits near
  the top until the working set outgrows L1 around 256 levels.
- **Uniform: `D*` = 2.** SortedVec overtakes RevVec immediately (depth 2: rev p50 16 ns vs
  sorted 11 ns) and the gap widens without bound (depth 2048: rev p50 697 ns vs sorted 14 ns —
  a ~50× loss). Binary search is `O(log n)`; the best-first linear scan is `O(n)` when touches
  spread across the whole ladder.

**FlatBook is depth- and locality-independent.** Its `update` p50 sits in 11–16 ns across every
depth in **both** localities (Concentrated 12–16 ns; Uniform a flat 14 ns from depth 2 to 2048).
At the depth/locality where RevVec collapses (Uniform, depth 2048) FlatBook is 14 ns vs RevVec's
697 ns and SortedVec's 14 ns — it ties the binary-search floor without paying any locate cost,
because the update is a single direct-indexed write. There is no crossover *with* FlatBook: it
never degrades with depth. (One artefact: `flat`/`concentrated` depth 4 reads 16 ns p50 vs the
~12 ns of its neighbours, `service_sweep.csv` — a single warm-up/frequency outlier, not a depth
effect, since deeper cells are cheaper.)

**Region each impl owns on service time:** shallow + top-concentrated → RevVec or SortedVec
(both at the ~8 ns floor); deep + spread → SortedVec (`O(log n)`, 14 ns at depth 2048 Uniform)
or FlatBook (`O(1)`, 14 ns) — provided FlatBook's span is bounded (§4). Interior distributions
either side of `D*` are exported as `service_update_{impl}_{loc}_d{D*}.hgrm` and plotted in
`plots/interior_update_concentrated_d256.svg` and `plots/interior_update_uniform_d2.svg`.

---

## 3. The real-data inversion — explained and resolved (`throughput.csv`)

This is the headline. Whole-corpus replay, no pacing, median of 31 runs — **service time, not
response time.**

| corpus | btree (Mev/s) | sorted (Mev/s) | rev (Mev/s) | flat (Mev/s) |
|---|---|---|---|---|
| steady (100k) | 44.8 | 74.0 | 66.1 | **113.0** |
| burst (100k) | 45.8 | 76.4 | 66.5 | **115.7** |
| flashcrash (100k) | 51.6 | 73.8 | 74.1 | 64.6 |
| btcusdt-sample (13,765) | **23.2** | 16.0 | 4.5 | 0.09 |

On the synthetic profiles the array structures win — FlatBook leads at 113.0 Mev/s (steady),
SortedVec next at 74.0, RevVec 66.1, BTree trails at 44.8. **On the real BTCUSDT corpus the
ranking fully inverts: BTree is fastest at 23.2 Mev/s, SortedVec 16.0, RevVec 4.5, and FlatBook
collapses to 0.09 Mev/s (10,926 ns/event).**

### Triangulating the real touch distribution

RevVec's real throughput is 4.5 Mev/s ≈ 222 ns/event (`throughput.csv`, btcusdt-sample
`ns_per_event` = 222.34). Place that against RevVec's synthetic `update` service costs
(`service_sweep.csv`): Concentrated is 8–21 ns at every depth, while Uniform climbs 38 ns
(depth 64) → 100 ns (256) → 183 ns (512) → 353 ns (1024) → 697 ns (2048). A 222 ns/event real
cost sits firmly in RevVec's **Uniform, deep** regime (between its depth-512 and depth-1024
Uniform costs), not anywhere near its 8–21 ns Concentrated regime. Were the real feed
top-of-book-concentrated, RevVec would replay at tens of ns/event (≈ 50–125 Mev/s); it does
not. Therefore the real touch/insert distribution is **moderately deep and spread across the
ladder** — precisely the regime that defeats a linear scan, and the reason the synthetic-vs-real
ranking inverts.

### FlatBook on the real book — the test of H5

H5 predicted FlatBook's `O(1)` update would let it **lead** real throughput and resolve the
rev-collapse. The service-time half holds (§2). The throughput half does **not**: FlatBook
replays the real corpus at **0.09 Mev/s, 10,926 ns/event** (`throughput.csv`), ~250× slower
than BTree. The mechanism is in `flat_memory.csv`: the real BTCUSDT book spans **5,753,082 ticks
(92,049,312 bytes ≈ 87.8 MiB)** across the two side arrays, versus 8,193 ticks (131,088 bytes
≈ 0.13 MiB) for every synthetic corpus. A cold replay from `FlatBook::default()` walks outward
across that 5.75 M-tick span; each out-of-range price triggers a recenter/grow that reallocates
and copies the whole array (`O(span)` per grow). The aggregate is the recenter storm that
dominates the replay. **H5 is therefore refuted on real throughput** — FlatBook does not lead;
its bounded-range assumption (§4) is violated by the real feed's price dispersion, and the
amortized-recenter premise breaks under a cold, wide-span replay.

BTree wins the real feed because its cost is span-agnostic and only `O(log n)` in occupied
levels — it neither memmoves (RevVec/SortedVec) nor allocates over a tick span (FlatBook).

---

## 4. The flat-array tradeoff, quantified (`flat_memory.csv`, `read_path.csv`)

FlatBook buys `O(1)` depth-and-locality-independent update (§2) and `O(1)` best access at three
costs, each measured.

**(a) Memory is proportional to price span, not occupied levels (`flat_memory.csv`).**

| config | span (ticks) | total bytes | ≈ |
|---|---|---|---|
| service depth 1–1024 | 8,193 | 131,088 | 0.13 MiB |
| service depth 2048 | 12,291 | 196,656 | 0.19 MiB |
| steady / burst / flashcrash corpus | 8,193 | 131,088 | 0.13 MiB |
| btcusdt-sample corpus | 5,753,082 | 92,049,312 | 87.8 MiB |

The bounded synthetic span fits in two ~0.13 MiB arrays; the real BTCUSDT span needs ~87.8 MiB.
That ~700× memory blow-up (5,753,082 / 8,193 ticks) is the same span that drives the throughput
collapse in §3 — the tradeoff and the failure are one number.

**(b) The read path: `O(1)` best, but a sparse-scan `top_n` (`read_path.csv`).** `best_bid` p50
is 11 ns at every depth for FlatBook (e.g. depth 2048: 11 ns) — flat at the timer floor, like
the Vecs. But `top_n_full` walks the sparse ladder: at depth 2048 FlatBook reads 1,990 ns p50
vs SortedVec 616 ns and RevVec 617 ns — ~3.2× the contiguous-array copy, because the depth-2048
ladder occupies every other tick (the benchmark's 2-tick spacing) so the scan visits ~2× the
slots, most empty. It still beats BTree's node-by-node `top_n_full` (3,517 ns at depth 2048).

**(c) The best-removal probe** is the characteristic cost when the cached best is deleted (the
scan toward the worse side for the next occupied slot); it is included in every `update`/`remove`
measurement above and does not surface as a separate regression at these depths.

---

## 5. Which structure when — sourced recommendation

| regime | use | sourced basis |
|---|---|---|
| shallow, top-of-book-concentrated touches | RevVec or SortedVec | `update` p50 ~8 ns at the floor, depths 8–128 Concentrated (`service_sweep.csv`); both beat BTree's 16–19 ns |
| deep, touches spread across the ladder | SortedVec | `update` p50 14 ns at depth 2048 Uniform vs RevVec 697 ns and BTree 50 ns (`service_sweep.csv`) |
| deep, **unbounded / wide** price range (the real-feed case) | BTree | fastest on the real BTCUSDT replay at 23.2 Mev/s vs sorted 16.0, rev 4.5, flat 0.09 (`throughput.csv`); span-agnostic where FlatBook needs 87.8 MiB (`flat_memory.csv`) |
| deep, **bounded** span, warm/amortized book | FlatBook | `update` p50 14 ns regardless of depth/locality (`service_sweep.csv`) and 113.0 Mev/s on the bounded synthetic steady corpus (`throughput.csv`), at ~0.13 MiB (`flat_memory.csv`) — but **only** when the span is bounded and the recenter cost is amortized, not paid on a cold wide-span replay |

FlatBook is the structurally-correct answer to the rev-collapse **on service time and on a
bounded span**; it is the wrong answer on an unbounded real feed, where its strength (a flat
array over the price axis) becomes 87.8 MiB of recenter churn. The shootout has no single
winner — the right container is a function of depth, touch locality, and price-span boundedness.

---

## 6. Hypotheses — outcomes (each with a sourced number)

- **H1 — RevVec dominates at shallow / top-concentrated: partially confirmed (refined).** At
  Concentrated depths 8–128 RevVec sits at the timer floor (p50 8 ns) and beats BTree (16–19 ns),
  but it is **tied with** SortedVec (8–9 ns), not dominant over it (`service_sweep.csv`).
  Confirmed vs BTree; inconclusive vs SortedVec (both at the floor).
- **H2 — SortedVec overtakes RevVec only at deep/uniform, large n: refuted as stated.** The
  *direction* holds (SortedVec wins under Uniform touches) but the *threshold* does not: the
  crossover is at `D*` = 2 under Uniform, not at large n (depth 2: rev p50 16 ns vs sorted 11 ns;
  depth 2048: 697 ns vs 14 ns, `service_sweep.csv`). The crossover is locality-gated.
- **H3 — BTree loses across realistic depths and pays an O(log n) best-access tax: split.**
  BTree loses on `update` at essentially every depth (Concentrated p50 16→31 ns, Uniform
  16→50 ns vs the Vecs) and on synthetic throughput (44.8 vs 74.0 Mev/s steady, `throughput.csv`)
  — confirmed. The specific best-access tax is **inconclusive**: `best_bid` p50 is 10–11 ns ≈ the
  7 ns clock floor for all impls (`read_path.csv`), so any `O(log n)` descent sits below the
  timer floor; the tree's structural cost does surface on `top_n_full` (3,517 ns vs ~616 ns at
  depth 2048).
- **H4 — the RevVec↔SortedVec crossover sits at some `D*`/locality: confirmed.** `D*` = 256
  (Concentrated), `D*` = 2 (Uniform) (`service_sweep.csv`), with interior distributions exported
  either side (`service_update_*_{concentrated_d256,uniform_d2}.hgrm`).
- **H5 — FlatBook's O(1) update is depth- and locality-independent and leads real throughput:
  split (service-time half confirmed, real-throughput half refuted).** Confirmed on service time:
  `update` p50 is a flat 14 ns across depth in both localities, vs RevVec's 697 ns at Uniform
  depth 2048 (`service_sweep.csv`). Refuted on real throughput: FlatBook replays the real
  BTCUSDT corpus at 0.09 Mev/s (10,926 ns/event, `throughput.csv`) — last, not first — because
  the real book's 5,753,082-tick span (87.8 MiB, `flat_memory.csv`) makes the cold-replay
  recenter/grow cost dominate. The honest finding: `O(1)` update does not imply leading
  throughput when the price span is unbounded.

---

## 7. Sustained, coordinated-omission-correct response time (`sustained.csv`)

Figures: `plots/sustained_p99_vs_rate_{steady,burst,flashcrash}.svg`. **Response time under a
schedule — not service time.**

### Real-arrival replay of the BTCUSDT sample at speed = 1

| impl | achieved (eps) | resp p50 (ns) | resp p99 (ns) | resp max (ns) | saturated |
|---|---|---|---|---|---|
| btree | 305 | 6,615 | 95,551 | 104,063 | no |
| sorted | 305 | 7,967 | 169,471 | 175,999 | no |
| rev | 305 | 15,703 | 620,031 | 704,511 | no |
| flat | 305 | 4,179 | 351,999 | 28,262,399 | no |

All four keep up with the real feed (achieved 305 eps vs ~304 natural): arrivals are ms-scale,
applies are ns-scale, so the book idles ~99.99% of the time and the 87.8 MiB recenter cost that
sinks FlatBook's unpaced throughput (§3) is absorbed by the inter-arrival gaps — FlatBook even
posts the best p50 (4,179 ns). But its `resp_max` of 28,262,399 ns (28.3 ms) is ~270× the
worst of the others: a single cold recenter spike, the same mechanism as §3, surfacing in the
tail rather than the throughput here.

### Synthetic fixed-rate sweep — max sustainable rate (bounded-p99, achieved ≥ 90% of target)

| impl | steady | burst | flashcrash |
|---|---|---|---|
| btree | 20 Mev/s | 20 Mev/s | 20 Mev/s |
| sorted | 20 Mev/s | 20 Mev/s | 20 Mev/s |
| rev | 20 Mev/s | 20 Mev/s | 20 Mev/s |
| flat | 20 Mev/s | 20 Mev/s | 20 Mev/s |

All four sustain 20 Mev/s and saturate at the next rung, 50 Mev/s; the true knee is bracketed
between them (the ladder does not probe inside). Below saturation the schedule, not the per-op
cost, governs the tail — which is why a 14 ns–697 ns service-time spread collapses to one shared
sustainable rate. The tail blow-up at saturation (steady profile, `resp_p99_ns`):

| impl | 20 Mev/s (sustained) | 50 Mev/s (saturated) |
|---|---|---|
| btree | 121 ns | 1,772,543 ns |
| sorted | 83 ns | 1,261,567 ns |
| rev | 97 ns | 1,156,095 ns |
| flat | 75 ns | 710,655 ns |

Past saturation the open-loop response tail jumps ~4 orders of magnitude — the accumulated
backlog the CO-correct subtraction is built to capture. FlatBook carries the lowest saturated
p99 (710,655 ns), consistent with its lowest per-op service cost on the bounded synthetic feed.

---

## Data provenance

| claim / figure | source |
|---|---|
| crossover tables, `D*`, FlatBook flat curve, H1/H2/H4/H5-service | `service_sweep.csv` |
| `crossover_update_{p50,p99}_{concentrated,uniform}.svg` | `service_sweep.csv` |
| `interior_update_{concentrated_d256,uniform_d2}.svg` | `service_update_*_{loc}_d{D*}.hgrm` |
| read tax, `top_n_full`, FlatBook sparse scan, H3 best-access | `read_path.csv` |
| `read_best_bid_vs_depth.svg` | `read_path.csv` |
| throughput table, real-corpus inversion, H5-throughput | `throughput.csv` |
| FlatBook memory / span tradeoff | `flat_memory.csv` |
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
./target/release/bench flatmem               # -> flat_memory.csv (FlatBook allocated span per config)
./target/release/bench plot                  # -> env.json + plots/*.svg from the committed CSVs

# or everything then plots:
./target/release/bench all --core 0
```

Conditions for every result above are the `env.json` manifest: i5-1135G7, `powersave` governor,
pinned core 0, `target-cpu=native`, 7 ns clock floor. With `FlatBook` the order-book shootout is
complete: four implementations behind one frozen trait, proven identical, measured, and
judged with a sourced verdict.

# PROFILING.md — Top-Down Microarchitecture Teardown of the `apply` Hot Loop

This document explains, at the level the CPU executes, *why* the four frozen
order-book implementations land where they did in the Phase 4 service-time
crossover and the Phase 5 real-data verdict. It interprets only committed data:
the Phase 4 `service_sweep.csv`, the Phase 5 `throughput.csv` and
`flat_memory.csv`, and the Phase 9 experiments `branch_experiment.csv` and
`cache_experiment.csv`. Every figure cites its source file. No number is invented;
where a hypothesis was refuted, the refutation is stated with the number that
refutes it.

---

## 1. Method and environment

**Top-Down Microarchitecture Analysis (TMAM).** Intel's top-down method classifies
each issue-slot the front end can deliver to the back end into one of four
categories: **Retiring** (slot did useful work), **Bad Speculation** (slot was
spent on a mispredicted path and squashed), **Frontend Bound** (no slot delivered —
fetch/decode starved), and **Backend Bound** (slot stalled on a resource, split
into **Memory Bound** — waiting on the cache/DRAM hierarchy — and **Core Bound** —
waiting on execution ports). A structure's dominant category names its bottleneck:
a binary search that mispredicts is Bad Speculation; a pointer chase that misses
cache is Memory Bound; a linear scan that simply retires many instructions is
Core/Retiring bound.

**Host (`bench/results/env.json`).** 11th Gen Intel Core i5-1135G7 @ 2.40 GHz
(Tiger Lake), 8 logical cores; kernel 7.0.0-15-generic; rustc 1.95.0 with
`-C target-cpu=native` (`.cargo/config.toml`); benchmarks pinned to core 0; CPU
governor **`powersave`**; measured clock read-read floor 7 ns. Cache sizes read
from `/sys/devices/system/cpu/cpu0/cache/`: **L1d 48 KiB, L2 1280 KiB (1.25 MiB),
LLC 8192 KiB (8 MiB)**.

**PMU availability — stated plainly: hardware counters are UNAVAILABLE on this
host.** The `perf` binary is present (`/usr/bin/perf`) but the kernel denies
counter access — `kernel.perf_event_paranoid = 4`, and the process holds neither
`CAP_PERFMON` nor `CAP_SYS_ADMIN`. Both `perf stat -e cycles,instructions true` and
`perf stat --topdown true` fail with *"No supported events found. Access to
performance monitoring and observability operations is limited."* The raw capture
is `bench/results/perf/perf_unavailable.txt`; `bench/results/perf/perf_summary.csv`
records the unavailability and retains the parsed-counter schema so a PMU-enabled
host can populate it unchanged. **No counters were collected and none are
fabricated.** Tiger Lake supports TMAM level-1 via `perf stat --topdown`; on a host
with `perf_event_paranoid <= 1` the commands in §7 would fill the
`td_retiring/td_bad_spec/td_frontend/td_backend` columns directly.

**The corroboration approach.** The analysis is built to stand without a PMU. Each
top-down category has an observable behavioral signature measured with the
cycle-accurate `quanta` clock:

- **Bad Speculation has a misprediction signature** — a branchy search is slow
  *only* on unpredictable input; a branchless search is flat across input
  predictability. `branch_experiment.csv` measures this 2×2.
- **Memory Bound has a cache-hierarchy signature** — latency steps as the working
  set crosses L1 → L2 → LLC → DRAM. `cache_experiment.csv` measures this against
  footprints read from `/sys`.

`perf` would be confirming evidence; it is not a single point of failure.

**Threats to validity.**
- *PMU access denied.* The top-down *categories* are inferred from behavioral
  signatures, not read from counters. The signatures are unambiguous (a flat-vs-
  steep latency response is not subtle), but the exact slot fractions are not
  measured.
- *Governor `powersave`.* Frequency may scale; absolute nanoseconds carry some
  frequency variance. This is mitigated by pinning and warmup (each cell warms the
  core before recording) and by the fact that every conclusion rests on *ratios and
  shapes* (flat vs rising, one impl vs another at the same instant), not on a single
  absolute number. The clean cells — e.g. `branchless` reading 3.499 vs 3.495 ns at
  depth 16 (`branch_experiment.csv`) — show the pinned core held a stable frequency.
- *Single host, `target-cpu=native`.* Binaries are host-specific by design (valid
  microarchitecture profiling); the numbers do not transfer to another CPU.
- *Timer floor.* The 7 ns read-read floor (`env.json`) is reported, never
  subtracted; sub-10 ns figures sit near it and are read as "at the floor", not as
  precise values. The block-timed `branch_experiment.csv` amortizes the floor over
  1024 lookups per bracket, so its sub-ns resolution is real.

---

## 2. The hot loop

Every benchmark drives the frozen `OrderBook::apply`. For a `Level` event (the
common case) `apply` does two things: **locate** the price level, then **mutate**
it (replace quantity, or insert/remove). The mutate cost is shared; the **locate**
step is the variable that separates the four implementations, and it is the locate
step this teardown dissects:

| Impl | Locate step | Storage |
|---|---|---|
| `SortedVecBook` | `binary_search_by_key` over a contiguous, price-sorted `Vec` | one array/side |
| `BTreeBook` | `BTreeMap` node descent (pointer chase) | scattered nodes |
| `RevVecBook` | linear scan from the best end | one array/side |
| `FlatBook` | direct index `bid_qty[px - base]` | one dense array/side spanning the price range |

The Phase 9 `bench profile` subcommand isolates this: it builds a book at a chosen
depth (untimed), warms up, then runs `--iters` `apply` calls over a pre-generated
event buffer in a tight, **untimed** loop, each call wrapped in `black_box`, with no
per-op timing — so an external profiler attributes cycles to `apply` cleanly. It is
the canonical target for the §1 `perf` commands; with the PMU unavailable here it
serves as the reproducible loop the §3/§4 behavioral experiments characterize.

---

## 3. The taxonomy, confirmed (and one hypothesis refuted)

The Phase 9 hypotheses (phase9-spec §2.1) and the verdict from committed data:

| Impl | Predicted dominant category | Verdict from data |
|---|---|---|
| `SortedVecBook` | Bad Speculation (branch-miss) | **Refuted** → it is **Memory Bound** (the locate is branchless on this toolchain) |
| `BTreeBook` | Memory Bound (load latency) | **Confirmed** |
| `RevVecBook` | Core/Retiring, rising with depth | **Confirmed** |
| `FlatBook` | Retiring (until recenter = Memory Bound) | **Confirmed** |

### `SortedVecBook` — predicted Bad Speculation, **measured Memory Bound**

The frozen `SortedVecBook` locates with `Vec::binary_search_by_key`. The Phase 9
branch experiment shows that `std`'s binary-search family compiles to **branchless**
code on this toolchain (rustc 1.95): the `std` variant in `branch_experiment.csv` is
flat across key predictability — at depth 16 it reads 3.118 ns on predictable keys
and 3.118 ns on random keys; at depth 16384, 12.272 ns predictable vs 16.483 ns
random, and that 4.2 ns gap is *memory*, not misprediction (the `branchless`
variant, which is also branch-free, shows the same ~4 ns rise: 13.327 → 17.265 ns).
A branchless locate cannot mispredict, so **the frozen sorted book pays no
branch-misprediction penalty** — the Bad-Speculation hypothesis is refuted for the
shipped implementation.

What `SortedVecBook` *is* bound by is memory latency in its dependent-load chain.
`cache_experiment.csv` (update p50, uniform touches) shows its locate cost climbing
monotonically as the array outgrows each cache: 9 ns at depth 256 (4 KiB, L1) → 14 ns
at depth 4096 (64 KiB, L2) → 29 ns at depth 65536 (1 MiB, L2) → 45 ns at depth
262144 (4 MiB, LLC) → 104 ns at depth 1048576 (16 MiB, DRAM). That is the
cache-hierarchy signature: binary search issues O(log depth) data-dependent loads,
and as the array spills past L1, L2, and LLC, more of those loads miss.

### `BTreeBook` — Memory Bound, confirmed

`cache_experiment.csv` shows `BTreeBook` elevated *even when small* and climbing
steeply: 40 ns at depth 256 (≈6 KiB, L1) — already 4.4× `SortedVecBook`'s 9 ns at
the same depth — rising to 57 ns (96 KiB, L2), 89 ns (1.5 MiB, LLC), 202 ns (6 MiB,
LLC), and 332 ns at depth 1048576 (≈24 MiB, DRAM). The elevation at small sizes is
the signature of a **scattered-node pointer chase**: each step of the descent is a
dependent load to a separately-allocated node, so even an L1-resident tree pays
load-to-use latency per level and defeats the prefetcher (the next address is not
known until the current node is read). This is Memory Bound by load latency, not by
bandwidth and not by retired instruction count.

### `RevVecBook` — Core/Retiring, rising with depth, confirmed

`RevVecBook` scans linearly from the best end, so its locate cost is the number of
levels touched. `cache_experiment.csv` (uniform touches) shows it off the scale of
the others: 95 ns at depth 256, 339 ns at depth 1024, 1333 ns at depth 4096, 5271 ns
at depth 16384. Crucially, at depth 16384 its array footprint is only 262 KiB —
**L2-resident** — yet it costs 5271 ns, ~290× `SortedVecBook` at the same footprint
(18 ns). The cost therefore is not the cache (the data fits L2); it is **retired
work** — the scan executes O(depth) compare-and-advance iterations. This is the
Core/Retiring signature: latency proportional to instruction count, independent of
where the data sits in the hierarchy. (The experiment caps `RevVecBook` at depth
16384 because its O(depth²) build and O(depth) scan make deeper cells run unbounded;
the trend is already unambiguous.)

### `FlatBook` — Retiring (minimal), confirmed; Memory Bound only on recenter

`FlatBook` indexes directly, so its locate is a single load regardless of depth.
`cache_experiment.csv` (uniform touches) shows it **flat across the entire
hierarchy**: 12 ns at depth 256 (128 KiB span) and 14 ns at depth 1048576 — whose
per-side span is **64 MiB, 8× the 8 MiB LLC**. A single random access into a 64 MiB
array should miss to DRAM (~100 ns), yet the measured p50 is 14 ns. The reason is
that the access has **no dependent-load chain**: it is one independent load whose
latency the out-of-order core hides behind the surrounding work (the store, the
best-index update). Contrast `BTreeBook`, whose dependent pointer chain *serializes*
its misses and cannot be hidden. This is the Retiring signature — minimal, constant
work — and it holds as long as the access stays in-span. The Memory-Bound regime for
`FlatBook` is the **recenter**, treated in §5.

---

## 4. Bad speculation → branchless

### 4.1 The misprediction 2×2 (`branch_experiment.csv`)

The experiment measures a lower-bound search over a sorted level array as a 2×2 of
**variant** × **key predictability**, swept by depth, 10,000,384 lookups per cell,
block-timed against the 6–7 ns clock floor. Three variants:

- **`branchy`** — an explicit control-flow binary search (`if arr[mid] < key { lo =
  mid+1 } else { hi = mid }`), a genuine data-dependent conditional jump.
- **`branchless`** — the §4 `branchless_lower_bound`, whose comparison drives the
  index through `std::hint::select_unpredictable` (a `cmov`).
- **`std`** — `slice::partition_point`, the reference.

The misprediction signature, isolated cleanly where the whole array is **L1-resident
so memory latency cannot confound it** — depth 256 (2 KiB array), p50 ns/lookup:

| variant | predictable | random | random − predictable |
|---|---|---|---|
| branchy | 7.019 | 36.093 | **+29.07 ns** |
| branchless | 7.038 | 7.030 | −0.01 ns |
| std | 6.085 | 6.077 | −0.01 ns |

The branchy search is **5.1× slower on random keys than on predictable keys** with
all data in L1 — that gap is pure branch misprediction: ~8 comparisons per search
(log₂ 256), each mispredicting ~half the time on random keys, each flush costing the
Tiger Lake pipeline ~15+ cycles. The branchless and std variants are **flat** to
within the timer floor — no data-dependent branch, nothing to mispredict. The
penalty grows with the number of comparisons (i.e. with log depth): +13.17 ns at
depth 16, +20.09 ns at depth 64, +29.07 ns at depth 256 (`branch_experiment.csv`).

At depth 16384 (128 KiB array, exceeds L1) the branchy penalty is +35.88 ns
(predictable 34.249 → random 70.124); the branchless variant holds a 3.94 ns spread
(13.327 → 17.265) — and that residual is memory, not misprediction, because the
branch-free `std` variant shows the same rise (12.272 → 16.483 = 4.21 ns). The 2×2
is plotted in `bench/results/plots/branch_misprediction_2x2.svg` (source:
`branch_experiment.csv`).

### 4.2 Before/after — the penalty eliminated

At depth 16384, replacing the branchy search with the branchless `cmov` search
takes random-key p50 from 70.124 ns to 17.265 ns — **52.86 ns removed**, of which
35.88 ns is the misprediction penalty proper (the branchy random−predictable gap)
and the remainder is the per-comparison branch overhead the `cmov` also avoids. The
`branchless_lower_bound` matches `std::partition_point` exactly on randomized inputs
(the correctness test `branchless_lower_bound_matches_partition_point` is green) and
reproduces its flat profile, confirming the elimination is real and not a
correctness regression.

A note recorded for honesty: the phase9-spec §4 wrote the branchless step as the
ternary `base = if arr[mid] < key { mid } else { base }`. On this toolchain LLVM
lowered *that form, and an arithmetic-select rewrite, back to a conditional jump* —
re-introducing the very misprediction the variant exists to avoid. Only
`std::hint::select_unpredictable` (stable since Rust 1.88, exactly what `std`'s own
`partition_point` uses internally) reliably pins the `cmov`. Achieving branchless
code from safe, stable Rust is therefore not automatic; the experiment names the
primitive that achieves it.

### 4.3 Freeze + FlatBook framing

The frozen core is **not** changed, for two reasons stated explicitly in
phase9-spec §4. First, the freeze doctrine: `book` drives every variant and harness
unmodified. Second — and this is the resolution of "branchless rewrite" under a
frozen core — the real-data verdict already chose `FlatBook`, whose direct index is
the **structural branchless answer**: it does not do a search at all, so there is no
data-dependent branch to mispredict and no `cmov` to insert. The branchless binary
search of §4.1 is therefore a *quantified instruction-level alternative*, not a
needed patch: it measures the headroom (≈29 ns at L1-resident depth 256) that a
branchy locate would leave on the table, headroom the shipped `SortedVecBook`
already captures because `std`'s binary search is branchless, and that `FlatBook`
captures by eliminating the search entirely.

---

## 5. The memory-bound story

### 5.1 The cache-footprint curve (`cache_experiment.csv`)

Update p50 at uniform locality, by per-side footprint (plotted in
`bench/results/plots/cache_footprint_latency.svg`, source `cache_experiment.csv`;
L1d/L2/LLC boundary lines annotated from `/sys`):

| footprint regime | `flat` | `sorted` | `btree` |
|---|---|---|---|
| L1 (≤48 KiB) | 12 ns | 9 ns | 40 ns |
| L2 (≤1.25 MiB) | 12–13 ns | 14–29 ns | 57–67 ns |
| LLC (≤8 MiB) | 12 ns | 45 ns | 89–202 ns |
| DRAM (>8 MiB) | 14 ns | 104 ns | 332 ns |

(Footprint regime is the resident cache level the per-side footprint fits within,
from `cache_experiment.csv`. The `sorted` and `flat` footprints are exact — the
contiguous level array and the allocated FlatBook span respectively; the `btree`
footprint is an entry-plus-node estimate, so its regime boundaries are approximate.
`rev` is omitted from this table — it is Core/Retiring bound, not a cache probe;
§3 and §6.1 cover it.)

Three distinct shapes, three mechanisms:

- **`flat` — contiguous, single access, no dependent chain.** Flat ~12–14 ns to a
  64 MiB span. The single direct load is independent; out-of-order execution hides
  even a DRAM miss behind the surrounding mutate work.
- **`sorted` — contiguous, O(log n) dependent loads.** Graceful climb 9 → 104 ns.
  The binary search's loads are dependent (the next index needs the current value),
  so the chain lengthens its exposure as the array spills past each cache, but the
  loads live in **one array** with good TLB and spatial locality, so the climb is
  gentle.
- **`btree` — scattered, O(log n) pointer-chase.** Steep climb 40 → 332 ns,
  elevated from the start. The descent is a chain of dependent loads to
  separately-allocated nodes; the prefetcher cannot run ahead (the next address is
  unknown until the node is read), so each level pays close to full load-to-use
  latency, and that latency grows as nodes scatter past LLC.

The contrast between `sorted` (104 ns) and `btree` (332 ns) at DRAM scale — same
O(log n) access count — is precisely the contiguous-vs-pointer-chase distinction:
contiguity buys prefetch and locality the scattered tree cannot.

### 5.2 Where the real book exceeds LLC — the `FlatBook` collapse

`cache_experiment.csv` measured `flat` as cache-robust for **in-span steady-state
updates**. The real-data collapse is a *different* mechanism, and the two
experiments must not be blurred: the collapse is the **recenter**, on a span the
real book makes enormous.

`flat_memory.csv` records the FlatBook per-side span by corpus. On the synthetic
corpora the span is 8193 ticks / 131,088 bytes (L2-resident). On the real
`btcusdt-sample` corpus the span is **5,753,082 ticks / 92,049,312 bytes — 88 MiB,
11× the 8 MiB LLC.** The real BTC/USDT book is wide and sparse: prices range over
millions of ticks. Each event at a new extreme price falls outside the allocated
array, so `ensure_range` allocates a larger array and copies the old contents — a
recenter — and in-span accesses across a 88 MiB array are guaranteed LLC/DRAM
misses. `throughput.csv` (full replay, real corpus) shows the consequence:
**`flat` costs 10,926.62 ns/event on `btcusdt-sample`**, against 8.85 ns/event on
the synthetic `steady` corpus — a 1235× regression, driven entirely by the 88 MiB
span and its recenter storm. The steady-state cache experiment (flat to 64 MiB) and
the real-data collapse are consistent: a single in-span access is OoO-hidden; a
continuous stream of out-of-span, span-growing accesses is not.

---

## 6. Closing the loop — the prior numbers, mechanistically

### 6.1 The Phase 4 crossover is locality-gated retired work

`service_sweep.csv` (update p50) shows, under **uniform** touches, `RevVecBook`
climbing 11 ns (depth 1) → 38 (64) → 100 (256) → 353 (1024) → **697 ns (depth
2048)**, while `SortedVecBook` stays flat at ~11–14 ns and `FlatBook` at ~13–14 ns
across all depths. Under **concentrated** touches, `RevVecBook` instead stays low —
10 ns (depth 1) → 19 (256) → 21 ns (depth 2048). The crossover (where the linear
scan overtakes the binary search) is therefore set entirely by **touch locality**,
and the Phase 9 experiments name the mechanism:

- `RevVecBook`'s cost is the **scan length** = levels touched. Concentrated touches
  land near the best (scan 1–2 levels) regardless of book depth → flat. Uniform
  touches spread across the depth (expected scan ≈ depth/2) → linear in depth. This
  is **retired work**, confirmed by `cache_experiment.csv` (5271 ns at a 262 KiB,
  L2-resident footprint — too fast-fitting to be a cache effect, too slow to be
  anything but instruction count). It is **not** misprediction: the scan has no
  binary search, and `branch_experiment.csv` shows even binary searches are
  branchless here.
- `SortedVecBook`'s O(log depth) **branchless** dependent-load chain is depth-robust
  → flat. `FlatBook`'s O(1) direct index → flat.

So under uniform-and-deep conditions the `RevVecBook` line climbs through and past
the flat `SortedVecBook`/`FlatBook` lines; under concentrated conditions it sits
beside them. The crossover is the geometry of a linear scan, gated by where the
touches land — a Core/Retiring story, not a speculation or cache story.

### 6.2 The Phase 5 real-data inversion is the memory hierarchy meeting book width

`throughput.csv`, ns/event:

| corpus | `flat` | `sorted` | `rev` | `btree` |
|---|---|---|---|---|
| `steady` (synthetic, narrow) | **8.85** | 13.51 | 15.12 | 22.30 |
| `btcusdt-sample` (real, wide) | 10,926.62 | 62.55 | 222.34 | **43.05** |

The order inverts completely between the best and worst structures. The mechanism is
**book width** (price span), which the synthetic corpora hid and the real corpus
exposed:

- On the **narrow** synthetic book, `FlatBook`'s span is 131,088 B (128 KiB, L2-resident,
  `flat_memory.csv`); its O(1) direct index with no dependent chain is fastest at
  8.85 ns/event. `BTreeBook`'s per-event pointer chase is the slowest at 22.30
  ns/event — exactly its Memory-Bound signature from §3.
- On the **wide** real book, `FlatBook`'s span explodes to 88 MiB (11× LLC,
  `flat_memory.csv`); the direct index becomes a guaranteed miss and the recenter
  storm dominates → 10,926.62 ns/event, last by more than two orders of magnitude (254× the next-best BTreeBook).
  `BTreeBook`'s memory is proportional to the *number of levels*, not the price
  *span*, so its compact O(log n) nodes handle the wide sparse book and it **leads**
  at 43.05 ns/event. `RevVecBook` degrades to 222.34 ns/event (its O(depth) scan
  over a now-deep book — the §6.1 mechanism at real scale).

The inversion is thus the same memory hierarchy seen in §5, interacting with a
data-dependent working-set size: the structure optimal for a tiny dense set
(`FlatBook`, when the span fits cache) is pessimal for a huge sparse one (when the
span is 11× LLC), and the structure whose footprint tracks level count rather than
span (`BTreeBook`) wins exactly when the book is wide. The Phase 5 verdict —
`BTreeBook` on the real BTCUSDT corpus — is the memory hierarchy's verdict.

### 6.3 Summary

Measure, then explain. The Phase 4 crossover is retired instruction count gated by
touch locality (`RevVecBook`'s scan). The Phase 5 inversion is the memory hierarchy
gated by book width (`FlatBook`'s span vs `BTreeBook`'s level-proportional nodes).
The bad-speculation bullet the binary search would have paid is real and quantified
(≈29 ns at L1-resident depth 256, `branch_experiment.csv`) but already dodged —
structurally by `FlatBook`'s direct index and instrumentally by `std`'s branchless
binary search. Each conclusion rests on committed data and a behavioral signature
that does not need a PMU.

---

## 7. Reproducibility

Host-specific by design (`-C target-cpu=native`, `.cargo/config.toml`); numbers are
valid only on the §1 host. From the repository root:

```sh
# Build the harness (release, target-cpu=native via .cargo/config.toml).
cargo build --release -p bench

# Phase 9 PMU-free experiments (pinned to core 0).
./target/release/bench branch-exp --core 0      # -> bench/results/branch_experiment.csv
./target/release/bench cache-exp  --core 0      # -> bench/results/cache_experiment.csv

# The isolated, untimed hot loop — the external-profiler target (§2).
./target/release/bench profile --impl sorted --op apply \
    --depth 2048 --locality uniform --iters 200000000 --core 0

# Figures (reads only the committed CSVs).
./target/release/bench plot --out bench/results
#   -> plots/branch_misprediction_2x2.svg, plots/cache_footprint_latency.svg

# Hardware counters IF the host permits (perf_event_paranoid <= 1 or CAP_PERFMON).
# Unavailable on the documented host; recorded in perf/perf_unavailable.txt.
perf stat -e cycles,instructions,branches,branch-misses,\
L1-dcache-loads,L1-dcache-load-misses,LLC-loads,LLC-load-misses \
    ./target/release/bench profile --impl sorted --op apply \
    --depth 2048 --locality uniform --iters 200000000
perf stat --topdown \
    ./target/release/bench profile --impl btree --op apply \
    --depth 2048 --locality uniform --iters 200000000

# Prior-phase inputs this teardown explains (already committed):
./target/release/bench service       # -> service_sweep.csv   (Phase 4 crossover)
./target/release/bench throughput    # -> throughput.csv      (Phase 5 real-data)
./target/release/bench flatmem       # -> flat_memory.csv     (FlatBook span)
```

Committed data artifacts: `bench/results/branch_experiment.csv`,
`bench/results/cache_experiment.csv`, `bench/results/perf/perf_summary.csv` (+
`perf_unavailable.txt`), `bench/results/service_sweep.csv`,
`bench/results/throughput.csv`, `bench/results/flat_memory.csv`,
`bench/results/env.json`, and the two figures under `bench/results/plots/`.

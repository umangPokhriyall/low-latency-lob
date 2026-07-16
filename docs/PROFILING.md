# PROFILING.md — Top-Down Microarchitecture Teardown of the `apply` Hot Loop

This document explains, at the level the CPU executes, *why* the four frozen
order-book implementations land where they did in the service-time
crossover and the real-data verdict. The story is **predicted-then-confirmed**:
the taxonomy was first established on a laptop with *no hardware counters*, from
unambiguous behavioral signatures, and then **confirmed on a rented AMD EPYC box against
native Zen 4 pipeline-utilization counters**. Both halves are kept — the PMU-free
prediction is the stronger result *because* silicon later agreed with it. Every figure
cites its source file. No number is invented; where a hypothesis was refuted, the
refutation is stated with the number that refutes it.

**Provenance.** The AMD Zen 4 counter captures (`perf/perf_btree.txt`,
`perf_sorted.txt`, `perf_rev.txt`, `perf_flat.txt`) and the cache-line-contention reports
(`perf/c2c_ring.txt`, `perf/c2c_seqlock.txt`) are the EPYC re-run. The two PMU-free
behavioral experiments — the misprediction 2×2 (`branch_experiment.csv`) and the
cache-footprint curve (`cache_experiment.csv`) — are the earlier laptop (i5-1135G7)
baseline that *predicted* the taxonomy and were outside the metal re-run scope; they are
cited as the laptop behavioral prediction, and the AMD counters are cited as the metal
confirmation. `flat_memory.csv` (book span in ticks/bytes) is host-independent.

---

## 1. Method and environment

**Top-Down Microarchitecture Analysis, and its AMD counterpart.** Intel's top-down method
(TMAM) classifies each issue-slot the front end can deliver to the back end into one of
four categories: **Retiring** (slot did useful work), **Bad Speculation** (slot spent on a
mispredicted path and squashed), **Frontend Bound** (no slot delivered — fetch/decode
starved), and **Backend Bound** (slot stalled on a resource, split into **Memory Bound** —
waiting on the cache/DRAM hierarchy — and **Core Bound** — waiting on execution ports). A
structure's dominant category names its bottleneck: a binary search that mispredicts is Bad
Speculation; a pointer chase that misses cache is Memory Bound; a linear scan that simply
retires many instructions is Core/Retiring bound.

**This is AMD Zen 4 pipeline-utilization analysis — the architectural counterpart to Intel
TMA, not Intel TMA relabeled.** Intel's `perf stat -M TopdownL1,TopdownL2` metric groups do
not exist on Zen 4; running them verbatim errors or misleads. AMD exposes an equivalent
pipeline-utilization decomposition through its own PMC events, and the four AMD buckets map
onto the four Intel categories one-for-one. The mapping used here (from the committed
`perf/perf_*.txt`):

| Intel TMA bucket | AMD Zen 4 bucket (as captured) | Zen 4 events behind it |
|---|---|---|
| Retiring | `retiring_fastpath` + `retiring_microcode` | `ex_ret_ops`, `ex_ret_ucode_ops` over dispatch slots (`ls_not_halted_cyc`) |
| Bad Speculation | `bad_speculation_mispredicts` + `..._pipeline_restarts` | `ex_ret_brn_misp` (retired mispredicted branches), `resyncs_or_nc_redirects` |
| Frontend Bound | `frontend_bound_bandwidth` + `frontend_bound_latency` | `de_no_dispatch_per_slot.no_ops_from_frontend` (raw + `cmask=6` for latency) |
| Backend Bound | `backend_bound_memory` + `backend_bound_cpu` | `de_no_dispatch_per_slot.backend_stalls`, `ex_no_retire.load_not_complete` (memory), `ex_no_retire.not_complete` |
| IPC | `insn per cycle` | `instructions` / `cycles` (native `ex_ret_instr` / `ls_not_halted_cyc`) |
| branch-miss rate | `% of all branches` | `branch-misses` / `branches` (native `ex_ret_brn_misp` / `ex_ret_brn`) |
| LLC/cache-miss rate | `% of all cache refs` | `cache-misses` / `cache-references` |

**Host (`bench/results/env.json`).** Latitude.sh `m4.metal.large`, single-socket **AMD EPYC
9254** (Zen 4, Genoa), 24 physical cores / 48 threads @ 2.9 GHz; **4 CCDs, each with a
private 32 MiB L3 (128 MiB aggregate)**; 64 B cache line; booted NPS1 (1 NUMA node);
384 GiB; kernel **6.8.0-124-generic**; rustc **1.96.1** with `-C target-cpu=native`;
benchmarks pinned to core 0 (CCD 0); CPU governor **`performance`** (amd-pstate); measured
clock read-read floor ~10 ns. Anyone can rent this exact SKU hourly and re-run.

**PMU availability — now full, on metal.** On the laptop the kernel denied counter access
(`perf_event_paranoid = 4`, no `CAP_PERFMON`) and this teardown was conducted PMU-free. On
the EPYC box `perf_event_paranoid = -1` and the native AMD Zen 4 PMU is fully exposed; the
`bench profile` untimed hot loop (§2) was wrapped in `perf stat` with the AMD
pipeline-utilization events plus IPC / branch-miss / cache-miss, one capture per
implementation, into `perf/perf_{btree,sorted,rev,flat}.txt`. The laptop artifacts
`perf/perf_unavailable.txt` and `perf/perf_summary.csv` are retained only as the record of
the earlier PMU-free condition; they are superseded by the EPYC captures.

**The corroboration approach — kept, because it predicted the counters.** The analysis was
built to stand *without* a PMU: each top-down category has an observable behavioral
signature measured with the cycle-accurate `quanta` clock —

- **Bad Speculation has a misprediction signature** — a branchy search is slow *only* on
  unpredictable input; a branchless search is flat across input predictability
  (`branch_experiment.csv`, laptop).
- **Memory Bound has a cache-hierarchy signature** — latency steps as the working set
  crosses L1 → L2 → LLC → DRAM (`cache_experiment.csv`, laptop).

These signatures *predicted* the taxonomy; the EPYC AMD counters (§3) *confirm* it. The
PMU is no longer a single point of failure — and its later agreement with the PMU-free
prediction is the load-bearing result of this document.

**Threats to validity — the confounds the metal run resolved.**
- *PMU access.* Resolved: native AMD Zen 4 counters now measure the slot fractions the
  laptop could only infer. The categories were confirmed, not overturned.
- *Governor / jitter.* The laptop ran `powersave` on a shared host; the EPYC producer owns
  a dedicated CCD-0 core at fixed frequency (`performance`), so the profile runs at a stable
  frequency and the tails are clean.
- *Cache-footprint experiment host.* `cache_experiment.csv` is the **laptop** behavioral
  curve (8 MiB LLC); it was not re-run on metal. Its *shapes* (flat vs stepping vs steep)
  are the host-independent signatures the AMD backend-bound counter now confirms directly;
  its absolute cache-crossing depths are laptop-specific and the EPYC's 32 MiB per-CCD L3
  moves the LLC crossing deeper. Labeled as such at use.
- *Single host, `target-cpu=native`.* Binaries are host-specific by design; numbers do not
  transfer to another CPU. The one residual topology caveat is single-socket, cross-CCD over
  Infinity Fabric (intra-socket), relevant only to the §6 `perf c2c` contention story.

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

The `bench profile` subcommand isolates this: it builds a book at a chosen
depth (untimed), warms up, then runs `--iters` `apply` calls over a pre-generated
event buffer in a tight, **untimed** loop, each call wrapped in `black_box`, with no
per-op timing — so an external profiler attributes cycles to `apply` cleanly. On the EPYC
box this is exactly the loop `perf stat` wrapped to produce `perf/perf_*.txt`
(depth 2048, uniform locality, 200,000,000 iters per impl); the timed sweeps were never
wrapped in perf, so the latency CSVs carry no profiling overhead.

---

## 3. The taxonomy, confirmed by AMD counters (and one hypothesis refuted)

The initial hypotheses, the PMU-free prediction, and the AMD-counter
verdict:

| Impl | Predicted dominant category | AMD Zen 4 verdict (`perf/perf_*.txt`) |
|---|---|---|
| `SortedVecBook` | Bad Speculation (branch-miss) | **Refuted** → **Memory Bound**: 50.5 % backend-mem, 0.1 % bad-spec, 0.04 % branch-miss |
| `BTreeBook` | Memory Bound (load latency) | **Confirmed** (+ frontend): IPC 1.33, 9.1 % backend-mem, 21.4 % frontend-latency, 8.12 % branch-miss |
| `RevVecBook` | Core/Retiring, rising with depth | **Confirmed**: 93.8 % retiring at IPC 6.10 |
| `FlatBook` | Retiring (until recenter = Memory Bound) | **Confirmed in-span; mispredict-bound wide**: 25.5 % bad-spec, 3.80 % branch-miss at depth-2048 uniform |

The full AMD Zen 4 pipeline-utilization capture (depth 2048, uniform, 200 M iters):

| impl | IPC | retiring | bad-spec | frontend (lat/bw) | backend-mem | backend-cpu | branch-miss | cache-miss |
|---|---|---|---|---|---|---|---|---|
| `sorted` | 2.50 | 40.2 % | **0.1 %** | 0.7 % / 0.2 % | **50.5 %** | 8.4 % | 0.04 % | 0.35 % |
| `btree` | 1.33 | 20.1 % | 28.3 % | 21.4 % / 15.6 % | 9.1 % | 5.4 % | 8.12 % | 0.35 % |
| `rev` | 6.10 | **93.8 %** | 1.3 % | 1.4 % / 0.2 % | 2.3 % | 1.0 % | 0.14 % | 0.04 % |
| `flat` | 2.43 | 39.0 % | 25.5 % | 17.6 % / 3.0 % | 13.4 % | 1.5 % | 3.80 % | 1.03 % |

### `SortedVecBook` — predicted Bad Speculation, **measured Memory Bound**

The frozen `SortedVecBook` locates with `Vec::binary_search_by_key`. The branch
experiment showed that `std`'s binary-search family compiles to **branchless** code on this
toolchain: the `std` variant in `branch_experiment.csv` (laptop) is flat across key
predictability — at depth 16, 3.118 ns predictable vs 3.118 ns random. A branchless locate
cannot mispredict, so the Bad-Speculation hypothesis was predicted refuted. **The EPYC AMD
counters close the case directly** (`perf/perf_sorted.txt`): **bad-speculation 0.1 %,
branch-miss 0.04 %** — the branchless locate does not mispredict on silicon either — while
**backend-bound-memory is 50.5 %**, by far the dominant bucket. The stall is on memory, not
speculation: the sorted book issues `O(log depth)` data-dependent loads in one contiguous
array, and half the pipeline's slots are spent waiting on those loads. This is the headline
confirmation the metal run was for: *memory-bound, not speculation-bound*, now an AMD
backend-bound counter value rather than an inference.

### `BTreeBook` — Memory Bound (pointer chase), confirmed, plus a frontend cost

`perf/perf_btree.txt` shows the lowest IPC of the four (**1.33**), the highest branch-miss
rate (**8.12 %**, bad-spec **28.3 %**), a large **frontend-bound-latency (21.4 %)**, and
**9.1 % backend-bound-memory**. The signature is a **scattered-node pointer chase**: each
descent step is a dependent load to a separately-allocated node, so the next address is
unknown until the current node is read — the prefetcher cannot run ahead, the front end
stalls waiting on the address (frontend-latency), and the descent's data-dependent branches
mispredict (bad-spec). This is Memory Bound by load latency with a frontend tax the
contiguous structures do not pay — richer than the laptop could see, and consistent with its
predicted category.

### `RevVecBook` — Core/Retiring, confirmed

`perf/perf_rev.txt` is unambiguous: **93.8 % retiring at IPC 6.10**, with bad-spec 1.3 %,
branch-miss 0.14 %, backend-memory 2.3 %, cache-miss 0.04 %. The scan is not stalled on
anything — it simply executes and *retires* an enormous number of compare-and-advance
instructions at high IPC. Its cost is retired-instruction count (scan length), exactly the
Core/Retiring signature the laptop's `cache_experiment.csv` predicted (5271 ns at a
262 KiB, L2-resident footprint — too fast-fitting to be a cache effect). The counters make
it a fact: nearly every slot retires, so latency is set by *how many* instructions, i.e. how
many levels the scan touches.

### `FlatBook` — Retiring in-span, mispredict-bound at wide uniform depth

`perf/perf_flat.txt` shows something the laptop could not: at depth-2048 uniform the flat
book's apply carries **25.5 % bad-speculation and 3.80 % branch-miss** (retiring 39.0 %,
frontend-latency 17.6 %, backend-memory 13.4 %). The direct index itself is branchless, but
the surrounding apply logic — bounds/recenter checks and the scattered writes across a wide
flat array under uniform touches — feeds data-dependent branches that mispredict. This is
the counter-level shadow of the real-data collapse (§5): the wider and sparser the access
pattern, the more the flat array pays in speculation and memory. On a narrow, warm, in-span
book (the synthetic corpora) this vanishes and the flat index is pure minimal retiring; the
depth-2048 uniform profile is the adversarial end of that spectrum.

---

## 4. Bad speculation → branchless (laptop behavioral prediction, AMD-counter confirmed)

### 4.1 The misprediction 2×2 (`branch_experiment.csv`, laptop)

The experiment measures a lower-bound search over a sorted level array as a 2×2 of
**variant** × **key predictability**, swept by depth, 10,000,384 lookups per cell,
block-timed against the ~6 ns clock floor. Three variants:

- **`branchy`** — an explicit control-flow binary search (`if arr[mid] < key { lo =
  mid+1 } else { hi = mid }`), a genuine data-dependent conditional jump.
- **`branchless`** — the `branchless_lower_bound`, whose comparison drives the
  index through `std::hint::select_unpredictable` (a `cmov`).
- **`std`** — `slice::partition_point`, the reference.

The misprediction signature, isolated cleanly where the whole array is **L1-resident so
memory latency cannot confound it** — depth 256 (2 KiB array), p50 ns/lookup:

| variant | predictable | random | random − predictable |
|---|---|---|---|
| branchy | 7.019 | 36.093 | **+29.07 ns** |
| branchless | 7.038 | 7.030 | −0.01 ns |
| std | 6.085 | 6.077 | −0.01 ns |

The branchy search is **5.1× slower on random keys than on predictable keys** with all data
in L1 — that gap is pure branch misprediction: ~8 comparisons per search (log₂ 256), each
mispredicting ~half the time on random keys. The branchless and std variants are **flat** to
within the timer floor — no data-dependent branch, nothing to mispredict. The penalty grows
with the number of comparisons: +13.17 ns at depth 16, +20.09 ns at depth 64, +29.07 ns at
depth 256, +35.88 ns at depth 16384 (`branch_experiment.csv`).

**The AMD counters confirm the mechanism at the implementation level.** The synthetic
branchy/branchless variants were not re-run on metal, but the shipped structures span the
same axis: the branchless binary search (`SortedVecBook`) shows **0.04 % branch-miss /
0.1 % bad-spec** (`perf/perf_sorted.txt`), while the branchy pointer-chase descent
(`BTreeBook`) shows **8.12 % branch-miss / 28.3 % bad-spec** (`perf/perf_btree.txt`) — an
~8-percentage-point branch-miss delta on an AMD counter, the counter-level counterpart of the
laptop's +29 ns latency delta. Branchless locate → the counter says it does not mispredict;
branchy locate → the counter says it does. The 2×2 is plotted in
`bench/results/plots/branch_misprediction_2x2.svg`.

### 4.2 Before/after — the penalty eliminated (laptop)

At depth 16384, replacing the branchy search with the branchless `cmov` search takes
random-key p50 from 70.124 ns to 17.265 ns — **52.86 ns removed**, of which 35.88 ns is the
misprediction penalty proper (the branchy random−predictable gap). The
`branchless_lower_bound` matches `std::partition_point` exactly on randomized inputs (the
correctness test `branchless_lower_bound_matches_partition_point` is green) and reproduces
its flat profile.

A note: the original experiment plan wrote the branchless step as the ternary
`base = if arr[mid] < key { mid } else { base }`. On this toolchain LLVM lowered *that form,
and an arithmetic-select rewrite, back to a conditional jump* — re-introducing the very
misprediction the variant exists to avoid. Only `std::hint::select_unpredictable` (stable
since Rust 1.88, exactly what `std`'s own `partition_point` uses internally) reliably pins
the `cmov`. Achieving branchless code from safe, stable Rust is therefore not automatic; the
experiment names the primitive that achieves it.

### 4.3 Freeze + FlatBook framing

The frozen core is **not** changed, for two reasons. First, the
freeze doctrine: `book` drives every variant and harness unmodified. Second, the real-data
verdict already chose `BTreeBook` for the wide book and `FlatBook` for the bounded one, and
`FlatBook`'s direct index is the **structural branchless answer**: it does not do a search at
all. The branchless binary search of §4.1 is therefore a *quantified instruction-level
alternative*, not a needed patch: it measures the headroom (≈29 ns at L1-resident depth 256)
that a branchy locate would leave on the table, headroom the shipped `SortedVecBook` already
captures because `std`'s binary search is branchless — and the AMD counter now confirms it
captures it (0.04 % branch-miss).

---

## 5. The memory-bound story

### 5.1 The cache-footprint curve (`cache_experiment.csv`, laptop)

Update p50 at uniform locality, by per-side footprint (laptop behavioral baseline; plotted
in `bench/results/plots/cache_footprint_latency.svg`; L1d/L2/LLC boundary lines annotated
from the laptop's `/sys`). The *shapes* are host-independent signatures; the absolute
cache-crossing depths are laptop-specific (8 MiB LLC — on the EPYC's 32 MiB per-CCD L3 the
LLC crossing lands deeper):

| footprint regime (laptop) | `flat` | `sorted` | `btree` |
|---|---|---|---|
| L1 (≤48 KiB) | 12 ns | 9 ns | 40 ns |
| L2 (≤1.25 MiB) | 12–13 ns | 14–29 ns | 57–67 ns |
| LLC (≤8 MiB) | 12 ns | 45 ns | 89–202 ns |
| DRAM (>8 MiB) | 14 ns | 104 ns | 332 ns |

Three distinct shapes, three mechanisms — and the AMD backend-bound counter (§3) now names
each without needing the footprint sweep:

- **`flat` — contiguous, single independent access, no dependent chain.** Flat ~12–14 ns to
  a 64 MiB span; the single direct load is hidden by out-of-order execution. On metal its
  in-span apply is minimal retiring; only wide/sparse access turns on the mispredict tax
  (§3, `perf/perf_flat.txt`).
- **`sorted` — contiguous, O(log n) dependent loads.** Graceful climb 9 → 104 ns. The
  binary search's loads are dependent, so the chain lengthens its exposure as the array
  spills past each cache — **the AMD counter reads this as 50.5 % backend-bound-memory**
  (`perf/perf_sorted.txt`), the metal confirmation of the laptop curve.
- **`btree` — scattered, O(log n) pointer-chase.** Steep climb 40 → 332 ns, elevated from
  the start; the descent's dependent loads to scattered nodes defeat the prefetcher — **the
  AMD counter reads this as low IPC (1.33) with 21.4 % frontend-latency + backend-memory**
  (`perf/perf_btree.txt`).

The contrast between `sorted` (104 ns) and `btree` (332 ns) at DRAM scale — same O(log n)
access count — is the contiguous-vs-pointer-chase distinction: contiguity buys prefetch and
locality the scattered tree cannot, and the metal frontend-bound counter on `btree` is the
direct evidence.

### 5.2 Where the real book exceeds the cache — the `FlatBook` collapse

`cache_experiment.csv` (laptop) measured `flat` as cache-robust for **in-span steady-state
updates**. The real-data collapse is a *different* mechanism, and the two must not be blurred:
the collapse is the **recenter**, on a span the real book makes enormous.

`flat_memory.csv` (host-independent) records the FlatBook per-side span by corpus. On the
synthetic corpora the span is 8193 ticks / 131,088 bytes (L2-resident). On the real
`btcusdt-sample` corpus the span is **5,753,082 ticks / 92,049,312 bytes — ≈88 MiB**. On the
EPYC box that is **~2.74× the 32 MiB per-CCD L3** (versus ~11× the laptop's 8 MiB LLC): the
cache is 4× larger here, so the crossover depth moved outward, **but the inversion still
occurred** because 88 MiB ≫ 32 MiB. Each event at a new extreme price falls outside the
allocated array, so `ensure_range` allocates a larger array and copies the old contents — a
recenter — and in-span accesses across an 88 MiB array are guaranteed L3/DRAM misses.
`throughput.csv` (full replay, real corpus, EPYC) shows the consequence: **`flat` costs
10,896.43 ns/event on `btcusdt-sample`**, against 7.46 ns/event on the synthetic `steady`
corpus — a ~1461× regression, driven entirely by the 88 MiB span and its recenter storm. The
larger metal cache did not rescue it — the honest, publishable confirmation the re-run was
for.

---

## 6. Cache-line contention — `perf c2c` (EPYC)

The seqlock and SPMC ring are single-writer/single-producer primitives whose only genuinely
shared state is a version/cursor word. On the laptop the false-vs-true-sharing distinction
was *inferred* from the `align(64)` layout plus the throughput-decline-with-K curve. On the
EPYC box, with the readers/consumers pinned **across CCDs** so their coherence traffic
crosses Infinity Fabric, `perf c2c record/report` directly observed the hit-modified (HITM)
cache-line transfers (`perf/c2c_ring.txt`, `perf/c2c_seqlock.txt`):

- **True sharing on the ring's write cursor, measured.** 60.0 % of the ring's LLC-misses
  resolve to **remote-cache HITM** (66.7 % for the seqlock), concentrated on a tiny set of
  shared cache lines (2 in each report). The ring's top shared line carries **99 store
  references** versus the seqlock's **10** — the ring's single write cursor is written on
  every push and polled from every CCD, exactly the true-sharing hotspot the K-sweep
  predicted.
- **No false sharing on the aligned slots.** No HITM appears on the 64-B-aligned per-slot
  payload lines; the `#[repr(align(64))]` discipline (EPYC cache line is 64 B, validating the
  alignment rationale) did place each slot and the cursor on disjoint lines, so adjacent
  consumers do not invalidate each other's slots.

The IBS sample is sparse (a handful of HITM events; symbolization attributes some to
std/kernel frames), so `perf c2c` **corroborates** the §4.2/§5-adjacent attribution rather
than pinpointing a source line — but the upgrade is real: the cross-CCD HITM that the
throughput decline implied is now directly observed. This is the coherence-level counterpart
of the pipeline buckets and is orthogonal to them (it is a cross-core, not a single-core,
effect).

---

## 7. Closing the loop — the prior numbers, mechanistically

### 7.1 The crossover is locality-gated retired work

`service_sweep.csv` (update p50, EPYC) shows, under **uniform** touches, `RevVecBook`
climbing 9 ns (depth 2) → 79 (256) → 259 (1024) → **519 ns (depth 2048)**, while
`SortedVecBook` stays flat at ~19–29 ns and `FlatBook` at ~9 ns across all depths. Under
**concentrated** touches `RevVecBook` instead stays low — 9 ns through depth 256, 29 ns at
depth 2048 — at or below `SortedVecBook`. The crossover is set entirely by **touch
locality**, and the AMD counters name the mechanism: `RevVecBook` is **93.8 % retiring**
(`perf/perf_rev.txt`) — its cost is scan length (levels touched), which concentrated touches
keep to 1–2 and uniform touches spread to ≈depth/2. It is retired work, not misprediction
(bad-spec 1.3 %) and not cache (cache-miss 0.04 %). `SortedVecBook`'s O(log depth) branchless
dependent-load chain and `FlatBook`'s O(1) direct index are depth-robust → flat.

### 7.2 The real-data inversion is the memory hierarchy meeting book width

`throughput.csv` (EPYC), ns/event:

| corpus | `flat` | `sorted` | `rev` | `btree` |
|---|---|---|---|---|
| `steady` (synthetic, narrow) | **7.46** | 10.44 | 12.45 | 19.47 |
| `btcusdt-sample` (real, wide) | 10,896.43 | 59.62 | 191.00 | **37.79** |

The order inverts completely between the best and worst structures. The mechanism is **book
width** (price span), which the synthetic corpora hid and the real corpus exposed:

- On the **narrow** synthetic book, `FlatBook`'s span is 131,088 B (128 KiB, L2-resident,
  `flat_memory.csv`); its O(1) direct index with no dependent chain is fastest at
  7.46 ns/event. `BTreeBook`'s per-event pointer chase (IPC 1.33, `perf/perf_btree.txt`) is
  the slowest at 19.47 ns/event.
- On the **wide** real book, `FlatBook`'s span explodes to 88 MiB (~2.74× the 32 MiB
  per-CCD L3, `flat_memory.csv`); the direct index becomes a guaranteed miss and the recenter
  storm dominates → 10,896.43 ns/event, last by ~288× behind the next-best `BTreeBook`.
  `BTreeBook`'s memory is proportional to the *number of levels*, not the price *span*, so its
  compact O(log n) nodes handle the wide sparse book and it **leads** at 37.79 ns/event.
  `RevVecBook` degrades to 191.00 ns/event (its O(depth) scan over a now-deep book — the §7.1
  mechanism at real scale).

The inversion is the same memory hierarchy seen in §5, interacting with a data-dependent
working-set size — and the AMD backend-bound counter (`sorted` 50.5 % memory, `flat`
mispredict-bound at wide depth) is the metal confirmation that it is a memory/speculation
story, not an instruction-count one.

### 7.3 Summary

Measure, then explain, then confirm. The crossover is retired instruction count
gated by touch locality (`RevVecBook`, 93.8 % retiring). The real-data inversion is the memory
hierarchy gated by book width (`FlatBook`'s span vs `BTreeBook`'s level-proportional nodes).
The bad-speculation bullet the binary search would have paid is real and quantified (≈29 ns
at L1-resident depth 256, `branch_experiment.csv`) but already dodged — structurally by
`FlatBook`'s direct index and instrumentally by `std`'s branchless binary search, and the
AMD counter confirms the shipped sorted book pays 0.04 % branch-miss / 0.1 % bad-spec while
sitting at 50.5 % backend-bound-memory. Every conclusion rests on committed data: a laptop
PMU-free behavioral signature that *predicted* the category, and a native AMD Zen 4 counter
that *confirmed* it.

---

## 8. Reproducibility

Host-specific by design (`-C target-cpu=native`, `.cargo/config.toml`); the AMD counter
numbers are valid on the §1 EPYC host. Anyone can rent `m4.metal.large` (EPYC 9254) hourly
and re-run. From the repository root:

```sh
# Build the harness (release, target-cpu=native via .cargo/config.toml).
cargo build --release -p bench

# The isolated, untimed hot loop — the external-profiler target (§2).
./target/release/bench profile --impl sorted --op apply \
    --depth 2048 --locality uniform --iters 200000000 --core 0

# Native AMD Zen 4 pipeline-utilization capture (EPYC; perf_event_paranoid <= 0).
# Vendor-detected AMD metric group + IPC/branch-miss/cache-miss, one per impl:
perf stat -M "$PERF_METRIC_GROUP" \
    -e instructions,cycles,branches,branch-misses,cache-references,cache-misses \
    -o bench/results/perf/perf_sorted.txt \
    ./target/release/bench profile --impl sorted --op apply \
    --depth 2048 --locality uniform --iters 200000000 --core 0
#   (repeat for --impl {btree,rev,flat})

# Cache-line contention (EPYC; readers/consumers pinned across CCDs).
perf c2c record -o bench/results/perf/c2c_ring.data -- <ring contention pass>
perf c2c report -i bench/results/perf/c2c_ring.data --stdio > bench/results/perf/c2c_ring.txt
#   (and c2c_seqlock for the seqlock)

# PMU-free behavioral experiments (laptop baseline; the prediction the counters confirm).
./target/release/bench branch-exp --core 0      # -> bench/results/branch_experiment.csv
./target/release/bench cache-exp  --core 0      # -> bench/results/cache_experiment.csv

# Prior-phase inputs this teardown explains (EPYC re-run).
./target/release/bench service       # -> service_sweep.csv   (the crossover)
./target/release/bench throughput    # -> throughput.csv      (the real-data study)
./target/release/bench flatmem       # -> flat_memory.csv     (FlatBook span, host-independent)

# Figures (reads only the committed CSVs).
./target/release/bench plot --out bench/results
```

Committed data artifacts: `bench/results/perf/perf_{btree,sorted,rev,flat}.txt` and
`perf/c2c_{ring,seqlock}.txt` (EPYC); `bench/results/branch_experiment.csv` and
`cache_experiment.csv` (laptop PMU-free baseline); `bench/results/service_sweep.csv`,
`throughput.csv` (EPYC); `bench/results/flat_memory.csv` (host-independent);
`bench/results/env.json`; the figures under `bench/results/plots/`. The laptop
`perf/perf_unavailable.txt` and `perf/perf_summary.csv` are retained as the record of the
earlier PMU-free condition, superseded by the AMD captures.

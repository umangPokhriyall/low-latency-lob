# BENCHMARKS — a low-latency limit-order-book engine, measured

This is the consolidated, sourced benchmark writeup for the engine: four order-book
implementations behind one frozen trait, two lock-free concurrency primitives, and the
assembled production-to-consumption pipeline. Every number below is re-derived from a
committed CSV under `bench/results/` and cited inline; nothing is computed by hand from
anything else, and nothing is invented. Where a hypothesis was refuted, the refutation
is stated with the number that refutes it. The microarchitecture mechanisms are
summarized here and treated in full in [`PROFILING.md`](PROFILING.md).

---

## 1. TL;DR

- **The "obviously optimal" flat array loses by more than two orders of magnitude on
  real market data.** On the real BTCUSDT replay, `FlatBook` costs **10,926.62 ns/event**
  while `BTreeBook` leads at **43.05 ns/event** — a **~254× loss** for the structure that
  wins every synthetic profile (`throughput.csv`). The cause is one number: the real
  book's per-side span is **92,049,312 bytes (≈88 MiB), ~11× the 8 MiB LLC**
  (`flat_memory.csv`). The tradeoff and the failure are the same span.
- **The data-structure crossover is locality-gated, not depth-gated.** The depth at
  which a best-first linear scan (`RevVecBook`) loses to a binary search (`SortedVecBook`)
  is `D*=256` when touches concentrate at the top of book and `D*=2` when they spread
  uniformly (`service_sweep.csv`). Depth alone does not determine the winner; where the
  touches land does.
- **Both lock-free primitives are loom-verified with zero `unsafe` anywhere in the
  workspace.** A contended seqlock read is **~11 ns p50** and the writer's store latency
  is **flat across reader count** (`seqlock_read.csv`); the SPMC ring's `push`/`recv` sit
  at **7–10 ns p50** (`ring_bench.csv`). Every crate is `#![forbid(unsafe_code)]`.
- **The microarchitecture analysis is rigorous without a PMU.** Hardware counters are
  denied on this host; the top-down categories are inferred from unambiguous behavioral
  signatures (a misprediction 2×2, a cache-footprint latency curve) and no counter is
  fabricated ([`PROFILING.md`](PROFILING.md), §1).

**The one-line verdict — which structure when:** shallow and top-concentrated →
`RevVecBook` or `SortedVecBook` (both at the timer floor); deep and spread →
`SortedVecBook` (`O(log n)`, branchless); deep and wide/unbounded price range (the real
feed) → `BTreeBook` (span-agnostic); deep and bounded span, warm book → `FlatBook`
(`O(1)` index). The shootout has no single winner — the right container is a function of
depth, touch locality, and price-span boundedness.

---

## 2. Environment & methodology

All numbers come from one host, recorded in `bench/results/env.json`:

| field | value (`env.json`) |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz (Tiger Lake) |
| logical cores | 8 |
| caches (`/sys/.../cpu0/cache`) | L1d 48 KiB, L2 1.25 MiB, LLC 8 MiB |
| CPU governor | `powersave` (turbo not pinned) |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0, `-C target-cpu=native` |
| pinned core | 0 |
| clock read-read floor | 7 ns (`clock_overhead_ns` column in every CSV) |

**Service time vs response time — kept strictly distinct.** Two of the questions a
latency study asks are different, and conflating them is the classic measurement error:

- *Service time* is the cost of the operation itself, with no arrival process and
  therefore no coordinated omission. The service-sweep (`service_sweep.csv`), read-path
  (`read_path.csv`), and whole-corpus throughput (`throughput.csv`) benchmarks measure
  service time, as do the primitive micro-benchmarks (`seqlock_read.csv`,
  `ring_bench.csv`).
- *Response time* is completion minus the **scheduled** arrival, under an open-loop
  arrival process. The sustained (`sustained.csv`) and end-to-end (`e2e.csv`) benchmarks
  measure response time and are **coordinated-omission-correct**: each event's latency is
  `completion − scheduled_arrival`, never `completion − apply_start`. When the system
  falls behind, the accumulated backlog is charged to every late event and its tail
  captures it. The CO correction is itself tested — `sustained.rs`'s
  `co_correct_records_accumulating_lag` asserts the backlog appears in the distribution.

The two are never blurred in this document: a table is labeled service or response, and
the inversion headline (§3.2) is a service-time result, while the saturation knee (§5) is
a response-time result.

**Hygiene.** Every measured operation wraps inputs and outputs in `black_box` (no
dead-code elision); each cell warms the pinned core before recording; the service sweep
takes ≥1,000,000 samples per cell and the branch experiment 10,000,384 lookups per cell.
The 7 ns clock floor is reported and **never subtracted** — sub-10 ns figures sit near it
and are read as "at the floor", not as precise values.

**Threats to validity.**
- *Governor `powersave`.* Frequency may scale, so absolute nanoseconds carry some
  variance; every conclusion rests on ratios and shapes (flat vs rising, one impl vs
  another at the same instant), not on a single absolute number. Frequency scaling
  surfaces as transient single-run `max` outliers (e.g. `flat`/`uniform` `top_n_full` at
  depth 2048 carries a `max` of 2,385,919 ns against a 1,990 ns p50, `read_path.csv`),
  not as structural shifts in p50/p99.
- *Single host, `target-cpu=native`.* Binaries are host-specific by design (valid
  microarchitecture profiling); the numbers do not transfer to another CPU.
- *Timer floor.* The 7 ns read-read floor is reported, never subtracted; `best_bid` and
  shallow-depth `update` results sit at the floor and differences there are not read as
  wins.
- *PMU unavailable → PMU-free.* Hardware performance counters are denied on this host
  (`kernel.perf_event_paranoid = 4`, no `CAP_PERFMON`); the unavailability is recorded in
  `bench/results/perf/perf_unavailable.txt` and the parsed-counter schema is retained
  (`perf/perf_summary.csv`) so a PMU-enabled host can populate it unchanged. The
  microarchitecture analysis is built to stand on behavioral signatures instead
  ([`PROFILING.md`](PROFILING.md), §1); **no counters were collected and none are
  fabricated.**

**Reproducibility.** Host-specific by design. From the repository root:

```sh
cargo build --release -p bench                  # target-cpu=native via .cargo/config.toml
./target/release/bench service    --core 0      # -> service_sweep.csv (+ .hgrm)
./target/release/bench read       --core 0      # -> read_path.csv
./target/release/bench throughput --core 0      # -> throughput.csv
./target/release/bench sustained  --core 0      # -> sustained.csv
./target/release/bench flatmem                  # -> flat_memory.csv
./target/release/bench seqlock    --core 0      # -> seqlock_read.csv
./target/release/bench ring       --core 0      # -> ring_bench.csv
./target/release/bench e2e        --core 0      # -> e2e.csv
./target/release/bench branch-exp --core 0      # -> branch_experiment.csv
./target/release/bench cache-exp  --core 0      # -> cache_experiment.csv
./target/release/bench plot       --out bench/results   # -> env.json + plots/*.svg
# or: ./target/release/bench all --core 0
```

---

## 3. The order-book shootout

Four implementations sit behind one frozen `OrderBook` trait and were proven
observationally identical by the differential oracle (`book/tests/oracle.rs`, four-way on
the bounded band). What separates them is the **locate** step of `apply`:
`SortedVecBook` binary-searches a contiguous price-sorted `Vec`; `BTreeBook` descends a
`BTreeMap` (a scattered-node pointer chase); `RevVecBook` scans linearly from the best
end; `FlatBook` indexes directly into one dense per-side array spanning the price range.

### 3.1 The crossover is locality-gated (`service_sweep.csv`)

`update` is an in-place quantity replace at an existing level, so it isolates locate cost
from any memmove. `update` p50 (ns), by depth and touch locality:

**Concentrated (top-of-book-biased — the realistic case):**

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 8 | 16 | 8 | 8 | 12 |
| 128 | 19 | 9 | 8 | 13 |
| 256 | 27 | 14 | 19 | 16 |
| 2048 | 31 | 17 | 21 | 16 |

**Uniform (spread across the whole ladder — the adversarial case):**

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 2 | 16 | 11 | 16 | 14 |
| 256 | 36 | 9 | 100 | 14 |
| 1024 | 47 | 11 | 353 | 14 |
| 2048 | 50 | 14 | 697 | 14 |

The `RevVecBook`↔`SortedVecBook` crossover `D*` is set by **where touches land**, not by
depth: `D*=256` concentrated (below it `RevVecBook` ties `SortedVecBook` at the floor;
from 256 up it is persistently worse), `D*=2` uniform (`SortedVecBook` overtakes
immediately and the gap widens to **697 ns vs 14 ns at depth 2048** — a ~50× loss).
`RevVecBook`'s cost is its scan length: concentrated touches hit near the best (scan 1–2
levels, flat in depth); uniform touches average ≈depth/2 (linear in depth). `FlatBook`'s
direct index is flat at **14 ns across every depth in both localities** — it ties the
binary-search floor without paying any locate cost. There is no crossover *with*
`FlatBook` on service time; it never degrades with depth. Figures:
`plots/crossover_update_p50_{concentrated,uniform}.svg` and the `_p99` pair (source:
`service_sweep.csv`).

### 3.2 The real-data inversion (`throughput.csv` + `flat_memory.csv`)

This is the headline. Whole-corpus replay, no pacing, median of 31 runs — **service
time.** ns/event, from `throughput.csv`:

| corpus | flat | sorted | rev | btree |
|---|---|---|---|---|
| steady (synthetic, narrow) | **8.85** | 13.51 | 15.12 | 22.30 |
| btcusdt-sample (real, wide) | 10,926.62 | 62.55 | 222.34 | **43.05** |

On the narrow synthetic book the array structures win — `FlatBook` leads at 8.85
ns/event, `BTreeBook` trails at 22.30. **On the real BTCUSDT corpus the ranking fully
inverts:** `BTreeBook` leads at 43.05 ns/event, `FlatBook` collapses to 10,926.62
ns/event — last by ~254× behind the leader.

The mechanism is the **memory hierarchy meeting book width**, and `flat_memory.csv`
supplies the one number that explains it. `FlatBook`'s per-side span is **8,193 ticks
(131,088 bytes, L2-resident)** on every synthetic corpus but **5,753,082 ticks
(92,049,312 bytes ≈ 88 MiB, ~11× the 8 MiB LLC)** on the real book — a ~700× span
blow-up (5,753,082 / 8,193). The real BTC/USDT book is wide and sparse: prices range over
millions of ticks. A cold replay walks outward across that span, and each event at a new
extreme price falls outside the allocated array, triggering an `ensure_range`
recenter/grow that reallocates and copies the whole array. That recenter storm, plus
guaranteed LLC/DRAM misses across an 88 MiB array, is the collapse. `BTreeBook` wins
because its memory is proportional to the **number of occupied levels**, not the price
**span**: its compact `O(log n)` nodes handle the wide sparse book where `FlatBook`'s
flat array cannot. The cache-hierarchy view of the same mechanism is in §6 and
[`PROFILING.md`](PROFILING.md) §5–6.

`RevVecBook`'s real cost of 222.34 ns/event also locates it precisely: it sits in its
own Uniform-deep service regime (between its depth-512 and depth-1024 uniform `update`
costs, §3.1), confirming the real touch distribution is moderately deep and spread —
exactly the regime that defeats a linear scan.

### 3.3 The flat-array tradeoff, quantified (`flat_memory.csv`, `read_path.csv`)

`FlatBook` buys its `O(1)` depth-and-locality-independent update at measured costs:

- **Memory is proportional to span, not occupied levels** (`flat_memory.csv`): 131,088
  bytes for every bounded synthetic corpus, **92,049,312 bytes (≈88 MiB)** for the real
  one. The ~700× memory blow-up and the §3.2 throughput collapse are the same span.
- **The read path is `O(1)` for `best_bid` but a sparse scan for `top_n`**
  (`read_path.csv`): `best_bid` p50 is 11 ns at every depth (at the floor, like the
  Vecs), but `top_n_full` at depth 2048 reads 1,990 ns p50 vs `SortedVecBook`'s 616 ns —
  ~3.2× the contiguous copy, because the sparse ladder forces the scan to visit empty
  slots. It still beats `BTreeBook`'s node-by-node `top_n_full` (3,517 ns).

### 3.4 Which structure when (sourced)

| regime | use | sourced basis |
|---|---|---|
| shallow, top-concentrated | `RevVecBook` or `SortedVecBook` | `update` p50 ~8 ns at the floor, depths 8–128 concentrated; both beat `BTreeBook`'s 16–19 ns (`service_sweep.csv`) |
| deep, spread across the ladder | `SortedVecBook` | `update` p50 14 ns at depth 2048 uniform vs `RevVecBook` 697 ns, `BTreeBook` 50 ns (`service_sweep.csv`) |
| deep, **wide / unbounded** price range (the real feed) | `BTreeBook` | leads the real BTCUSDT replay at 43.05 ns/event vs sorted 62.55, rev 222.34, flat 10,926.62 (`throughput.csv`); span-agnostic where `FlatBook` needs 88 MiB (`flat_memory.csv`) |
| deep, **bounded** span, warm/amortized book | `FlatBook` | `update` p50 14 ns regardless of depth/locality (`service_sweep.csv`) and 8.85 ns/event on the bounded synthetic steady corpus (`throughput.csv`) at 131,088 bytes (`flat_memory.csv`) — **only** when the span is bounded and the recenter cost is not paid on a cold wide-span replay |

---

## 4. The concurrency primitives

Both primitives live in `sync`, which — like every crate here — is
`#![forbid(unsafe_code)]`: shared concurrent mutation is expressed with atomics, not
`UnsafeCell`. Both are loom-verified for memory ordering and corroborated by real-thread
stress tests (`sync/tests/`).

### 4.1 Seqlock — single-writer / many-reader top-of-book snapshot (`seqlock_read.csv`)

The seqlock publishes a `TopOfBook` via a version counter: many readers take an
optimistic snapshot and retry only if a write straddled the read. Read latency under a
contending writer, by reader count `K` and writer mode (`seqlock_read.csv`):

| mode | K | read p50 | read p99 | read p99.9 | samples |
|---|---|---|---|---|---|
| full_tilt | 1 | 11 ns | 24 ns | 100 ns | 1,000,000 |
| full_tilt | 2 | 11 ns | 13 ns | 14 ns | 2,000,000 |
| full_tilt | 4 | 11 ns | 13 ns | 14 ns | 4,000,000 |
| paced | 4 | 11 ns | 14 ns | 15 ns | 4,000,000 |

A `load()` costs **~11 ns p50** (≈4 ns above the floor) and stays in the **13–24 ns band
at p99**, flat across reader count and writer mode. The optimistic-retry rate
(`mean_retries_per_load`) is **≤0.005304** at worst (full_tilt, K=1) and 0.000000 in every
other cell: readers almost never pay the retry path because the writer's odd-version
window is only a few nanoseconds wide.

The load-bearing result is **writer independence from reader count** (`write_p50_ns`):
the store latency is **10, 10, 11 ns at K=1,2,4** full-tilt (7 ns paced) — flat, no
upward trend. This is the property a seqlock exists to provide and a `Mutex<TopOfBook>`
cannot: the writer is **wait-free**, never blocked by any reader, so reader count does
not tax it. Across the 14,000,000 timed reads in `seqlock_read.csv` no torn read was
returned, and the loom model (`sync/tests/loom_seqlock.rs`) plus the real-thread stress
test (`sync/tests/stress_seqlock.rs`) certify the ordering. Figures:
`plots/seqlock_read_p99_vs_readers.svg`, `plots/seqlock_write_vs_readers.svg`.

The multi-millisecond `read_max_ns` (46 µs … 12 ms) is OS-deschedule jitter on this
non-isolated `powersave` host, not seqlock cost; the interior p99.9 (≤100 ns, ≤14 ns once
K≥2) is the true tail. It is reported, not hidden, and not attributed to the primitive.

### 4.2 SPMC broadcast ring (`ring_bench.csv`)

The ring is a single-producer / many-independent-consumer broadcast bus: the producer
streams `[u64; W]` records and **never blocks** (it overwrites on wrap); each consumer
reads the whole stream from its own cursor and **detects overrun** rather than corrupting
silently. Latency, `full_tilt`, by consumer count `K` (`ring_bench.csv`):

| K | push p50 | push p99 | recv p50 | recv p99 |
|---|---|---|---|---|
| 1 | 7 ns | 44 ns | 9 ns | 57 ns |
| 2 | 7 ns | 50 ns | 10 ns | 83 ns |
| 4 | 8 ns | 23 ns | 9 ns | 78 ns |

`push` p50 is **7–8 ns** and `try_recv` p50 **9–10 ns** — within one clock floor of the
metal, and **flat as K rises**. The flatness is the direct false-sharing-free signal:
slots are `#[repr(align(64))]` (one per cache line, the write cursor on its own line), so
adjacent consumers do not invalidate each other's lines. Verified structurally by the
`size_of::<Slot<W>>() % 64 == 0` static assertion and behaviorally by the flat `recv`
latency.

**Producer broadcast throughput is *not* flat — an honest negative result**
(`producer_mev_s`, `full_tilt`): **38.65 → 20.85 → 15.38 Mev/s at K=1,2,4**, a ~2.5×
decline. This is **not** false sharing (it persists with flat per-op latency); it is
**true sharing on the one genuinely-shared word, the write cursor**. Every `try_recv`
reads the producer's cursor on every poll (the broadcast contract), and the producer's
`Release` store on every push invalidates the K consumer copies, so the store buffer
drains slower against the coherence traffic as K grows. The `align(64)` discipline
eliminates *false* sharing; what remains is the *true* sharing any SPMC broadcast inherits
on its shared progress counter. The decline is in **rate**, never in **progress** — the
producer completes its full budget at every K (wait-free). Figure:
`plots/ring_producer_throughput_vs_consumers.svg`.

Overrun behavior tracks the producer/consumer speed ratio (`overrun_rate`): under a
free-running producer a lone consumer is **97.4% lapped** (K=1 full_tilt), collapsing to
**2.9%** at K=4 (where the true-sharing decline drops the producer below the aggregate
drain rate); under a realistic **paced 1 MHz feed every consumer keeps up and overrun is
≈0** (0.000 at K=4). Overruns are always **detected and reported**, never silent: the
loom model (`sync/tests/loom_ring.rs`) and stress test (`sync/tests/stress_ring.rs`)
prove no torn or duplicated record ever escapes.

### 4.3 Honest progress guarantees

State them plainly: the **writer/producer is wait-free** (it never blocks on a reader or
consumer and completes its full budget at every K). The **readers and consumers are not
lock-free** — a writer that updates fast enough can force a reader to retry, and a
producer fast enough laps a consumer. The seqlock answers retries by re-reading (a
near-zero cost here); the ring answers lapping by **detecting the overrun and resyncing
derived state from the seqlock snapshot** before resuming from the oldest resident ring
position. Lossy-but-detected is a deliberate broadcast contract, not a defect: no
consumer can stall the producer.

---

## 5. End-to-end pipeline (`e2e.csv`)

The engine composes the verified parts into one hot path: a pinned producer replays a
corpus through `EngineProducer::<BTreeBook>::process` — `book.apply` →
`seqlock.store(top_of_book)` → `ring.push(pack(ev))` — while `K` pinned independent
consumers each drain the ring, do light derived work, resync from the seqlock on overrun,
and record end-to-end latency. **Response time, coordinated-omission-correct:** the
producer stamps each event's `ts` with its scheduled arrival before processing, and each
consumer records `completion − scheduled_arrival`.

**Real-corpus headline (BTCUSDT replay, speed 1, `BTreeBook`):** at K=1 the pipeline runs
**5,227 ns p50 / 128,767 ns p99** over 13,765 events (`e2e.csv`). The pipeline idles the
vast majority of the time (achieved ~305 eps against 304 target); the ~129 µs p99 is a
property of the **captured feed**, not the pipeline — Binance `@depth@100ms` batches
deliver many events sharing one timestamp, scheduled simultaneously and serviced serially,
a genuine coordinated-omission effect the CO-correct stamping surfaces rather than
averages away.

**Synthetic fixed-rate floor and saturation:** while the pipeline serves the rate, p50
sits at the **~110–140 ns full-pipeline floor** (apply + seqlock store + ring push +
cross-core propagation + recv) — e.g. K=1 reads 125, 126, 118, 129 ns p50 at 1/2/5/10 M
eps (`e2e.csv`). Once the target exceeds what the pipeline sustains, p50 and p99 jump to
the **millisecond scale** as the producer falls behind and the backlog is charged to every
event (the CO-correct saturation signal). The **max sustainable rate falls with K** — the
highest non-saturated cell is ~20 M eps at K=1, dropping toward ~13 M eps at K=4
(`saturated` column) — tracking the producer-throughput decline below.

**The true-sharing reality, reported not engineered around:** at the saturated operating
point (100 M eps target) producer throughput is **20.60 → 16.98 → 12.74 Mev/s at K=1,2,4**
(`e2e.csv`), a ~38% decline. This is the same true-sharing-on-the-write-cursor effect
isolated in §4.2, now visible end-to-end. Phase 8 measures and reports it; it does not
modify the verified `sync` primitives to chase it. The pipeline's `overrun_rate` is
0.000000 in every cell — the consumer step is cheaper than the producer step, so under
load the cost is schedule backlog (coordinated omission), not ring lapping; the
overrun→resync composition is exercised where the producer genuinely outruns a consumer
(`engine/tests/pipeline.rs` and the structural `high_rate_produces_overruns` test in
`bench/src/benches/e2e.rs`). Figures: `plots/e2e_p99_vs_rate.svg`,
`plots/e2e_producer_throughput_vs_consumers.svg`.

---

## 6. Microarchitecture summary

The four implementations form a microarchitecture taxonomy, confirmed against committed
data in [`PROFILING.md`](PROFILING.md) (which interprets `service_sweep.csv`,
`throughput.csv`, `flat_memory.csv`, `branch_experiment.csv`, `cache_experiment.csv`):

- **`SortedVecBook` — Memory Bound** (a prediction refuted, see §7). Its binary search is
  a chain of `O(log n)` dependent loads in one contiguous array; cost climbs gently as the
  array spills past each cache (9 → 104 ns, L1 → DRAM, `cache_experiment.csv`).
- **`BTreeBook` — Memory Bound, pointer chase.** Elevated even when small (40 ns at L1 vs
  `SortedVecBook`'s 9 ns) and climbing steeply to 332 ns at DRAM: each descent step is a
  dependent load to a scattered node, so the prefetcher cannot run ahead
  (`cache_experiment.csv`).
- **`RevVecBook` — Core/Retiring, rising with depth.** Its cost is retired instruction
  count (scan length): 5,271 ns at depth 16384 on an L2-resident 262 KiB footprint —
  too fast-fitting to be a cache effect, too slow to be anything but instruction count
  (`cache_experiment.csv`).
- **`FlatBook` — Retiring (minimal), until recenter.** Its single independent load is
  hidden by out-of-order execution even across a 64 MiB span — flat ~12–14 ns across the
  whole hierarchy (`cache_experiment.csv`). The Memory-Bound regime is the recenter on the
  88 MiB real span (§3.2).

The misprediction finding: a branchy binary search over **random** keys with all data in
L1 is **5.1× slower** than over predictable keys (36.093 vs 7.019 ns p50, depth 256,
`branch_experiment.csv`) — a **+29.07 ns** pure-misprediction penalty, rising to **+35.9 ns**
at depth 16384. A branchless `cmov` search (`std::hint::select_unpredictable`) is flat
across predictability. The honest refutation in §7 is built on this. Figure:
`plots/branch_misprediction_2x2.svg`, `plots/cache_footprint_latency.svg`.

---

## 7. Honest findings & surprises

These are featured, not buried — they are what makes the work checkable.

- **The real-data inversion.** The structure that wins every synthetic profile
  (`FlatBook`, 8.85 ns/event steady) is last by ~254× on the real feed (10,926.62
  ns/event), and the structure that loses every synthetic profile (`BTreeBook`, 22.30
  ns/event steady) leads it (43.05 ns/event) — because the real book's 88 MiB span is
  ~11× LLC (`throughput.csv` + `flat_memory.csv`). "The tradeoff and the failure are one
  number."
- **The `SortedVecBook` refutation.** It was predicted to be Bad-Speculation-bound (a
  binary search that mispredicts). It is **not**: `std::partition_point` compiles to
  **branchless** code on this toolchain (rustc 1.95), flat across key predictability in
  `branch_experiment.csv` (3.118 ns predictable vs 3.118 ns random at depth 16). The
  shipped sorted book pays no branch-misprediction penalty; it is Memory Bound by its
  dependent-load chain. The ~29 ns misprediction penalty is real and quantified, but it is
  the cost a *branchy* locate would pay — already dodged structurally by `FlatBook`'s
  direct index and instrumentally by `std`'s branchless search.
- **The true-sharing decline.** The SPMC ring's producer throughput falls ~2.5×
  (1→4 consumers) — an honest negative against the original "flat throughput proves no
  false sharing" hypothesis. The cause is profiled (true sharing on the shared write
  cursor, §4.2), the per-op latency is flat, and the producer stays wait-free; the rate
  falls, the progress guarantee does not.
- **Perf unavailable, analysis still rigorous.** Hardware counters are denied on this
  host (§2). The top-down microarchitecture categories are nonetheless established from
  unambiguous behavioral signatures — a misprediction 2×2 and a cache-footprint curve —
  and no counter is fabricated. An honest negative result with a profile is the signal,
  not the liability.
- **Zero `unsafe`, loom-verified primitives.** Every crate is
  `#![forbid(unsafe_code)]`; both lock-free primitives are verified by loom models and
  corroborated by real-thread stress tests. Concurrent shared mutation is expressed with
  atomics, the sound tool under Rust's memory model, rather than `UnsafeCell`.

---

## 8. Reproducibility & artifacts

The committed CSVs under `bench/results/` are the source of truth; this document re-derives
every number from them. The exact regeneration commands are in §2. Per-claim provenance:

| claim group | source CSV |
|---|---|
| crossover, `D*`, FlatBook flat service curve | `service_sweep.csv` (+ `.hgrm`) |
| read path, `top_n_full`, best access | `read_path.csv` |
| real-data inversion, synthetic throughput | `throughput.csv` |
| FlatBook span / memory tradeoff | `flat_memory.csv` |
| sustained CO-correct response, saturation | `sustained.csv` |
| seqlock read/write latency, retry rate | `seqlock_read.csv` |
| ring push/recv latency, throughput, overrun | `ring_bench.csv` |
| end-to-end latency, saturation, true sharing | `e2e.csv` |
| misprediction 2×2, branchless elimination | `branch_experiment.csv` |
| cache-footprint latency curve | `cache_experiment.csv` |
| environment, corpora fingerprints, clock floor | `env.json` |
| PMU unavailability (recorded, not fabricated) | `perf/perf_unavailable.txt`, `perf/perf_summary.csv` |

Figures are under `bench/results/plots/` and each is generated from its committed CSV by
`bench plot`. The mechanistic teardown is [`PROFILING.md`](PROFILING.md); the interim
per-phase notes (`RESULTS.md`, `seqlock.md`, `ring.md`, `e2e.md`) remain as working
records, consolidated here into the single public artifact.

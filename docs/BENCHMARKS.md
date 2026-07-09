# BENCHMARKS — a low-latency limit-order-book engine, measured

This is the consolidated, sourced benchmark writeup for the engine: four order-book
implementations behind one frozen trait, two lock-free concurrency primitives, and the
assembled production-to-consumption pipeline. Every number below is re-derived from a
committed CSV under `bench/results/` and cited inline; nothing is computed by hand from
anything else, and nothing is invented. Where a hypothesis was refuted, the refutation
is stated with the number that refutes it. The microarchitecture mechanisms are
summarized here and treated in full — now with native AMD Zen4 hardware counters — in
[`PROFILING.md`](PROFILING.md).

**Provenance note (read once, then trust the citations).** The hardware-fidelity-dependent
measurements were re-run on a single rented AMD EPYC bare-metal box (below), and every
per-op latency, throughput, and primitive number here is that EPYC dataset:
`service_sweep.csv`, `read_path.csv`, `throughput.csv`, `sustained.csv`,
`seqlock_read.csv`, `ring_bench.csv`, the AMD pipeline captures `perf/perf_*.txt`, and the
cache-line-contention reports `perf/c2c_*.txt`. Two things are *not* host-dependent and are
carried unchanged: `flat_memory.csv` records the book's price-**span** in ticks/bytes (a
property of the corpus, identical on any host). Three timing datasets were **outside the
metal re-run scope** (Phases 4/6/7/9) and remain the earlier laptop (Intel i5-1135G7)
baseline — the two PMU-free behavioral experiments `branch_experiment.csv` and
`cache_experiment.csv`, and the Phase-8 end-to-end pipeline `e2e.csv`; each is labeled as
such at the point of use. The laptop baseline is referenced as historical context, never
mixed into an EPYC claim.

---

## 1. TL;DR

- **The "obviously optimal" flat array loses by nearly two-and-a-half orders of magnitude
  on real market data.** On the real BTCUSDT replay, `FlatBook` costs **10,896.43 ns/event**
  while `BTreeBook` leads at **37.79 ns/event** — a **~288× loss** for the structure that
  wins every synthetic profile (`throughput.csv`). The cause is one number: the real
  book's per-side span is **92,049,312 bytes (≈88 MiB)**, **~2.74× the 32 MiB per-CCD L3**
  (`flat_memory.csv`; the EPYC's per-CCD L3 is 32 MiB, so the inversion survives a cache
  4× larger than the laptop's 8 MiB). The tradeoff and the failure are the same span.
- **The data-structure crossover is locality-gated, not depth-gated.** Under uniform
  touches a best-first linear scan (`RevVecBook`) pulls clear of the binary search
  (`SortedVecBook`) by depth ≈64 and degrades to **519 ns vs 29 ns at depth 2048 (~18×)**;
  under top-concentrated touches the same scan **never loses within the swept range** — it
  holds 9–29 ns through depth 2048, at/near the ~10 ns clock floor (`service_sweep.csv`).
  Depth alone does not determine the winner; where the touches land does.
- **Both lock-free primitives are loom-verified with zero `unsafe` anywhere in the
  workspace.** A contended seqlock read is **~10 ns p50** and the writer's store latency
  is **flat across reader count** (`seqlock_read.csv`); the SPMC ring's `push`/`recv` sit
  at **~10 ns p50** (`ring_bench.csv`). Every crate is `#![forbid(unsafe_code)]`.
- **The microarchitecture analysis is now confirmed by native AMD Zen4 counters.** On the
  laptop, hardware counters were denied and the top-down categories were *predicted* from
  behavioral signatures. On the EPYC box (`perf_event_paranoid = -1`) the AMD Zen4
  pipeline-utilization counters make the prediction hardware-fact: `SortedVecBook` is
  **50.5 % backend-bound (memory) with 0.1 % bad-speculation** — memory-bound, not
  speculation-bound (`perf/perf_sorted.txt`), exactly as the PMU-free method predicted.
  This is the AMD architectural counterpart to Intel TMA, not Intel TMA relabeled
  ([`PROFILING.md`](PROFILING.md)).

**The one-line verdict — which structure when:** shallow and top-concentrated →
`RevVecBook` or `SortedVecBook` (both at the timer floor); deep and spread →
`SortedVecBook` (`O(log n)`, branchless); deep and wide/unbounded price range (the real
feed) → `BTreeBook` (span-agnostic); deep and bounded span, warm book → `FlatBook`
(`O(1)` index). The shootout has no single winner — the right container is a function of
depth, touch locality, and price-span boundedness.

---

## 2. Environment & methodology

The hardware-fidelity-dependent numbers come from one rented bare-metal host, recorded in
`bench/results/env.json`:

| field | value |
|---|---|
| provider / SKU | Latitude.sh `m4.metal.large`, single socket, Ashburn (US-East) |
| CPU | AMD EPYC 9254, 24 physical cores / 48 threads @ 2.9 GHz (Zen 4, Genoa) (`env.json`) |
| chiplet topology | 4 CCDs, each with a **private 32 MiB L3** → **128 MiB aggregate L3**; 64 B cache line |
| NUMA | booted **NPS1** → `numactl --hardware` reports **1 NUMA node**; per-CCD L3 isolation holds regardless of NPS |
| RAM | 384 GiB |
| CPU governor | **`performance`** (amd-pstate) (`env.json`) |
| OS / kernel | Ubuntu 24.04 LTS, kernel **6.8.0-124-generic** (`env.json`) |
| rustc | **1.96.1**, `-C target-cpu=native` (`env.json`) |
| pinning | LOB producer pinned to **one dedicated CCD-0 core** (`pinned_core: 0`); Phase 6/7 readers/consumers spread **across CCDs** to surface cross-CCD coherence traffic |
| clock read-read floor | **~10 ns** (`clock_overhead_ns`; 9–10 ns across the metal CSVs) |

**Reproducibility as a first-class claim.** This host is not a bespoke lab machine: anyone
can rent this exact SKU (`m4.metal.large`, EPYC 9254) hourly from Latitude.sh and re-run
the suite in a handful of dollars. That is a stronger reproducibility guarantee than a
whole-host cloud metal instance nobody can afford to replicate. The per-CCD private L3 is
the load-bearing topology fact: pinning the producer to one CCD-0 core gives it a private
32 MiB L3 and its own execution ports, and spreading the Phase 6/7 readers across the other
CCDs maximizes the cross-CCD coherence traffic that `perf c2c` (§4.2) is there to observe.
The single-socket, intra-socket-Infinity-Fabric caveat is the only residual (§7).

**Service time vs response time — kept strictly distinct.** Two of the questions a
latency study asks are different, and conflating them is the classic measurement error:

- *Service time* is the cost of the operation itself, with no arrival process and
  therefore no coordinated omission. The service-sweep (`service_sweep.csv`), read-path
  (`read_path.csv`), and whole-corpus throughput (`throughput.csv`) benchmarks measure
  service time, as do the primitive micro-benchmarks (`seqlock_read.csv`,
  `ring_bench.csv`).
- *Response time* is completion minus the **scheduled** arrival, under an open-loop
  arrival process. The sustained (`sustained.csv`) and end-to-end (`e2e.csv`, laptop
  baseline) benchmarks measure response time and are **coordinated-omission-correct**: each
  event's latency is `completion − scheduled_arrival`, never `completion − apply_start`.
  When the system falls behind, the accumulated backlog is charged to every late event and
  its tail captures it. The CO correction is itself tested — `sustained.rs`'s
  `co_correct_records_accumulating_lag` asserts the backlog appears in the distribution.

The two are never blurred in this document: a table is labeled service or response, and
the inversion headline (§3.2) is a service-time result, while the saturation knee (§5) is
a response-time result.

**Hygiene.** Every measured operation wraps inputs and outputs in `black_box` (no
dead-code elision); each cell warms the pinned core before recording; the service sweep
takes ≥1,000,000 samples per cell. The ~10 ns clock floor is reported and **never
subtracted** — sub-10 ns figures sit at it and are read as "at the floor", not as precise
values. On this box p50s land on a coarse ~10 ns grid (9 / 19 / 29 … ns), so differences
under ~10 ns between cells are read as ties at the floor, not as wins.

**Threats to validity — the resolved confounds and the one that remains.**
- *Governor now pinned to `performance` on an isolated CCD → jitter-free tails.* The
  laptop ran `powersave` on a shared, non-isolated host, and its tails carried scheduler
  jitter. On the EPYC box the producer owns a dedicated CCD-0 core at a fixed frequency, so
  the interior p99/p99.9 are clean; residual multi-ms `max` values (e.g. `seqlock_read.csv`
  `read_max_ns` up to ~6 ms) are isolated OS-deschedule events, reported and not
  attributed to the primitive.
- *PMU now available → native AMD counters.* The laptop denied hardware counters
  (`perf_event_paranoid = 4`); the microarchitecture story stood PMU-free. On the EPYC box
  (`perf_event_paranoid = -1`) the native AMD Zen4 pipeline-utilization counters were
  captured (`perf/perf_*.txt`) and **confirm** the PMU-free predictions
  ([`PROFILING.md`](PROFILING.md)). The stale laptop artifacts
  (`perf/perf_unavailable.txt`, `perf/perf_summary.csv`) are retained only as the record of
  that earlier condition.
- *Inferred cache-line sharing now directly observed → `perf c2c` HITM.* On the laptop the
  false-vs-true-sharing distinction was inferred from the `align(64)` layout and the
  throughput-decline curve. On the EPYC box `perf c2c` directly observes cross-CCD
  hit-modified (HITM) transfers (`perf/c2c_ring.txt`, `perf/c2c_seqlock.txt`), upgrading the
  attribution from inferred to measured (§4.2).
- *Single host, `target-cpu=native`.* Binaries are host-specific by design (valid
  microarchitecture profiling); the numbers do not transfer to another CPU. Absolute
  nanoseconds are not comparable to the archived laptop baseline — the EPYC 9254 @ 2.9 GHz
  is a different machine; the qualitative verdicts (which structure when, writer wait-free,
  zero `unsafe`, CO-correct method) are properties of the code and method, not the host.
- *The single residual caveat:* one socket, loopback. The Phase 6/7 cross-core traffic is
  **intra-socket, cross-CCD over Infinity Fabric**, not inter-socket. This is strictly more
  local than a two-socket box, and orthogonal to every pipeline bucket — but it is the one
  topology limitation to state plainly.

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
from any memmove. `update` p50 (ns), by depth and touch locality (EPYC; values on the
~10 ns grid):

**Concentrated (top-of-book-biased — the realistic case):**

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 8 | 9 | 9 | 9 | 9 |
| 128 | 19 | 19 | 9 | 9 |
| 256 | 19 | 19 | 9 | 9 |
| 2048 | 29 | 39 | 29 | 19 |

**Uniform (spread across the whole ladder — the adversarial case):**

| depth | btree | sorted | rev | flat |
|---|---|---|---|---|
| 2 | 9 | 9 | 9 | 9 |
| 256 | 39 | 19 | 79 | 9 |
| 1024 | 39 | 29 | 259 | 9 |
| 2048 | 49 | 29 | 519 | 9 |

The crossover between the linear scan and the binary search is set by **where touches
land**, not by depth. Under **uniform** touches `RevVecBook` pulls clear of the flat
`SortedVecBook` line by depth ≈64 (rev 29 ns vs sorted 19 ns; both are floor-tied below
that) and the gap widens to **519 ns vs 29 ns at depth 2048 — a ~18× loss**. Under
**concentrated** touches `RevVecBook` *never loses within the swept range*: it holds 9–29 ns
through depth 2048, staying at or below even `SortedVecBook` (which climbs to 39 ns as its
binary-search dependent-load chain lengthens), because concentrated touches scan only the
top 1–2 levels regardless of book depth. `FlatBook`'s direct index is essentially flat —
**9 ns across every depth under uniform touches, 9–19 ns concentrated** — it ties the
binary-search floor without paying any locate cost, and never degrades with depth. Figures:
`plots/crossover_update_p50_{concentrated,uniform}.svg` and the `_p99` pair (source:
`service_sweep.csv`).

### 3.2 The real-data inversion (`throughput.csv` + `flat_memory.csv`)

This is the headline. Whole-corpus replay, no pacing, median of 31 runs — **service
time.** ns/event, from `throughput.csv`:

| corpus | flat | sorted | rev | btree |
|---|---|---|---|---|
| steady (synthetic, narrow) | **7.46** | 10.44 | 12.45 | 19.47 |
| btcusdt-sample (real, wide) | 10,896.43 | 59.62 | 191.00 | **37.79** |

On the narrow synthetic book the array structures win — `FlatBook` leads at 7.46
ns/event, `BTreeBook` trails at 19.47. **On the real BTCUSDT corpus the ranking fully
inverts:** `BTreeBook` leads at 37.79 ns/event, `FlatBook` collapses to 10,896.43
ns/event — last by **~288×** behind the leader.

The mechanism is the **memory hierarchy meeting book width**, and `flat_memory.csv`
supplies the one number that explains it. `FlatBook`'s per-side span is **8,193 ticks
(131,088 bytes, L2-resident)** on every synthetic corpus but **5,753,082 ticks
(92,049,312 bytes ≈ 88 MiB)** on the real book — a ~702× span blow-up (5,753,082 / 8,193).
The real BTC/USDT book is wide and sparse: prices range over millions of ticks. A cold
replay walks outward across that span, and each event at a new extreme price falls outside
the allocated array, triggering an `ensure_range` recenter/grow that reallocates and copies
the whole array. That recenter storm, plus guaranteed misses across an 88 MiB array, is the
collapse. **The larger cache did not save it:** 88 MiB is still ~2.74× the EPYC's 32 MiB
per-CCD L3, so the inversion held even against a cache 4× the laptop's 8 MiB LLC — the
crossover depth moved outward but the collapse did not disappear, which is itself the
interesting confirmation. `BTreeBook` wins because its memory is proportional to the
**number of occupied levels**, not the price **span**: its compact `O(log n)` nodes handle
the wide sparse book where `FlatBook`'s flat array cannot. The native AMD Zen4 counters make
this mechanistic — `SortedVecBook` 50.5 % backend-bound (memory), `FlatBook`
mispredict-bound at wide depth — in §6 and [`PROFILING.md`](PROFILING.md).

`RevVecBook`'s real cost of 191.00 ns/event also locates it precisely: it sits in its own
Uniform-deep service regime (between its depth-512 and depth-1024 uniform `update` costs,
§3.1), confirming the real touch distribution is moderately deep and spread — exactly the
regime that defeats a linear scan.

### 3.3 The flat-array tradeoff, quantified (`flat_memory.csv`, `read_path.csv`)

`FlatBook` buys its `O(1)` depth-and-locality-independent update at measured costs:

- **Memory is proportional to span, not occupied levels** (`flat_memory.csv`, a
  host-independent property of the corpus): 131,088 bytes for every bounded synthetic
  corpus, **92,049,312 bytes (≈88 MiB)** for the real one. The ~702× memory blow-up and the
  §3.2 throughput collapse are the same span.
- **The read path is `O(1)` for `best_bid` but a sparse scan for `top_n`**
  (`read_path.csv`, EPYC): `best_bid` p50 is **9 ns at every depth and every impl** (at the
  floor), but `top_n_full` at depth 2048 reads **2,459 ns p50** for `FlatBook` vs
  `SortedVecBook`'s **529 ns** — ~4.6× the contiguous copy, because the sparse ladder forces
  the scan to visit empty slots. It still edges `BTreeBook`'s node-by-node `top_n_full`
  (2,649 ns).

### 3.4 Which structure when (sourced)

| regime | use | sourced basis (EPYC) |
|---|---|---|
| shallow, top-concentrated | `RevVecBook` or `SortedVecBook` | `update` p50 ~9 ns at the floor, depths 8–256 concentrated; both at/below `BTreeBook`'s 19 ns (`service_sweep.csv`) |
| deep, spread across the ladder | `SortedVecBook` | `update` p50 29 ns at depth 2048 uniform vs `RevVecBook` 519 ns, `BTreeBook` 49 ns (`service_sweep.csv`) |
| deep, **wide / unbounded** price range (the real feed) | `BTreeBook` | leads the real BTCUSDT replay at 37.79 ns/event vs sorted 59.62, rev 191.00, flat 10,896.43 (`throughput.csv`); span-agnostic where `FlatBook` needs 88 MiB (`flat_memory.csv`) |
| deep, **bounded** span, warm/amortized book | `FlatBook` | `update` p50 ~9 ns regardless of depth/locality (`service_sweep.csv`) and 7.46 ns/event on the bounded synthetic steady corpus (`throughput.csv`) at 131,088 bytes (`flat_memory.csv`) — **only** when the span is bounded and the recenter cost is not paid on a cold wide-span replay |

---

## 4. The concurrency primitives

Both primitives live in `sync`, which — like every crate here — is
`#![forbid(unsafe_code)]`: shared concurrent mutation is expressed with atomics, not
`UnsafeCell`. Both are loom-verified for memory ordering and corroborated by real-thread
stress tests (`sync/tests/`). On the EPYC box the readers/consumers were pinned **across
CCDs** so their coherence traffic crosses Infinity Fabric — the worst realistic case for a
single-socket fleet, and the case `perf c2c` measures.

### 4.1 Seqlock — single-writer / many-reader top-of-book snapshot (`seqlock_read.csv`)

The seqlock publishes a `TopOfBook` via a version counter: many readers take an
optimistic snapshot and retry only if a write straddled the read. Read latency under a
contending writer, by reader count `K` and writer mode (`seqlock_read.csv`, EPYC):

| mode | K | read p50 | read p99 | read p99.9 | samples |
|---|---|---|---|---|---|
| full_tilt | 1 | 10 ns | 10 ns | 10 ns | 1,000,000 |
| full_tilt | 2 | 10 ns | 10 ns | 10 ns | 2,000,000 |
| paced | 1 | 10 ns | 10 ns | 10 ns | 1,000,000 |
| paced | 2 | 10 ns | 10 ns | 10 ns | 2,000,000 |

A `load()` costs **~10 ns p50** (at the clock floor) and stays there through **p99.9** in
every cell, flat across reader count and writer mode. The optimistic-retry rate
(`mean_retries_per_load`) is **≤0.000151** at worst (full_tilt, K=1) and 0.000000 in the
K=2 cells: readers almost never pay the retry path because the writer's odd-version window
is only a few nanoseconds wide.

The load-bearing result is **writer independence from reader count** (`write_p50_ns`):
the store latency is **10 ns at K=1 and K=2** in every mode — flat, no upward trend. This
is the property a seqlock exists to provide and a `Mutex<TopOfBook>` cannot: the writer is
**wait-free**, never blocked by any reader, so reader count does not tax it. Across the
**6,000,000** timed reads in `seqlock_read.csv` no torn read was returned, and the loom
model (`sync/tests/loom_seqlock.rs`) plus the real-thread stress test
(`sync/tests/stress_seqlock.rs`) certify the ordering. Figures:
`plots/seqlock_read_p99_vs_readers.svg`, `plots/seqlock_write_vs_readers.svg`.

The multi-millisecond `read_max_ns` (up to ~6 ms) is OS-deschedule jitter on this
non-isolated-at-the-OS-level host, not seqlock cost; the interior p99.9 (10 ns) is the true
tail. It is reported, not hidden, and not attributed to the primitive.

### 4.2 SPMC broadcast ring (`ring_bench.csv`, `perf/c2c_ring.txt`)

The ring is a single-producer / many-independent-consumer broadcast bus: the producer
streams `[u64; W]` records and **never blocks** (it overwrites on wrap); each consumer
reads the whole stream from its own cursor and **detects overrun** rather than corrupting
silently. Latency, `full_tilt`, by consumer count `K` (`ring_bench.csv`, EPYC):

| K | push p50 | push p99 | recv p50 | recv p99 |
|---|---|---|---|---|
| 1 | 10 ns | 10 ns | 10 ns | 140 ns |
| 2 | 10 ns | 10 ns | 10 ns | 300 ns |

`push` p50 is **10 ns** and `try_recv` p50 **10 ns** — at the clock floor, and **flat as K
rises**. The flatness is the direct false-sharing-free signal: slots are
`#[repr(align(64))]` (one per cache line, the write cursor on its own line), and the EPYC
cache line is **64 B** (`getconf LEVEL1_DCACHE_LINESIZE`), so the alignment rationale carries
over exactly and adjacent consumers do not invalidate each other's lines. Verified
structurally by the `size_of::<Slot<W>>() % 64 == 0` static assertion and behaviorally by
the flat `recv` latency.

**Producer broadcast throughput is *not* flat — an honest negative result**
(`producer_mev_s`, `full_tilt`): **12.17 → 8.46 Mev/s at K=1,2**, a ~1.4× decline over the
one step measured on this box. This is **not** false sharing (it persists with flat per-op
latency); it is **true sharing on the one genuinely-shared word, the write cursor**. Every
`try_recv` reads the producer's cursor on every poll (the broadcast contract), and the
producer's `Release` store on every push invalidates the K consumer copies, so the store
buffer drains slower against the coherence traffic as K grows.

**`perf c2c` upgrades this from inferred to measured** (`perf/c2c_ring.txt`,
`perf/c2c_seqlock.txt`). With the consumers pinned across CCDs, `perf c2c` directly observed
cross-CCD hit-modified transfers — the coherence signature of true sharing: **60.0 % of the
ring's LLC-misses resolve to remote-cache HITM** (66.7 % for the seqlock), concentrated on a
tiny set of shared cache lines (2 in each report) carrying heavy producer store traffic —
**99 store references on the ring's top shared line** versus **10 on the seqlock's**,
consistent with the ring's single write cursor being written on every push and polled from
every CCD. Critically, **no HITM appears on the 64-B-aligned per-slot payload lines** — the
alignment did eliminate false sharing, leaving only the true sharing any broadcast bus
inherits on its shared progress counter. The IBS sample is sparse (a handful of HITM events;
symbolization attributes some to std/kernel frames), so this **corroborates** the
throughput-decline attribution rather than pinpointing a source line — but the cross-CCD
HITM is now directly observed, not merely inferred from the K-sweep. The decline is in
**rate**, never in **progress** — the producer completes its full budget at every K
(wait-free). Figure: `plots/ring_producer_throughput_vs_consumers.svg`.

Overrun behavior tracks the producer/consumer speed ratio (`overrun_rate`). On this faster
core the consumer nearly keeps up even against a free-running producer: a lone consumer is
only **0.73 % lapped** (K=1 full_tilt) and **0 %** at K=2 (where the true-sharing decline
drops the producer below the aggregate drain rate); under a realistic **paced 1 MHz feed
every consumer keeps up and overrun is 0** at both K. This is a marked change from the
laptop, where the slower core left a lone consumer ~97 % lapped — the same mechanism, a
faster drain. Overruns are always **detected and reported**, never silent: the loom model
(`sync/tests/loom_ring.rs`) and stress test (`sync/tests/stress_ring.rs`) prove no torn or
duplicated record ever escapes.

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

## 5. End-to-end pipeline (`e2e.csv` — laptop Phase-8 baseline)

The engine composes the verified parts into one hot path: a pinned producer replays a
corpus through `EngineProducer::<BTreeBook>::process` — `book.apply` →
`seqlock.store(top_of_book)` → `ring.push(pack(ev))` — while `K` pinned independent
consumers each drain the ring, do light derived work, resync from the seqlock on overrun,
and record end-to-end latency. **Response time, coordinated-omission-correct:** the
producer stamps each event's `ts` with its scheduled arrival before processing, and each
consumer records `completion − scheduled_arrival`.

**Scope note.** The Phase-8 end-to-end benchmark was **outside the metal re-run scope**
(Phases 4/6/7/9), so the numbers in this section are the earlier laptop (i5-1135G7)
baseline, labeled as such and not comparable in absolute ns to the EPYC sections. The metal
run independently re-confirmed the two primitives this pipeline composes (§4) and directly
observed, via `perf c2c`, the true-sharing mechanism this section reports end-to-end — so
the *mechanism* is metal-confirmed even though the pipeline latency here is not re-measured.

**Real-corpus headline (BTCUSDT replay, speed 1, `BTreeBook`, laptop):** at K=1 the
pipeline runs **5,227 ns p50 / 128,767 ns p99** over 13,765 events (`e2e.csv`). The pipeline
idles the vast majority of the time (achieved ~305 eps against 304 target); the ~129 µs p99
is a property of the **captured feed**, not the pipeline — Binance `@depth@100ms` batches
deliver many events sharing one timestamp, scheduled simultaneously and serviced serially,
a genuine coordinated-omission effect the CO-correct stamping surfaces rather than averages
away.

**Synthetic fixed-rate floor and saturation (laptop):** while the pipeline serves the rate,
p50 sits at the **~110–140 ns full-pipeline floor** (apply + seqlock store + ring push +
cross-core propagation + recv) — e.g. K=1 reads 125, 126, 118, 129 ns p50 at 1/2/5/10 M eps
(`e2e.csv`). Once the target exceeds what the pipeline sustains, p50 and p99 jump to the
**millisecond scale** as the producer falls behind and the backlog is charged to every event
(the CO-correct saturation signal). The **max sustainable rate falls with K** — the highest
non-saturated cell is ~20 M eps at K=1, dropping toward ~13 M eps at K=4 (`saturated`
column) — tracking the producer-throughput decline below.

**The true-sharing reality, reported not engineered around:** at the saturated operating
point (100 M eps target) producer throughput is **20.60 → 16.98 → 12.74 Mev/s at K=1,2,4**
(`e2e.csv`), a ~38% decline. This is the same true-sharing-on-the-write-cursor effect
isolated in §4.2 and now **directly observed on EPYC via `perf c2c`** (§4.2). Phase 8
measures and reports it; it does not modify the verified `sync` primitives to chase it. The
pipeline's `overrun_rate` is 0.000000 in every cell — the consumer step is cheaper than the
producer step, so under load the cost is schedule backlog (coordinated omission), not ring
lapping; the overrun→resync composition is exercised where the producer genuinely outruns a
consumer (`engine/tests/pipeline.rs` and the deterministic overrun test in
`bench/src/benches/e2e.rs`). Figures: `plots/e2e_p99_vs_rate.svg`,
`plots/e2e_producer_throughput_vs_consumers.svg`.

---

## 6. Microarchitecture summary — now with native AMD Zen4 counters

The four implementations form a microarchitecture taxonomy. On the laptop it was
*predicted* from PMU-free behavioral signatures; on the EPYC box the native AMD Zen4
pipeline-utilization counters **confirm** it (`perf/perf_btree.txt`, `perf_sorted.txt`,
`perf_rev.txt`, `perf_flat.txt`; apply hot loop, depth 2048 uniform, 200 M iters). The AMD
buckets are the architectural counterpart to Intel TMA — **retiring / bad-speculation /
frontend-bound / backend-bound(memory|cpu)** — not Intel TMA relabeled
([`PROFILING.md`](PROFILING.md) maps each Zen4 event). Headline counters:

| impl | IPC | retiring | bad-spec | frontend | backend-mem | branch-miss | verdict |
|---|---|---|---|---|---|---|---|
| `SortedVecBook` | 2.50 | 40.2 % | **0.1 %** | 0.7 % | **50.5 %** | 0.04 % | **Memory Bound** (branchless locate) |
| `BTreeBook` | 1.33 | 20.1 % | 28.3 % | 21.4 % | 9.1 % | 8.12 % | Memory Bound + frontend (pointer chase) |
| `RevVecBook` | 6.10 | **93.8 %** | 1.3 % | 1.4 % | 2.3 % | 0.14 % | Core/Retiring (scan length) |
| `FlatBook` | 2.43 | 39.0 % | 25.5 % | 17.6 % | 13.4 % | 3.80 % | mispredict-bound at wide uniform depth |

- **`SortedVecBook` — Memory Bound, hardware-confirmed** (a prediction confirmed, see §7):
  **50.5 % backend-bound-memory with only 0.1 % bad-speculation and 0.04 % branch-miss**
  (`perf_sorted.txt`). Its binary search is a chain of `O(log n)` dependent loads in one
  contiguous array; the counters put the stall squarely on memory, not on speculation —
  exactly the PMU-free prediction.
- **`BTreeBook` — Memory Bound + frontend, pointer chase.** Lowest IPC (1.33), highest
  branch-miss (8.12 %), and a large frontend-bound-latency component (21.4 %): each descent
  step is a dependent load to a scattered node, so the prefetcher cannot run ahead and the
  front end stalls waiting on the next node address (`perf_btree.txt`).
- **`RevVecBook` — Core/Retiring.** **93.8 % retiring at IPC 6.10** (`perf_rev.txt`) — the
  scan simply executes and retires a huge number of compare-and-advance instructions; its
  cost is retired-instruction count (scan length), not cache or speculation.
- **`FlatBook` — retiring in-span, mispredict-bound at wide uniform depth.** At depth-2048
  uniform its apply carries **25.5 % bad-speculation and 3.80 % branch-miss**
  (`perf_flat.txt`) — the scattered writes across a wide flat array feed data-dependent
  bounds/recenter branches. This is the counter-level shadow of the real-data collapse: the
  wider the span, the worse the flat array's speculation and memory behavior.

The misprediction finding (from `branch_experiment.csv`, **laptop PMU-free baseline**): a
branchy binary search over **random** keys with all data in L1 is **5.1× slower** than over
predictable keys (36.093 vs 7.019 ns p50, depth 256) — a **+29.07 ns** pure-misprediction
penalty, rising to **+35.9 ns** at depth 16384. A branchless `cmov` search
(`std::hint::select_unpredictable`) is flat across predictability. The EPYC counters confirm
the mechanism at the impl level: the shipped `SortedVecBook`'s branchless locate shows
**0.04 % branch-miss / 0.1 % bad-spec** (`perf_sorted.txt`), while the branchy pointer-chase
`BTreeBook` shows **8.12 % branch-miss / 28.3 % bad-spec** — the branch-miss delta the
laptop 2×2 predicted, now on an AMD counter. Figures:
`plots/branch_misprediction_2x2.svg`, `plots/cache_footprint_latency.svg` (both laptop
behavioral experiments).

---

## 7. Honest findings & surprises

These are featured, not buried — they are what makes the work checkable.

- **The real-data inversion, sharper on EPYC.** The structure that wins every synthetic
  profile (`FlatBook`, 7.46 ns/event steady) is last by ~288× on the real feed (10,896.43
  ns/event), and the structure that loses every synthetic profile (`BTreeBook`, 19.47
  ns/event steady) leads it (37.79 ns/event) — because the real book's 88 MiB span is
  ~2.74× the 32 MiB per-CCD L3 (`throughput.csv` + `flat_memory.csv`). The inversion held
  against a cache 4× the laptop's, which strengthens rather than weakens the finding: "the
  tradeoff and the failure are one number."
- **The `SortedVecBook` refutation — predicted PMU-free, now hardware-confirmed.** It was
  predicted to be Bad-Speculation-bound (a binary search that mispredicts). It is **not**:
  `std::partition_point` compiles to **branchless** code on this toolchain, flat across key
  predictability in `branch_experiment.csv` (laptop), and the EPYC AMD counters close the
  case — **0.1 % bad-speculation, 0.04 % branch-miss, 50.5 % backend-bound-memory**
  (`perf_sorted.txt`). The shipped sorted book pays no branch-misprediction penalty; it is
  Memory Bound by its dependent-load chain, and the AMD backend-bound counter says so
  directly. The PMU-free behavioral signature *predicted* this before silicon *confirmed*
  it — the stronger form of the finding.
- **The true-sharing decline — inferred on the laptop, measured on EPYC.** The SPMC ring's
  producer throughput falls (~1.4× over 1→2 consumers here, ~2.5× over 1→4 on the laptop) —
  an honest negative against the original "flat throughput proves no false sharing"
  hypothesis. The cause is now directly observed: `perf c2c` shows cross-CCD HITM
  concentrated on the single shared write cursor (60.0 % of ring LLC-misses to remote-cache
  HITM, 99 store refs on that line) and **none on the aligned slots** (§4.2). The `align(64)`
  discipline eliminated *false* sharing; what remains is the *true* sharing any SPMC
  broadcast inherits, and the producer stays wait-free — the rate falls, the progress
  guarantee does not.
- **PMU-free predicted, AMD-counter confirmed.** The whole microarchitecture story was
  built on the laptop with no hardware counters, on unambiguous behavioral signatures. The
  EPYC re-run did not overturn a single category — it confirmed each against a native AMD
  Zen4 counter. Predicted-then-confirmed is a stronger result than either half alone, and
  the laptop PMU-free method is kept, not discarded.
- **Zero `unsafe`, loom-verified primitives.** Every crate is `#![forbid(unsafe_code)]`;
  both lock-free primitives are verified by loom models and corroborated by real-thread
  stress tests. Concurrent shared mutation is expressed with atomics, the sound tool under
  Rust's memory model, rather than `UnsafeCell`.
- **The residual caveat, stated.** Single socket, loopback: the Phase 6/7 cross-core
  coherence traffic is intra-socket, cross-CCD over Infinity Fabric, not inter-socket —
  strictly more local than a two-socket box and orthogonal to every pipeline bucket, but the
  one topology limitation. The archived laptop (i5-1135G7) baseline remains the historical
  reference for the datasets not re-run on metal (`branch_experiment.csv`,
  `cache_experiment.csv`, `e2e.csv`).

---

## 8. Reproducibility & artifacts

The committed CSVs under `bench/results/` are the source of truth; this document re-derives
every number from them. The exact regeneration commands are in §2. Per-claim provenance
(EPYC unless marked):

| claim group | source | host |
|---|---|---|
| crossover, `D*`, FlatBook flat service curve | `service_sweep.csv` (+ `.hgrm`) | EPYC |
| read path, `top_n_full`, best access | `read_path.csv` | EPYC |
| real-data inversion, synthetic throughput | `throughput.csv` | EPYC |
| FlatBook span / memory tradeoff | `flat_memory.csv` | host-independent (span/bytes) |
| sustained CO-correct response, saturation | `sustained.csv` | EPYC |
| seqlock read/write latency, retry rate | `seqlock_read.csv` | EPYC |
| ring push/recv latency, throughput, overrun | `ring_bench.csv` | EPYC |
| AMD Zen4 pipeline utilization (per impl) | `perf/perf_{btree,sorted,rev,flat}.txt` | EPYC |
| cache-line contention (HITM, true vs false sharing) | `perf/c2c_ring.txt`, `perf/c2c_seqlock.txt` | EPYC |
| end-to-end latency, saturation, true sharing | `e2e.csv` | **laptop baseline** (Phase 8 not re-run) |
| misprediction 2×2, branchless elimination | `branch_experiment.csv` | **laptop baseline** (PMU-free behavioral) |
| cache-footprint latency curve | `cache_experiment.csv` | **laptop baseline** (PMU-free behavioral) |
| environment, corpora fingerprints, clock floor | `env.json` | EPYC |
| earlier PMU-unavailable condition (historical) | `perf/perf_unavailable.txt`, `perf/perf_summary.csv` | laptop (superseded by `perf/perf_*.txt`) |

Figures are under `bench/results/plots/` and each is generated from its committed CSV by
`bench plot`. The mechanistic teardown is [`PROFILING.md`](PROFILING.md); the interim
per-phase notes (`RESULTS.md`, `seqlock.md`, `ring.md`, `e2e.md`) remain as working
records, consolidated here into the single public artifact.

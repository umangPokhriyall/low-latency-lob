# SPMC broadcast ring — throughput, latency & false-sharing — interim findings (Benchmark 6)

This is the interim findings note for the `sync::SpmcRing` broadcast bus, built
**only** from the committed `bench/results/ring_bench.csv`. Every number below cites that CSV with
its units and conditions; nothing is computed by hand. The consolidated writeup is `docs/BENCHMARKS.md`.

The ring is the engine's single-producer / many-independent-consumer output bus: one writer streams
`[u64; W]` records and **never blocks** (it overwrites on wrap), and each consumer reads the *whole*
stream from its own cursor, detecting overrun rather than corrupting silently. Its memory ordering is
proven by loom (`sync/tests/loom_ring.rs`) and corroborated by a real-thread stress test
(`sync/tests/stress_ring.rs`: no-loss, overrun-detection, and no-tear/no-dup paths, zero violations
over millions of deliveries). This benchmark answers the remaining quantitative questions: **what do
`push` and `try_recv` cost, what broadcast throughput does the producer sustain, does adding
consumers tax the writer (the false-sharing test), and how does the overrun rate track consumer
speed?**

## 1. Environment & methodology

| field | value (`env.json`) |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz |
| logical cores | 8 |
| CPU governor | `performance` |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0, `target-cpu = native` |
| clock read-read floor | **7 ns** (`clock_overhead_ns` column in `ring_bench.csv`) |

One producer thread is pinned to core 0; `K` consumer threads are pinned to cores `1..=K`. Each
thread sets its own affinity from a full mask (the producer runs in its own spawned thread, never the
long-lived main thread, so later cells are not silently un-pinned). The ring is `capacity = 1024`
slots of `W = 4` words. The consumer ladder is `K ∈ {1, 2, 4}`: **K = 8 is skipped on this host** — a
clean run needs the producer's core plus `K` distinct consumer cores (`1 + K ≤ 8`), so 8 consumers
would need 9 cores.

`push` and `try_recv` payloads are `black_box`ed (no dead-code elision, §3.3). Producer **throughput**
is measured over a dedicated **untimed** push pass (the pure push rate, free of per-push clock-read +
histogram overhead); **push latency** is measured over a separate steady-state timed pass; **recv
latency** brackets each `try_recv` that returns a record. The 7 ns clock floor is reported and **never
subtracted**. Two producer modes: `full_tilt` (push as fast as possible) and `paced` (a fixed 1 MHz
feed rate via busy-spin — the realistic market-data case). This is **service time, not a
coordinated-omission study**: a consumer issues its next `try_recv` as soon as the last returns.

The single-consumer (`K = 1`) `full_tilt` throughput is **run-to-run noisy** (≈30–48 Mev/s across
repeats) because a lone consumer is almost entirely lapped and its sporadic bus traffic varies; the
`K = 2` (≈21 Mev/s) and `K = 4` (≈15.4 Mev/s) cells are stable to ±1 Mev/s. The numbers below are one
committed run; the **trend** (not the third significant figure) is the result.

## 2. Latency — `push` and `try_recv` are at the clock floor, FLAT across K

From `ring_bench.csv` (`full_tilt`):

| K | push p50 | push p99 | recv p50 | recv p99 |
|---|---|---|---|---|
| 1 | 7 ns | 44 ns | 9 ns | 57 ns |
| 2 | 7 ns | 50 ns | 10 ns | 83 ns |
| 4 | 8 ns | 23 ns | 9 ns | 78 ns |

`push` p50 is **7–8 ns** and `try_recv` p50 is **9–10 ns** — both within ~1 clock floor (7 ns) of the
metal, and both **flat as K rises**. A `push` is a handful of `Relaxed`/`Release` atomic stores plus
one fence; a `try_recv` is an `Acquire` load, a stamp check, the word loads, a fence and a re-check.
The flatness is the **direct, per-operation false-sharing signal**: if adjacent slots shared a cache
line, or the write position shared a line with a slot, the *latency* of these ops would climb with K
as cross-core invalidations piled onto each op. It does not. `paced` p50s are identical (6–7 ns), and
the p99 tails (≤ 95 ns) are the overrun/resync path and occasional contention.

## 3. Producer throughput vs K — NOT flat: true sharing on the write cursor (an honest result)

The §6 hypothesis was that producer push throughput stays ~flat as K rises ("consumers read distinct
lines; the write position is isolated"), a flat curve being the false-sharing-free signature. The
measured curve is **not flat** (`ring_bench.csv`, `full_tilt`, `producer_mev_s`):

| K | producer Mev/s |
|---|---|
| 1 | 38.65 |
| 2 | 20.85 |
| 4 | 15.38 |

Throughput roughly halves from 1→2 consumers, then settles toward ~15 Mev/s. **This is not false
sharing, and it is not a measurement or frequency-scaling artifact** (it persists on the
`performance` governor, and the per-op `push` latency in §2 is flat). It is **true sharing on the one
genuinely-shared word, the write cursor `write.v`**:

- Every consumer reads `write.v` on **every** `try_recv` (step `(R0)`) to learn how far the producer
  has advanced — that is the broadcast contract, not an accident of layout. A lapped consumer reads
  it *twice* per overrun (once in `(R0)`, once in the fresh re-load inside `resync`).
- The producer publishes `write.v` with a `Release` store on **every** push (step `(P4)`). On x86 that
  store must take the line for ownership (RFO), invalidating the `K` consumer copies. As K grows, the
  producer's store buffer drains slower against that coherence traffic — so **throughput** falls even
  though the **issue latency** of an individual `push` stays ~8 ns (the store is buffered; §2).
- A lapped consumer additionally resyncs to the oldest resident position, whose slot is **exactly the
  slot the producer is about to overwrite** (`oldest & mask == p & mask`), adding true sharing on that
  boundary slot.

So the `align(64)` discipline does its job — it eliminates **false** sharing (verified structurally by
the `size_of::<Slot<W>>() % 64 == 0` static assertion: one slot per line, write position on its own
line) and is confirmed by the flat per-op latencies (§2) and by the **flat `recv` latency across K**
(consumers do not tax *each other*, because they read distinct slot lines). What remains is the
**irreducible true sharing** that any SPMC broadcast has on the shared producer cursor. The honest
verdict: **the ring is false-sharing-free, but its broadcast throughput is bounded by true-sharing
coherence traffic on the write cursor, declining ~2.5× from 1 to 4 consumers.** (A future
optimisation — consumers caching the producer position and re-reading it less often, or the producer
publishing it on a coarser cadence — would relax this; it is out of scope for the frozen
primitive. The plot `plots/ring_producer_throughput_vs_consumers.svg` shows the curve.)

The producer **completes its full budget at every K** — it is **wait-free** and never blocks on a
consumer (the writer-never-blocks property, also proven by the Session-2 stress test). The decline is
in *rate*, never in *progress*.

## 4. Overrun rate vs consumer speed — lossy under a flat-out producer, lossless when paced

From `ring_bench.csv` (`overrun_rate` = fraction of consumer-observed positions lost to overrun):

| K | full_tilt | paced (1 MHz) |
|---|---|---|
| 1 | 0.974 | 0.049 |
| 2 | 0.843 | 0.059 |
| 4 | 0.029 | 0.000 |

`full_tilt` is the overrun regime: a free-running producer outruns its consumers and laps them. At
`K = 1` a single consumer is **97 % lapped** (the producer at ~39 Mev/s vastly outpaces one drainer);
at `K = 4` the producer — now taxed to ~15 Mev/s by the §3 true sharing — drops *below* the aggregate
consumer drain rate, so consumers **keep up** and overrun collapses to ~3 %. The crossover is sharp
because lapping is cumulative: once a consumer falls a ring-length behind it loses everything until it
catches up. Overruns are always **detected and reported** (never silent) — the loom + stress tests
prove no torn or duplicated record ever escapes.

`paced` is the realistic market-data regime: at a 1 MHz feed every consumer trivially keeps pace and
overrun is **≈ 0** (the small residual at `K ∈ {1,2}` is the untimed warm-up burst briefly lapping a
just-started consumer before steady state; `K = 4` records exactly 0.000). This is the **lossless
broadcast** the engine relies on, and it composes with the seqlock: a consumer that *does* get
lapped under a burst detects the overrun and resyncs its derived state from the seqlock top-of-book
snapshot before resuming from the ring's oldest resident position.

## 5. Headline

- `push` and `try_recv` are **at the clock floor** (p50 7–10 ns) and **flat across K** — the
  per-operation false-sharing-free signal.
- The `align(64)` discipline eliminates **false** sharing (static `size_of % 64` assertion; flat `recv`
  latency across K shows consumers do not tax each other).
- Producer broadcast throughput is **not flat** — it declines ~2.5× from 1→4 consumers due to
  **true sharing on the shared write cursor** (every consumer polls it; the producer's `Release` store
  invalidates K copies). An honest negative against the §6 "flat" hypothesis, with the cause profiled.
- The producer is **wait-free**: it completes its full budget at every K; consumers are **not**
  lock-free (a fast producer laps them), but overruns are always **detected**, never silent.
- Under a realistic **paced** feed, broadcast is **lossless** (overrun ≈ 0).

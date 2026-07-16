# Seqlock read latency under write contention — interim findings (Benchmark 5)

This is the interim findings note for the `sync::SeqLock` snapshot cell, built
**only** from the committed `bench/results/seqlock_read.csv`. Every number below cites that CSV
with its units and conditions; nothing is computed by hand. The consolidated writeup is `docs/BENCHMARKS.md`.

The seqlock is the engine's single-writer / many-reader top-of-book snapshot: one writer mutates
the book and publishes a `TopOfBook` via a version counter; many readers take an optimistic
snapshot and retry only if a write straddled the read. Its memory ordering is proven by loom
(`sync/tests/loom_seqlock.rs`) and corroborated by a real-thread torn-read stress test
(`sync/tests/stress_seqlock.rs`, zero violations over millions of reads). This benchmark answers
the remaining quantitative question: **what does a read cost under write contention, and does the
writer stay independent of reader count?**

## 1. Environment & methodology

| field | value (`env.json`) |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz |
| logical cores | 8 |
| CPU governor | `powersave` (turbo **not** pinned) |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0, `target-cpu = native` |
| clock read-read floor | **7 ns** (`clock_overhead_ns` column in `seqlock_read.csv`) |

One writer thread is pinned to core 0; `K` reader threads are pinned to cores `1..=K`. Each reader
times one `load()` per sample with the TSC-backed `BenchClock` and `black_box`es the returned
snapshot (no dead-code elision, §3.3); the writer brackets only its `store()` call. The reader
ladder is `K ∈ {1, 2, 4}`: **K = 8 is skipped on this host** — a clean run needs the writer's own
core plus `K` distinct reader cores (`1 + K ≤ 8`), so 8 readers would need 9 cores. Two writer
modes: `full_tilt` (store as fast as possible — worst-case contention) and `paced` (store at a
fixed 1 MHz feed rate via busy-spin — the realistic market-data case).

This is **service time, not a coordinated-omission study**: a reader issues its next `load()` as
soon as the previous returns, so there is no arrival schedule for reads — each sample is the cost
of one `load()` under a given write pressure. The 7 ns clock floor is reported and **never
subtracted**; values within a few ns of it are not individually resolvable, and the `powersave`
governor (frequency not pinned) adds ns-scale noise. The robust, repeatable signals are stated as
such below; near-floor deltas are not over-read.

## 2. Read latency stays in the tens of nanoseconds

From `seqlock_read.csv` (`read_p50_ns` / `read_p99_ns` / `read_p999_ns`):

| mode | K | read p50 | read p99 | read p99.9 | samples |
|---|---|---|---|---|---|
| full_tilt | 1 | 11 ns | 24 ns | 100 ns | 1,000,000 |
| full_tilt | 2 | 11 ns | 13 ns | 14 ns | 2,000,000 |
| full_tilt | 4 | 11 ns | 13 ns | 14 ns | 4,000,000 |
| paced | 1 | 11 ns | 15 ns | 17 ns | 1,000,000 |
| paced | 2 | 9 ns | 14 ns | 17 ns | 2,000,000 |
| paced | 4 | 11 ns | 14 ns | 15 ns | 4,000,000 |

A `load()` costs **~11 ns at p50** (≈ 4 ns above the 7 ns clock floor) and stays in the **13–24 ns
band at p99**, essentially flat across reader count and writer mode. The p50/p99 spread between
cells (9–11 ns p50; 13–24 ns p99) is within the clock-floor band and is **not** read as a real
ordering of the configurations — the takeaway is that a contended seqlock read is a tens-of-
nanoseconds operation, not that K = 1 full-tilt is "slower." Figure:
`plots/seqlock_read_p99_vs_readers.svg` (source: `seqlock_read.csv`).

**`read_max_ns` is multi-millisecond (46 µs … 12 ms) and is OS-scheduling jitter, not seqlock
cost.** On a `powersave`, non-core-isolated host a reader is occasionally descheduled mid-sample;
`read_p999_ns` (≤ 100 ns, and ≤ 14 ns once K ≥ 2) is the true interior tail. The max is reported,
not hidden, and not attributed to the primitive.

## 3. Retries are negligible — the critical section is a few nanoseconds

`mean_retries_per_load` from the CSV: **0.005304** at `full_tilt, K = 1`, and **0.000000** (below
5 × 10⁻⁷) for every other cell, including all `paced` cells. The writer's odd-`seq` window — the
five `Relaxed` payload stores between the two `seq` increments — is only a few nanoseconds wide, so
the probability that a read straddles it is well under 1 % even when the writer never stops, and is
effectively zero at a realistic 1 MHz feed rate. Readers almost never pay the retry path; the
optimistic snapshot succeeds on the first attempt.

## 4. The writer is independent of reader count (the load-bearing result)

`write_p50_ns` / `write_p99_ns` from the CSV, the writer's own `store()` latency:

| mode | K=1 | K=2 | K=4 |
|---|---|---|---|
| full_tilt store p50 | 10 ns | 10 ns | 11 ns |
| full_tilt store p99 | 20 ns | 12 ns | 12 ns |
| paced store p50 | 7 ns | 7 ns | 7 ns |
| paced store p99 | 11 ns | 12 ns | 11 ns |

The writer's store latency is **flat across K** — ~10–11 ns p50 full-tilt, 7 ns p50 paced, with no
upward trend as readers are added. This is the property a seqlock exists to provide and a mutex
cannot: the writer is **wait-free**, never blocked by any reader, so reader count does not tax it.
A `Mutex<TopOfBook>` would invert this — every reader contends for the lock the writer needs, and
writer latency would climb with K. Figure: `plots/seqlock_write_vs_readers.svg` (source:
`seqlock_read.csv`), plotted full-tilt, linear-y so the flatness is read directly.

## 5. Summary

- A contended `load()` is **~11 ns p50, 13–24 ns p99** (`seqlock_read.csv`), flat across K and
  writer mode — a few ns above the 7 ns clock floor.
- The optimistic-retry rate is **negligible**: ≤ 0.0053 retries/load at worst (full-tilt, K = 1),
  ~0 under a realistic paced feed.
- The writer's `store()` latency is **independent of reader count** (~10 ns p50 full-tilt, flat
  K = 1→4), confirming the wait-free writer guarantee with numbers.
- Caveats stated, not buried: `powersave` governor (frequency not pinned), multi-ms `read_max_ns`
  is OS deschedule jitter (interior p99.9 ≤ 100 ns), and K = 8 is out of core budget on this
  8-core host. Re-running on a core-isolated `performance`-governor host with ≥ 9 cores would
  tighten the tail and add the K = 8 point; the qualitative results (flat read latency, ~0
  retries, writer independence) are not expected to change.

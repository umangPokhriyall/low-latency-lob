# End-to-end production-to-consumption latency — interim findings (Benchmark 7)

This is the interim findings note for the assembled `engine` pipeline, built
**only** from the committed `bench/results/e2e.csv`. Every number below cites that CSV with its units
and conditions; nothing is computed by hand. The consolidated writeup is `docs/BENCHMARKS.md`.

The pipeline is the first assembly of all the verified parts into one hot path: a pinned producer
replays a corpus through `EngineProducer::<BTreeBook>::process` — `book.apply` →
`seqlock.store(top_of_book)` → `ring.push(pack(ev))` — while `K` pinned, independent consumers each
drain the SPMC broadcast ring (`EngineConsumer::poll`), do light derived work (unpack + a mid-price),
resync from the seqlock on overrun, and record the end-to-end latency of every cleanly delivered
event. The frozen `book`, the loom-verified `sync` seqlock and ring, and the deterministic `feed` are
composed unchanged; the engine adds only the pipeline logic and the `BookEvent ↔ [u64; 5]` packing.
This benchmark answers: **what is the production→consumption latency of the whole composed pipeline,
where does it saturate, and how does adding consumers tax the producer?**

## 1. Environment & methodology

| field | value (`env.json`) |
|---|---|
| CPU | 11th Gen Intel Core i5-1135G7 @ 2.40 GHz |
| logical cores | 8 |
| CPU governor | `performance` |
| kernel | 7.0.0-15-generic |
| rustc | 1.95.0, `target-cpu = native` |
| clock read-read floor | **7 ns** (`clock_overhead_ns` column in `e2e.csv`) |

One producer thread is pinned to core 0; `K` consumer threads are pinned to cores `1..=K`. Each
thread sets its own affinity from a full mask (the producer runs in its own spawned thread, never the
long-lived main thread, so later cells are not silently un-pinned). The ring is `capacity = 4096`
slots of `W = 5` words (one `BookEvent`). The consumer ladder is `K ∈ {1, 2, 4}`: **K = 8 is skipped
on this host** — a clean run needs the producer's core plus `K` distinct consumer cores
(`1 + K ≤ 8`), so 8 consumers would need 9 cores.

Observed events are `black_box`ed (no dead-code elision, §3.3). The 7 ns clock floor is reported and
**never subtracted**. Pacing is a busy-spin against the bench clock, never a sleep (a syscall would
dwarf the ns-scale op). The producer and every consumer of a cell share **one clock base**, published
once the producer is past warmup and all consumers have crossed a readiness barrier, so the one-way
cross-thread latency is measured against a single timeline.

### Coordinated-omission correctness (the load-bearing detail)

The producer paces the replay to a schedule and **stamps each event's `ts` with its scheduled arrival**
(ns from the shared base) *before* processing it. Each consumer records
`latency = now_since_ns(base) − ev.ts` — completion minus **scheduled** arrival, never minus push
time. So when the pipeline falls behind at saturation, the accumulated backlog is charged to every
late event and the tail captures it; the naive "now − push_time" form would measure service time and
hide the very coordinated omission this study exists to expose (the §7 CO-proof in
`bench/src/benches/sustained.rs` pins the same distinction for the single-thread case).

Two schedules: real-arrival replay of `btcusdt-sample.mdf` at `speed = 1` (the headline real-data
run) and a synthetic fixed-rate sweep over `steady-s1-100k.mdf` to saturation.

## 2. Real-corpus headline (BTCUSDT, speed 1)

`schedule = real`, 13 765 events, ~304 eps natural arrival. The producer applies BTreeBook (the
real-data champion) and broadcasts; consumers measure full-pipeline latency. (`e2e.csv`, `samples` =
`13765 × K`.)

| K | e2e p50 (ns) | e2e p99 (ns) | e2e p99.9 (ns) | e2e max (ns) |
|---|---|---|---|---|
| 1 | 5 227 | 128 767 | 131 071 | 131 327 |
| 2 | 5 587 | 129 215 | 138 111 | 139 135 |
| 4 | 2 901 | 153 855 | 164 863 | 166 143 |

At the real feed's ms-scale arrival rate the pipeline keeps up comfortably — the producer is idle the
vast majority of the time (`producer_mev_s ≈ 0.0003`, i.e. ~305 eps achieved against 304 target). The
median is single-digit µs and the **p99 of ~129 µs is a property of the captured feed, not the
pipeline**: Binance `@depth@100ms` batches deliver many events sharing one timestamp, so events at the
back of a batch are *scheduled* simultaneously and serviced serially — a genuine coordinated-omission
effect the CO-correct stamping surfaces honestly rather than averaging away.

## 3. Synthetic latency vs rate, and saturation

`schedule = fixed`, `steady-s1-100k.mdf`, 100 000 events. A cell is `saturated` (`e2e.csv` last
column) when achieved throughput fell below 90 % of target — the producer could no longer keep the
schedule. End-to-end p50 / p99 (ns) by target rate (eps):

| target eps | K=1 p50 | K=1 p99 | K=2 p50 | K=2 p99 | K=4 p50 | K=4 p99 |
|---|---|---|---|---|---|---|
| 1 M  | 125 | 90 495 | 130 | 186 | 115 | 227 |
| 2 M  | 126 | 2 457 | 126 | 184 | 108 | 181 |
| 5 M  | 118 | 1 864 | 126 | 204 | 116 | 238 |
| 10 M | 129 | 4 495 | 140 | 487 | 115 | 3 719 |
| 20 M | 14 095 | 46 623 | **405 503** | **837 631** | **1 417 215** | **2 797 567** |
| 50 M | **1 378 303** | **2 785 279** | **1 935 359** | **3 854 335** | **2 904 063** | **5 738 495** |
| 100 M | **1 940 479** | **3 819 519** | **2 437 119** | **4 841 471** | **3 428 351** | **6 787 071** |

(Bold = `saturated = true`, except the K=1 20 M cell which kept the schedule but with a rising tail.)

The shape is the expected open-loop response: while the pipeline can serve the rate, p50 sits at the
**~110–140 ns full-pipeline floor** (apply + seqlock store + ring push + cross-core propagation +
recv) and the tail is sub-µs to low-µs. Once the target exceeds what the pipeline sustains, p50 and
p99 jump to the **millisecond** scale because the producer falls behind the schedule and the backlog is
charged to every event — the CO-correct saturation signal. The **max sustainable rate falls with K**:
the highest non-saturated cell is ~20 M eps at K=1, dropping toward ~13–17 M eps at K=4 (`saturated`
column), tracking the producer-throughput curve in §4.

## 4. The true-sharing curve (the §6 result, reported not engineered around)

At the saturated (free-running) operating point — `target = 100 M eps`, where the producer pushes as
fast as it can — `producer_mev_s` is the producer's full-tilt broadcast throughput under `K`
consumers (`e2e.csv`, `schedule = fixed`):

| K | producer throughput (Mev/s) | achieved eps |
|---|---|---|
| 1 | **20.60** | 20 596 991 |
| 2 | **16.98** | 16 983 168 |
| 4 | **12.74** | 12 735 104 |

Producer throughput **declines ~38 % from K=1 to K=4**. This is **true sharing** on the producer's
write cursor: every `Consumer::try_recv` issues an `Acquire` load of the producer's `write.v` to learn
how far the stream has advanced, so the cache line holding `write.v` ping-pongs between the producer
(which writes it once per push) and every consumer (which reads it). It is **not false sharing** — the
ring slots are `#[repr(align(64))]`, one slot per cache line, and `WritePos` sits alone on its own
line (the ring's stress tests proved the slot/cursor isolation, and `ring_bench.csv` shows the *push op itself* stays
flat across K). The cost here is the single, genuinely-shared progress counter that a broadcast bus
with independent cursors inherently needs. This is measured and reported; it does **not**
modify the verified `sync` primitives to chase it.

Plots (each cites `e2e.csv`):
`plots/e2e_p99_vs_rate.svg` — end-to-end p99 vs target rate, one line per K (the CO-correct tail and
its saturation knee); `plots/e2e_producer_throughput_vs_consumers.svg` — the producer-throughput-vs-K
decline above.

## 5. Overrun behaviour

`overrun_rate = 0.000000` in **every** cell (`e2e.csv`). This is a real, explainable result, not a
gap: the consumer step (`try_recv` + unpack + a clock read + a histogram record) is **cheaper than the
producer step** (apply + seqlock store + ring push), so a consumer never trails the producer by a full
ring capacity (4096) — it keeps up, and the latency cost under load is **schedule backlog
(coordinated omission), not ring lapping**. The overrun → seqlock-resync composition is therefore
exercised and asserted *elsewhere*, where the producer genuinely outruns a consumer:
`engine/tests/pipeline.rs` §4.3 (a stalled consumer on a tiny ring resyncs, `resyncs > 0`, post-resync
delivery ordered) and the `high_rate_produces_overruns` unit test in `bench/src/benches/e2e.rs` (a
single consumer against a free-running stream is lapped and reports `skipped > 0`).

## 6. Next optimization — the batched-recv mitigation (hypothesis, NOT implemented here)

The §4 true-sharing decline is the direct evidence for a future `sync` enhancement, documented now and
left unbuilt this phase: a consumer that **caches the last-seen `write.v` and re-reads it only once it
has drained up to the cached value** would amortize the shared-cursor `Acquire` load across a batch of
records, cutting the ping-pong pressure on the producer's write line at the cost of slightly staler
"is there new data" visibility. That is a batched `recv` path in the verified ring, so it would need
its own loom + stress re-verification and is out of scope for the assembly phase. It is the measure-
first, optimize-with-evidence discipline in action: **this benchmark produced the number that would
justify the change**.

## 7. Honest caveats

- **One host, governor `performance`** (recorded in `env.json`). Numbers are host-specific
  (`target-cpu = native` by design); they are reproducible up to timing noise, not portable.
- **K = 8 not measured** on this 8-core host (it needs 9 cores); the true-sharing trend across
  {1, 2, 4} is the evidence, and the slope is monotone.
- The real-corpus p99 reflects the **captured feed's batching**, not a pipeline limit (§2); the
  synthetic sweep is where the pipeline's own saturation is measured (§3).
- This is the interim note. `docs/PROFILING.md` and the consolidated
  `docs/BENCHMARKS.md` are where these numbers are cross-checked against counters and written up in full.

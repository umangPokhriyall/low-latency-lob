# Distribution thread — the LOB engine

A findings-led, sourced technical thread for the low-latency / Rust / systems community.
Every number traces to a committed CSV (see [`BENCHMARKS.md`](BENCHMARKS.md)) or
[`PROFILING.md`](PROFILING.md). Proof-first: the repo is the opener. No hype, no emoji,
numbers over adjectives. Post breaks marked `———`.

———

**1/**

I built a limit-order-book engine where the "obviously optimal" data structure — a flat
array with `O(1)` indexed updates — lost by ~254× on real market data.

It wins every synthetic benchmark. On a real BTCUSDT book it is dead last. One number
explains why.

Repo (frozen core, loom-verified primitives, sourced benchmarks):
github.com/umangPokhriyall/Web3-Terminal

———

**2/**

The setup: four order-book implementations behind one trait — sorted Vec (binary search),
BTreeMap (pointer chase), reverse Vec (linear scan from best), and a flat array (direct
index `bid_qty[px - base]`).

A differential oracle proves all four observationally identical, so every benchmark
difference is pure performance, not behavior.

———

**3/**

Whole-corpus replay, ns/event (`throughput.csv`):

steady (synthetic, narrow book):
  flat 8.85 · sorted 13.5 · rev 15.1 · btree 22.3

btcusdt-sample (real, wide book):
  btree 43.1 · sorted 62.6 · rev 222 · flat 10,927

The ranking fully inverts. Best becomes worst.

———

**4/**

The mechanism is the memory hierarchy meeting book width.

The flat array's size is proportional to price *span*, not to occupied levels. Synthetic
book span: ~0.13 MiB (fits L2). Real BTCUSDT span: ~88 MiB — about 11× the 8 MiB LLC
(`flat_memory.csv`).

Every update is now a cache miss, plus recenter/grow churn as prices walk outward. The
tradeoff and the failure are the same number.

———

**5/**

BTreeMap wins the real book for the same reason the flat array loses it: its memory tracks
the *number of levels*, not the price span. A wide, sparse book is compact `O(log n)`
nodes to a tree and 88 MiB of mostly-empty array to the flat structure.

"Optimal" is a function of the data, not the structure.

———

**6/**

The synthetic crossover is also not what intuition says. The depth at which a linear scan
from the best price loses to a binary search is set by *touch locality*, not by depth
(`service_sweep.csv`):

- concentrated (top-of-book) touches: crossover at depth 256
- uniform touches: crossover at depth 2

———

**7/**

Why: the linear scan's cost is its scan length = levels touched. Concentrated touches land
near the best (scan 1–2 levels, flat in depth); uniform touches average ≈depth/2 (linear
in depth → 697 ns at depth 2048 vs the binary search's 14 ns).

It is retired instruction count, gated by where the touches land. Locality, not depth.

———

**8/**

Two lock-free primitives carry the concurrency, both with zero `unsafe`:

- a single-writer/many-reader **seqlock** for the top-of-book snapshot
- a single-producer/many-consumer **SPMC broadcast ring** for the full event stream

The whole workspace is `#![forbid(unsafe_code)]`. Both are model-checked with loom.

———

**9/**

The zero-`unsafe` part is a real design decision, not a flex.

The textbook seqlock uses `UnsafeCell` + a non-atomic memcpy under a version guard. Under
Rust's memory model that is a data race — UB — even though the version counter discards a
torn read. The discard does not make the race defined.

———

**10/**

The sound fix: make the payload atomic. Store each word as `AtomicU64` accessed `Relaxed`;
carry ordering with the version/stamp counter + `Acquire`/`Release` fences. A torn read
then reads stale-but-valid atomic words and is *detected*, not UB.

Result (`seqlock_read.csv`): read ~11 ns p50, writer latency flat across reader count —
the wait-free-writer property a mutex cannot give.

———

**11/**

The ring is lossy-but-detected: the producer never blocks (it overwrites on wrap), and a
lapped consumer gets an `Overrun`, then resyncs its derived state from the seqlock
snapshot before resuming. Lossy ring + always-current seqlock compose into a
self-healing consumer.

Honest progress: writer wait-free; readers/consumers are not lock-free.

———

**12/**

An honest negative result: the ring's producer throughput is *not* flat in consumer count
— it declines ~2.5× from 1→4 consumers (`ring_bench.csv`).

Not false sharing (slots are 64-byte aligned; per-op latency stays flat). It is *true*
sharing on the one genuinely-shared word: the write cursor every consumer must poll. A
broadcast bus inherits that.

———

**13/**

A refuted hypothesis, kept in the writeup. I predicted the sorted-Vec book would be
misprediction-bound (a branchy binary search).

It is not. `std::partition_point` compiles to branchless code on this toolchain — flat
across predictable vs random keys (`branch_experiment.csv`). The sorted book is
memory-bound, not speculation-bound.

———

**14/**

The misprediction penalty is still real and quantified — for a *branchy* search:
+29 ns at L1-resident depth 256, random vs predictable keys, eliminated by a
`select_unpredictable` cmov.

The shipped code already dodges it: structurally (the flat array does no search) and
instrumentally (`std`'s search is branchless).

———

**15/**

One more piece of honesty: the host denied hardware performance counters
(`perf_event_paranoid = 4`, no `CAP_PERFMON`).

So the microarchitecture analysis stands PMU-free — top-down categories inferred from
behavioral signatures (a misprediction 2×2, a cache-footprint latency curve). No counter
is fabricated; the unavailability is recorded. ([`PROFILING.md`](PROFILING.md))

———

**16/**

Everything is built from committed data: every headline re-derives from a CSV under
`bench/results/`, coordinated-omission-correct, with the clock floor reported and never
subtracted.

Frozen sans-IO core, four impls, two loom-verified primitives, a full microarchitecture
teardown — and the findings that lost are featured, not hidden.

Repo: github.com/umangPokhriyall/Web3-Terminal

Technical critique welcome — tell me where the measurement or the mechanism is wrong.

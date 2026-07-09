# Low-latency limit-order-book engine

[![CI](https://github.com/umangPokhriyall/Web3-Terminal/actions/workflows/ci.yml/badge.svg)](https://github.com/umangPokhriyall/Web3-Terminal/actions/workflows/ci.yml)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/umangPokhriyall/Web3-Terminal)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A single-symbol limit-order-book engine built as falsifiable proof-of-work: four
order-book implementations behind one frozen sans-IO trait, two loom-verified lock-free
concurrency primitives, a deterministic replay feed, and a coordinated-omission-correct
benchmark harness — measured, explained at the microarchitecture level, and honest about
what lost.

## Headline numbers

All sourced to [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md), re-derived from committed CSVs
under `bench/results/`. Hardware-fidelity-dependent numbers were re-run on rented AMD EPYC
bare metal (Latitude.sh `m4.metal.large`, EPYC 9254, 24c/48t, 4 CCDs × private 32 MiB L3,
`performance` governor, kernel 6.8.0-124, `target-cpu=native`, `bench/results/env.json`) —
anyone can rent this exact SKU hourly and re-run.

- **The "obviously optimal" flat array loses by ~288× on real market data.** On the real
  BTCUSDT replay `FlatBook` costs **10,896 ns/event** while `BTreeBook` leads at **37.79
  ns/event** — yet `FlatBook` wins every synthetic profile (7.46 ns/event on `steady`).
  The cause is one number: the real book's per-side span is **~88 MiB, ~2.74× the 32 MiB
  per-CCD L3** — the inversion held even against a cache 4× the laptop's (`throughput.csv`,
  `flat_memory.csv`).
- **The data-structure crossover is locality-gated, not depth-gated:** under uniform touches
  a linear scan loses to the binary search by depth ≈64 and degrades to 519 ns vs 29 ns
  (~18×) at depth 2048; under top-concentrated touches it never loses within the swept range
  (`service_sweep.csv`).
- **SortedVec is memory-bound, not speculation-bound — PMU-free-predicted, then confirmed on
  native AMD Zen 4 counters:** **50.5 % backend-bound-memory / 0.1 % bad-speculation /
  0.04 % branch-miss** (`perf/perf_sorted.txt`).
- **Seqlock read ~10 ns p50, writer latency flat across reader count** (`seqlock_read.csv`);
  **SPMC ring push/recv ~10 ns p50**, true-sharing on the write cursor confirmed by
  `perf c2c` cross-CCD HITM (`ring_bench.csv`, `perf/c2c_ring.txt`).

## Architecture

```
  corpus replay (feed, integer ticks)
        │  BookEvent
        ▼
  ┌──────── producer (pinned) ────────┐
  │  book.apply(ev)        frozen LOB  │
  │  seqlock.store(top) ──► SeqLock    │  latest top-of-book snapshot
  │  ring.push(pack(ev)) ─► SpmcRing   │  full broadcast stream
  └────────────────────────────────────┘
            │ seqlock        │ ring (broadcast)
       ┌────┴─────┐    ┌─────┴──────┬───────────┐
       ▼          ▼    ▼            ▼           ▼
   consumer0  consumer1  ...   consumerK-1   (each pinned, independent)
     poll(): Item -> work | Overrun -> resync from seqlock | Empty -> spin
```

Five crates, one acyclic graph rooted at the frozen `book`: `book` (sans-IO core) ←
`feed`, `sync`, `engine`, `bench`. Design writeup: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Credibility signals

- **Coordinated-omission-correct benchmarks** — service time and response time never
  blurred; response latency is `completion − scheduled_arrival`, so backlog lands in the
  tail (`bench`).
- **Two loom-verified lock-free primitives** — a single-writer/many-reader seqlock and a
  single-producer/many-consumer broadcast ring, model-checked under `--cfg loom`.
- **Zero `unsafe` workspace-wide** — every crate is `#![forbid(unsafe_code)]`; concurrent
  shared mutation uses atomics, not `UnsafeCell`.
- **A frozen sans-IO core with a differential oracle** — four implementations proven
  observationally identical (`book/tests/oracle.rs`), then frozen at `book-v1-frozen` and
  driven by every harness unmodified.
- **A top-down microarchitecture teardown** — each implementation's bottleneck predicted
  from PMU-free behavioral signatures on a laptop, then confirmed against native AMD Zen 4
  pipeline-utilization counters on rented EPYC bare metal ([`docs/PROFILING.md`](docs/PROFILING.md)).

## Honest findings featured, not hidden

- **The real-data inversion** above: `FlatBook`'s `O(1)` index is fastest on a narrow
  synthetic book and last by ~288× on the wide real one, because its span blows the cache.
  The tradeoff and the failure are the same number.
- **A refuted hypothesis, hardware-confirmed:** `SortedVecBook` was predicted to be
  misprediction-bound. It is not — `std::partition_point` is already branchless on this
  toolchain (`branch_experiment.csv`), and the EPYC AMD counters close it: 0.1 %
  bad-speculation, 50.5 % backend-bound-memory (`perf/perf_sorted.txt`). Memory-bound, not
  speculation-bound.
- **PMU-free predicted, AMD-counter confirmed:** the laptop denied hardware counters
  (`perf_event_paranoid = 4`) so the microarchitecture analysis stood on behavioral
  signatures; the rented EPYC box exposed the native AMD Zen 4 PMU and confirmed every
  category — predicted-then-confirmed, not either half alone
  ([`docs/PROFILING.md`](docs/PROFILING.md)).

## Build / test / run / reproduce

```sh
# Build + the three gates the repo ships behind.
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Model-check the lock-free primitives' memory ordering (separate, slow).
RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_seqlock --test loom_ring --release

# Reproduce the numbers (host-specific by design; writes CSVs under bench/results/).
cargo build --release -p bench
./target/release/bench service    --core 0   # service_sweep.csv  (the crossover)
./target/release/bench throughput --core 0   # throughput.csv     (the real-data inversion)
./target/release/bench sustained  --core 0   # sustained.csv      (CO-correct response time)
./target/release/bench seqlock    --core 0   # seqlock_read.csv
./target/release/bench ring       --core 0   # ring_bench.csv
./target/release/bench e2e        --core 0   # e2e.csv
./target/release/bench branch-exp --core 0   # branch_experiment.csv
./target/release/bench cache-exp  --core 0   # cache_experiment.csv
./target/release/bench flatmem               # flat_memory.csv
./target/release/bench plot --out bench/results   # env.json + plots/*.svg
# or everything: ./target/release/bench all --core 0
```

## Documentation

- [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) — the consolidated, sourced benchmark writeup.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the design writeup and crate DAG.
- [`docs/PROFILING.md`](docs/PROFILING.md) — the top-down microarchitecture teardown.
- [`docs/SELF-AUDIT.md`](docs/SELF-AUDIT.md) — the hardest-mechanism study aid.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

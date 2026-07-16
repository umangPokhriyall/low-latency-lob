# Low-latency limit-order-book engine

[![CI](https://github.com/umangPokhriyall/low-latency-lob/actions/workflows/ci.yml/badge.svg)](https://github.com/umangPokhriyall/low-latency-lob/actions/workflows/ci.yml)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/umangPokhriyall/low-latency-lob)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A single-symbol limit-order-book engine in Rust: four order-book implementations
behind one sans-IO trait, two loom-verified lock-free concurrency primitives, a
deterministic replay feed, and a coordinated-omission-correct benchmark harness.

Highlights:

- Four order-book implementations (`BTreeBook`, `SortedVecBook`, `RevVecBook`,
  `FlatBook`) proven observationally identical by a differential oracle, then
  benchmarked against each other on synthetic and real market data
- A single-writer/many-reader seqlock and a single-producer/many-consumer
  broadcast ring, both model-checked with loom
- Integer tick/lot arithmetic throughout the measured path; no floats, no
  allocation in the hot loop
- Coordinated-omission-correct measurement: service time and response time are
  reported separately
- `#![forbid(unsafe_code)]` in every crate
- Microarchitecture-level profiling of each implementation's bottleneck

## Results

All numbers are re-derived from committed CSVs under `bench/results/` and
consolidated in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). Hardware-dependent
numbers were measured on an AMD EPYC 9254 bare-metal host (24c/48t, 4 CCDs ×
32 MiB L3, `performance` governor, `target-cpu=native`; full environment in
`bench/results/env.json`).

- **The flat array loses by ~288× on real market data.** On a real BTCUSDT
  replay, `FlatBook` costs **10,896 ns/event** while `BTreeBook` leads at
  **37.79 ns/event** — even though `FlatBook` wins every synthetic profile
  (7.46 ns/event on `steady`). The cause: the real book's per-side span is
  ~88 MiB, ~2.74× the 32 MiB per-CCD L3 (`throughput.csv`, `flat_memory.csv`).
- **The data-structure crossover is locality-gated, not depth-gated.** Under
  uniform touches a linear scan loses to binary search by depth ≈64 and degrades
  to 519 ns vs 29 ns (~18×) at depth 2048; under top-concentrated touches it
  never loses within the swept range (`service_sweep.csv`).
- **`SortedVecBook` is memory-bound, not speculation-bound.** It was initially
  hypothesized to be misprediction-bound; AMD Zen 4 counters show 50.5 %
  backend-bound-memory, 0.1 % bad-speculation, 0.04 % branch-miss —
  `std::partition_point` is already branchless on this toolchain
  (`branch_experiment.csv`, `perf/perf_sorted.txt`).
- **Seqlock reads ~10 ns p50 with writer latency flat across reader count**
  (`seqlock_read.csv`); **SPMC ring push/recv ~10 ns p50**, with true sharing on
  the write cursor confirmed by `perf c2c` cross-CCD HITM samples
  (`ring_bench.csv`, `perf/c2c_ring.txt`).

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

Five crates, one acyclic graph rooted at `book`: `book` (sans-IO core) ←
`feed`, `sync`, `engine`, `bench`. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Build, test, reproduce

```sh
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

- [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) — consolidated benchmark writeup
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design writeup and crate DAG
- [`docs/PROFILING.md`](docs/PROFILING.md) — microarchitecture analysis

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

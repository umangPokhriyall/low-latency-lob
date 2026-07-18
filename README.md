# Low-latency limit-order-book engine

[![CI](https://github.com/umangPokhriyall/low-latency-lob/actions/workflows/ci.yml/badge.svg)](https://github.com/umangPokhriyall/low-latency-lob/actions/workflows/ci.yml)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/umangPokhriyall/low-latency-lob)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A single-symbol limit-order-book engine in Rust with four interchangeable
order-book implementations behind a shared sans-IO trait, two loom-verified
lock-free concurrency primitives, deterministic market-data replay, and a
coordinated-omission-correct benchmark harness.

The project explores how data structure choice, cache locality, and lock-free
publication affect end-to-end market-data processing latency.

Highlights:

- Four order-book implementations (`BTreeBook`, `SortedVecBook`, `RevVecBook`,
  `FlatBook`) shown observationally identical by a differential oracle, then
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

The complete benchmark methodology, raw measurements, and microarchitectural
analysis are documented in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) and
[`docs/PROFILING.md`](docs/PROFILING.md). All reported numbers are re-derived
from committed CSVs under `bench/results/` and were measured on an AMD EPYC
9254 bare-metal host (full environment in `bench/results/env.json`).

Two representative findings:

- **The flat array loses by ~288× on real market data.** On a real BTCUSDT
  replay, `FlatBook` costs **10,896 ns/event** while `BTreeBook` leads at
  **37.79 ns/event**, even though `FlatBook` wins every synthetic profile.
  The real book's per-side span (~88 MiB) exceeds the 32 MiB per-CCD L3,
  making locality—not asymptotic complexity—the dominant cost.
- **The lock-free seqlock and broadcast ring both operate near the clock-read
  floor (~10 ns p50).** The seqlock's writer latency remains flat as readers
  increase, and both primitives are implemented without `unsafe` and verified
  with loom.

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

The repository is organized as five crates in a strictly acyclic dependency
graph rooted at the frozen `book` crate (the sans-IO core):: `book` (sans-IO core) ←
`feed`, `sync`, `engine`, `bench`. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Build and test

```sh
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Model-check the lock-free primitives' memory ordering (separate, slow).
RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_seqlock --test loom_ring --release
```

Benchmark results are hardware-dependent by design. Exact reproduction commands,
measurement methodology, and per-claim provenance are documented in
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md), with profiling captures in
[`docs/PROFILING.md`](docs/PROFILING.md).

## Documentation

- [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) — consolidated benchmark writeup
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design writeup and crate DAG
- [`docs/PROFILING.md`](docs/PROFILING.md) — microarchitecture analysis

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

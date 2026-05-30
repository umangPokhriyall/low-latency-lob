## 1. Project ledger

| # | Project | Role | Status |
|---|---|---|---|
| 1 | **Rust-Tcp-Server** | control-plane reactor + I/O methodology | ✅ Complete (Phases 0–2, DoD verified, telemetry committed) |
| 2 | **Web3-Terminal → low-latency market-data engine** | lock-free hot path + profiling discipline | ⏳ Next |
| 3 | **Stream-hive** (decentralized transcoding) | untrusted-worker isolation + verification + scheduler | ◻ Pending |
| 4 | **solana-mpc-kit** | secret hygiene primitive (threshold sig) | ◻ Pending (reframe + harden) |
| 5 | **Coingate** | idempotency / exactly-once primitive | ◻ Pending (freeze) |
| ★ | **microVM agent sandbox** (flagship) | the assembly of all of the above | ◻ Architecture stage |

## 2. Rust-Tcp-Server — completed-state record

**What it is:** 11 TCP server concurrency models (iterative → forking → preforked → thread-per-conn → thread-pool → poll → epoll-lt → epoll-et → event-loop → multireactor → io-uring) behind one `Server` trait, on a sans-IO `core`.

**Architecture (load this — it is the template):** `core` (sans-IO protocol: `RequestParser`, `Response::encode`, `Connection`/`ConnAction` state machine — FROZEN, drove all 11 models unmodified) → `sys` (raw OS I/O: epoll, poll, io_uring, affinity, ConnTable) → `reactor` (the event-loop assembly, reused by multireactor) → `models` (one strategy each). Open-loop, coordinated-omission-correct load generator.

**Headline telemetry (fill from `bench/results/`):**
- Top Throughput: 50,000 RPS Served at C=8000 (Sustained by multireactor and epoll-et).
- io_uring vs epoll-et syscalls/req: 4.024 down to 2.015 per request ($2.0\times$ efficiency win).  
- C10K Cap: Capped safely at 8,000 connections due to host CommitLimit virtual memory overcommit safety blocks. thread-per-conn crashed at 4,608 threads with an EAGAIN loadgen-error while ballooning to 121 MB RSS.
- multireactor Scaling Factor: Flat at $1.0\times$ throughput cap below saturation, but dropped median latency from 376,575 $\mu$s to 100 $\mu$s ($3,700\times$ drop) by scaling from 1 to 2 workers
- io_uring Verdict: Single-ring architecture saturated at 11,270 RPS under extreme edge-triggered concurrent bursts due to completion queue ring buffer saturation, highlighting the need for multi-reactor pinned configurations.

**Key decisions made (carry forward):** sans-IO boundary; no async runtime / no tokio; raw `io-uring` crate, purpose-built (multishot accept + provided buffer rings + batched submit); shared-nothing `SO_REUSEPORT` over single-acceptor+handoff; `Vec` header store over `HashMap`.

**Artifacts:** `docs/BENCHMARKS.md`, `README.md` (pinned-ready), `docs/ARCHITECTURE.md`, `docs/x-thread.md`. Distribution status: {FILL: published? X posted? HN posted?}

## 3. Synergy map — what each repo feeds the flagship

- **Rust-Tcp-Server** → sandbox API **control plane** (acceptor-free SO_REUSEPORT reactors) + host↔guest **I/O multiplexer** (epoll-ET / io_uring loop) + the **benchmark methodology** for cold-start profiling.
- **Web3-Terminal** → the **fast output-streaming path** (SPMC ring buffer, seqlock) + the **latency obsession + top-down profiling** discipline applied to sandbox cold-start.
- **Stream-hive** → the **untrusted-code isolation model**, **output verification** (probabilistic spot-check), and the **orchestrator scheduler**.
- **solana-mpc-kit** → **secret hygiene** (zeroization, constant-time) for secrets transiting a sandbox.
- **Coingate** → **idempotent / exactly-once** job-submission API semantics.

By the time the flagship starts, every component has been built and measured in isolation. The flagship is assembly, not invention.

### 4. Remaining-project seeds — Web3-Terminal Focus

**Web3-Terminal (Repo 2).** Rebuild the dead/thin repo as a **low-latency market-data engine** — strip the "web3/terminal" framing entirely; it is a pure systems artifact. Primitives, straight from the David Gross / Jane Street notes: limit order book as `BTreeMap` → sorted `Vec` → reverse-ordered `Vec` + linear search, each step benchmarked with the *interior latency distribution* plotted; a **seqlock** for snapshot state; an **SPMC lock-free ring buffer** for the event stream (bounded, cache-line-aligned, reserve-N-bytes optimization); a **top-down microarchitecture** profiling writeup (bad-speculation → branchless story). This is the highest elite-signal artifact of the five. Synergy: the SPMC/seqlock hot path is the sandbox's output multiplexer; the profiling discipline is how we'll attack cold-start.



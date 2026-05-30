# kickoff-brief.md — Low-Latency Market-Data Engine (Repo 2)

*Authoritative kickoff brief. Subordinate to `NORTH-STAR.md`; supersedes the legacy `Web3-Terminal` repo and both dated design inputs. Phase specs live in `docs/specs/phaseN-spec.md` and are written only when reached. Numbers in this repo come from `bench/results/`, never from prose.*

---

## 0. One-paragraph thesis

A single-symbol limit-order-book hot path, built sans-IO, where **one frozen `core` abstraction drives four book implementations** (BTreeMap → sorted Vec → reverse-sorted Vec + linear scan → flat price-tick array), each measured against the same deterministic event corpus with **interior latency distributions** plotted, the crossover identified honestly, and the hot apply-loop torn down at the **microarchitecture level** (IPC, branch-miss, LLC-miss; a bad-speculation → branchless story). State is published to many readers via a **seqlock**; derived events fan out via a **cache-line-aligned SPMC ring buffer**. There is no exchange, no arbitrage, no UI, no Redis, no async runtime in the measured path. The market-data framing is the vehicle; the LOB shootout + the two concurrency primitives + the profiling writeup are the deliverable. These three become the flagship sandbox's host↔guest output multiplexer and its cold-start profiling methodology.

---

## 1. Refactor decision table — preserve / drop / rebuild

The legacy repo (`collector`, `arbitrage`, `arb_v1`; ~3,856 LOC; 4 commits; zero tests; zero benchmarks; zero docs; **no order book exists**) is overwhelmingly a rebuild. Calls are final.

### DROP (delete from history's relevance — do not port)
| Item | Reason |
|---|---|
| **Redis Streams as IPC** | Hot path round-tripping a network daemon. Latency in ms RTTs, not ns cache misses. Antithetical to the entire thesis. |
| **`f64` price/qty/bid/ask** | Float in a hot LOB = correctness smell + no branchless comparison story + no tick lattice. Disqualifying on sight. |
| **tokio `full`, `Arc<Mutex>`, `DashMap`, per-stream `spawn`** | Allocation-heavy, lock-heavy, runtime-heavy. Banned from the measured path (Rust-Tcp-Server precedent: no async runtime). |
| **`arbitrage/` + `arb_v1/` binaries** | Trading logic, not systems work. Not falsifiable. Gone. |
| **Multi-exchange fan-in (binance/binance2/bybit/kraken/coinbase/hyperliquid)** | Breadth ≠ depth. Six half-adapters signal a tutorial-follower. One book, measured to the cache line, signals an engineer. |
| **The detector zoo** (`CROWDED_LONG`, `LIQUIDATION_CASCADE`, `BASIS_DIVERGENCE`, `GLOBAL_FAIR_VALUE_DIVERGENCE`, funding/OI/liquidation "leverage microscope") | Architecture astrology. A taxonomy of trading opinions, none falsifiable. |
| **Aggregator "pricing truth" layer, Execution engine, one-click strategies, whale tracking, UI/terminal framing** | Product fluff. Every noun is a feature, not a primitive. |
| **Live WebSocket feed in the benchmark path** | Unreproducible. Cannot replay a flash crash; coordinated omission makes live numbers a lie. A benchmark you can't replay is not a benchmark. |

### PRESERVE (ideas, not code — almost nothing executable survives)
| Item | How it survives |
|---|---|
| Binance WS message-format knowledge | Reused *only* inside the quarantined one-shot `recorder` (Phase 3), never in the measured path. |
| Single-writer symbol ownership (from Input 2) | Correct. Becomes the `engine` producer thread owning one book. |
| Events-vs-state separation; seqlock-for-readers; SPMC fan-out; writer-never-blocks (from Input 2) | The genuinely correct David Gross spine. Becomes `sync` + `engine`. |
| The one keeper sentence from the mentor transcript | *"reading prices really fast is the interesting challenge."* That is the entire mandate; everything else in Input 1 is pruned. |

### REBUILD (the actual work — net-new)
| Item | Note |
|---|---|
| **Fixed-point tick types** `Px(i64)`, `Qty(i64)` | Frozen in `core`. The tick lattice is the foundation of every branchless comparison. |
| **The limit order book itself** | Does not exist in the legacy repo. Four implementations behind one trait. This is the centerpiece. |
| **Seqlock snapshot cell** | Net-new. Crown-jewel primitive #1. |
| **SPMC cache-line-aligned ring buffer** | Net-new. Crown-jewel primitive #2. |
| **Coordinated-omission-correct open-loop bench harness** | Net-new. Reuse the Rust-Tcp-Server loadgen methodology. |
| **Microarchitecture profiling writeup** | Net-new. The highest-signal artifact. |

---

## 2. High-signal primitives a Principal Engineer evaluates

These are the three things a senior reader will actually look at. Each must come with a number and an honest story.

### 2.1 The book data-structure shootout — `BTreeMap` vs `Sorted Vec` vs `Reverse-sorted Vec + linear scan` vs `Flat price-tick array`

One `OrderBook` trait, four implementations, identical event sequence, plotted interior latency distribution per variant. The signal is **not** "I know what a BTreeMap is" — it's "I measured the crossover and told the truth about it."

- **`BTreeMap<Px, Level>`** — the naive baseline. Pointer-chasing, per-node heap allocation, O(log n) with bad cache constants. Expect it to lose at small/medium book depth and only justify itself at large, sparse books.
- **`Sorted Vec<(Px, Level)>`** — contiguous, binary-search lookup (cache-friendly but branchy), O(n) insert shift. The shift cost vs the cache win is the tension to measure.
- **`Reverse-sorted Vec + linear scan`** — the counterintuitive contender. Hot end (best levels) at the front; linear scan is branch-predictor-friendly and hardware-prefetchable; real books concentrate activity at the top of book. Expect this to win for realistic depths.
- **`Flat array indexed by price tick`** — the real-LOB endgame for dense books: O(1) update, no search, perfectly cache-resident, branchless. The cost is memory for sparse/wide books. Walking the shootout all the way to this answer is the free signal upgrade — a Principal Engineer expects the author to know it exists and to show *where* it dominates and where it wastes memory.

**The deliverable is the crossover plot + the honest verdict**, e.g. "Vec-linear dominates below N≈X levels; the flat array dominates for dense books at the cost of Y MB; BTreeMap never wins in our regime." An honest negative result with a profile is elite signal. A claimed win without the distribution is worthless.

### 2.2 Seqlock snapshot cell — single-writer / many-reader, lock-free reads

Version-counter protocol: writer increments to odd (write in progress), writes payload, increments to even; readers snapshot the version, read, re-read the version, retry on mismatch or odd. The signal is **correct memory ordering** (writer: `Release` on the closing version bump; reader: `Acquire` on both reads; `fence` where the platform needs it) and **no torn reads under stress**.
- Falsifiable claim required: read-latency distribution under concurrent write contention, and a stress test (millions of iterations, multi-thread) that proves zero torn reads.

### 2.3 SPMC cache-line-aligned ring buffer — bounded, single-producer, many independent consumers

Bounded power-of-two capacity; `#[repr(align(64))]` slots (or explicit padding) to eliminate false sharing; per-slot sequence numbers (Vyukov-style) so producer and consumers never share a hot cache line; a **reserve-N-bytes** API for variable-length payloads; single producer (no CAS contention on the write side); consumers fully independent (each tracks its own read cursor). The signal: **provably false-sharing-free** (padding verified; ideally `perf c2c` shows no inter-core contention on hot lines), **no lost/duplicated/torn entries under stress**, and a throughput + tail-latency number.

---

## 3. Definition of Done + microarchitectural profiling criteria

A hard DoD, mirroring the Rust-Tcp-Server DoD culture. The repo is **not** done until every box is checked.

**Working system behind a clean abstraction**
- [ ] Four `OrderBook` implementations behind one frozen trait.
- [ ] **Differential correctness oracle**: all four produce byte-identical book state on the same event sequence. (This is both the correctness proof and the "one abstraction, many implementations" demonstration.)

**Reproducible benchmark with committed numbers**
- [ ] Committed deterministic event corpus (`feed/corpus/`) + synthetic generator (steady / burst / flash-crash profiles).
- [ ] Open-loop, **coordinated-omission-correct** harness (reuse Rust-Tcp-Server loadgen methodology).
- [ ] **Interior latency distributions** per book variant: p50 / p99 / p99.9 + full histogram, committed to `bench/results/`.
- [ ] The BTreeMap/Vec/flat-array **crossover plotted**, with the honest verdict written down.

**Microarchitectural teardown (the highest-signal artifact)**
- [ ] `perf stat` on the hot apply-loop, committed: **IPC, branch-miss-rate, LLC-miss-rate** for at least the winning and a losing book variant.
- [ ] At least one **branchless optimization** with before/after numbers (e.g. branchless best-bid/ask update, or branch-free price-to-index mapping for the flat array).
- [ ] The **bad-speculation → branchless narrative** written in `docs/PROFILING.md` (the David Gross top-down story: which counter pointed where, what the rewrite did to it).
- [ ] Seqlock: read-latency-under-contention number + zero-torn-read stress proof.
- [ ] SPMC ring: throughput + tail latency + false-sharing-free proof + zero-loss stress proof.

**Honesty, packaging, ownership**
- [ ] Teardown writeup states where each structure lost, where it won, what surprised us. No marketing language.
- [ ] `docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`, 60-second-graspable `README.md`, `docs/x-thread.md`.
- [ ] **Self-audit passed**: I can re-derive, from memory, (a) the LOB crossover and *why* it falls where it does, and (b) the seqlock memory-ordering argument. If I can't explain it, I don't own it, and it can't support the flagship. Generation must never outrun comprehension.

---

## 4. Synergy to the microVM flagship sandbox

Every primitive here is a sandbox component, built and measured in isolation first. The flagship is assembly, not invention.

- **SPMC cache-aligned ring buffer → the host↔guest output multiplexer.** The sandbox must stream a guest's stdout/stderr/event log to many readers (the orchestrator, a log sink, a live viewer) without the guest-facing writer ever blocking. That is *exactly* a single-producer / many-independent-consumer ring. The market-data engine's producer (book → derived events) is the rehearsal for the sandbox producer (guest → output events).
- **Seqlock snapshot cell → the sandbox's live-state read path.** The orchestrator polls a microVM's current state (status, resource counters, top-of-output) at high frequency without ever stalling the VM's writer. Same single-writer/many-reader seqlock, same memory-ordering argument.
- **Interior-latency-distribution + top-down profiling discipline → the cold-start attack.** The methodology built here — coordinated-omission-correct measurement, `perf stat` counter-led optimization, the bad-speculation→branchless story — is precisely how we'll profile and shave microVM cold-start. The LOB hot loop is the practice target; cold-start is the real one.

By the time the flagship begins, the output multiplexer, the state read path, and the profiling methodology are already built, measured, and committed.

---

## 5. Phase breakdown — autonomous Claude Code sessions

Spec-driven, agent-executed. One session = one deliverable, ends **build + clippy + test green → commit → STOP**. Each phase gets its own `docs/specs/phaseN-spec.md`, written only when reached. A `CLAUDE.md` guardrail forbids touching future phases. Load-bearing phases (the elite-signal ones) flagged ★.

**Phase 0 — Demolition + workspace skeleton + `CLAUDE.md`.**
Delete `arbitrage/`, `arb_v1/`, all exchange adapters except the Binance parsing kept aside for the recorder, all Redis, all f64 types. Stand up the new workspace: `core`, `sync`, `feed`, `bench`, `engine` as compiling stubs. Add `CLAUDE.md` guardrail. Define `Px(i64)` / `Qty(i64)` tick types in `core`. → green + commit + STOP.

**Phase 1 — `core`: event model + `OrderBook` trait + BTreeMap baseline.** ★
Define `BookEvent` (Add / Cancel / Modify / Trade), the `OrderBook` trait (`apply`, `best_bid`, `best_ask`, `top_n`), and the `BTreeMap` baseline impl. Unit tests against a hand-verified event sequence. → green + commit + STOP.

**Phase 2 — `core`: impls 2 & 3 (sorted Vec, reverse-sorted Vec + linear scan) + differential oracle. FREEZE core.** ★
Two more impls behind the same trait. Differential test: all three produce identical state on the same sequence. After this passes, **`core` is frozen** — it must drive every later harness unmodified, exactly as the Rust-Tcp-Server `core` drove all 11 models. → green + commit + STOP.

**Phase 3 — `feed`: deterministic replay corpus + synthetic generator + quarantined Binance recorder.**
The `recorder` (tokio, one exchange, off the measured path) captures one real session to a committed binary corpus. The synthetic generator emits controllable load profiles (steady / burst / flash-crash). The replay iterator is the deterministic, allocation-free event source the harness consumes. → green + commit + STOP.

**Phase 4 — `bench`: open-loop, coordinated-omission-correct harness + interior latency distributions.** ★
Drive each book variant from the corpus at fixed arrival rates; record per-`apply` latency into an HdrHistogram; emit p50/p99/p99.9 + full distribution to `bench/results/`. Produce the BTreeMap-vs-Vec crossover plot. → commit numbers + STOP.

**Phase 5 — `core`: flat price-tick array impl + crossover close-out.** ★
The fourth book impl (O(1) flat array). Re-run the Phase 4 harness across all four. Write the honest crossover verdict (where each wins, the memory cost of the flat array). → commit numbers + STOP.

**Phase 6 — `sync`: seqlock snapshot cell.** ★
Single-writer/many-reader seqlock over the top-of-book snapshot. Memory-ordering correct. Stress test: zero torn reads over millions of multi-thread iterations. Benchmark: read latency under concurrent writes. → green + commit + STOP.

**Phase 7 — `sync`: SPMC cache-line-aligned ring buffer.** ★
Bounded power-of-two, padded slots, per-slot sequence numbers, reserve-N-bytes API, single producer / many independent consumers, lock-free. Stress test: no lost/duplicated/torn entries. Benchmark: throughput + per-op tail latency; prove false-sharing-free. → commit numbers + STOP.

**Phase 8 — `engine`: assembly + pinned end-to-end hot path.**
Producer thread (pinned): replay → apply to frozen book → publish seqlock snapshot → push derived event to SPMC ring. Consumer threads pinned. End-to-end latency distribution under load. → commit numbers + STOP.

**Phase 9 — Microarchitecture profiling writeup.** ★
`perf stat` IPC / branch-miss / LLC-miss on the hot apply-loop; identify bad speculation; implement ≥1 branchless rewrite; before/after numbers; write `docs/PROFILING.md` (the top-down narrative). → commit + STOP.

**Phase 10 — DoD close-out + distribution-ready.**
`docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`, 60-second `README.md`, `docs/x-thread.md`. Pass the self-audit. Then — and only then — distribution. → commit + STOP.

---

## 6. Non-negotiables carried from NORTH-STAR

- Sans-IO: `core` is pure logic, frozen after Phase 2, drives every variant unmodified.
- Measure, never guess: every claim traces to a committed file in `bench/results/`.
- Distributions, not averages: p99/p99.9 + full interior histogram, coordinated-omission-correct.
- Mechanical sympathy: know the cost of the cache miss, the branch miss, the false-sharing line. Pin threads.
- Honesty is the signal: publish where structures lost. A profiled negative result beats a fake win.
- Scope discipline: one session, one deliverable, green + commit + STOP. Future phases are off-limits until reached.
- Voice: numbers and architecture carry the weight. No marketing language, ever.

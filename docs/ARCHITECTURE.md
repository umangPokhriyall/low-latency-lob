# ARCHITECTURE — a low-latency limit-order-book engine

This is the design writeup: what the system is, how it is partitioned, and why each
boundary sits where it does. The performance numbers live in
[`BENCHMARKS.md`](BENCHMARKS.md); the microarchitecture teardown in
[`PROFILING.md`](PROFILING.md). This document is the structure those numbers measure.

---

## 1. Thesis

Four design commitments shape every boundary in this repo. They are stated here and shown
in the sections that follow.

- **Sans-IO discipline.** The order-book logic (`book`) knows nothing about where events
  come from or where snapshots go. It is a pure state machine over integer events: feed it
  an event, it mutates; ask it for the top of book, it answers. No sockets, no async, no
  allocation in the hot path, no dependencies. The *what* is separated from the *how*, so
  one frozen core drives every harness, every primitive, and the assembled engine
  unchanged.
- **Measure, never guess.** No design choice between implementations is argued from
  intuition. Four order-book structures sit behind one trait precisely so they can be
  measured against each other under coordinated-omission-correct load; the verdict
  (§3, [`BENCHMARKS.md`](BENCHMARKS.md)) is a number, and it inverted the intuition.
- **One abstraction, many implementations.** The `OrderBook` trait is the product; the
  four structures are instances. The differential oracle proves them observationally
  identical, so swapping one for another is sound, and no logic is copy-pasted across them.
- **Honesty is the signal.** The writeups feature what underperformed — the flat array
  collapsing on real data, a predicted bottleneck refuted, a throughput decline the
  original hypothesis did not predict, a PMU-free prediction later *confirmed* against
  native AMD Zen 4 counters on rented bare metal. An honest negative result with a profile
  is the elite signal; this architecture is built to surface them, not bury them.

---

## 2. The crate DAG

Five crates, one acyclic dependency graph, with `book` as the frozen root every other
crate depends on and nothing depends back into:

```
                ┌──────────────────────────────┐
                │   book  (frozen sans-IO core) │
                │   OrderBook trait + 4 impls   │
                │   integer ticks, no I/O,      │
                │   no async, no deps           │
                └──────────────────────────────┘
                  ▲        ▲        ▲        ▲
                  │        │        │        │
        ┌─────────┘   ┌────┘        └────┐   └─────────┐
        │             │                  │             │
   ┌─────────┐   ┌─────────┐        ┌─────────┐   ┌──────────┐
   │  feed   │   │  sync   │        │ engine  │   │  bench   │
   │ corpus, │   │ seqlock,│        │ pinned  │   │ CO-correct
   │ replay, │   │ SPMC    │        │ pipeline│   │ harness  │
   │ recorder│   │ ring    │        │ assembly│   │ + plots  │
   └─────────┘   └─────────┘        └─────────┘   └──────────┘
     (recorder        │                  ▲   ▲         │
      async behind ───┘──────────────────┘   │         │
      a feature)   engine = book + sync       └─────────┘
                                          bench depends on all four
```

What each crate owns:

| crate | owns | depends on |
|---|---|---|
| `book` | the `OrderBook` trait, the four implementations, the `BookEvent` model, the `Px`/`Qty` integer tick types, the differential oracle | nothing (no third-party deps) |
| `feed` | the binary corpus format, the deterministic replay iterator, the synthetic load-profile generator, and the **quarantined** live recorder | `book` |
| `sync` | the two lock-free primitives — a single-writer/many-reader seqlock and a single-producer/many-consumer broadcast ring | nothing (std atomics; `loom` under `cfg(loom)`) |
| `engine` | the assembly: the pinned `book → seqlock → ring → consumers` pipeline and the `BookEvent ↔ [u64; 5]` packing | `book`, `sync` |
| `bench` | the coordinated-omission-correct harness, every benchmark, the profiling scaffolding, and the SVG plotter | `book`, `sync`, `feed`, `engine` |

**The async / float quarantine.** Two things are banned from every measured path and
permitted in exactly one place. *Floats* exist only inside the `recorder` binary, at the
exchange-string parse edge, converted to integer ticks before anything reaches the corpus.
*Async* (tokio and the TLS/websocket stack) is pulled in only by `feed`'s `recorder`
feature, which gates the `recorder` binary; the default `feed` tree links none of it
(`cargo tree -p feed` proves it) and the replay path, the book, the primitives, the
engine, and the harness are entirely synchronous. The corpus is the membrane: everything
upstream of it may touch floats and async; nothing downstream ever does.

---

## 3. The sans-IO `book`

One trait, four implementations:

```rust
pub trait OrderBook: Default {
    fn apply(&mut self, ev: &BookEvent);
    fn best_bid(&self) -> Option<(Px, Qty)>;
    fn best_ask(&self) -> Option<(Px, Qty)>;
    fn top_n(&self, side: Side, out: &mut [(Px, Qty)]) -> usize;
    fn depth(&self, side: Side) -> usize;
    fn last_trade(&self) -> Option<(Px, Qty, Side)>;
}
```

The four implementations differ only in how they store levels and locate one:

- **`SortedVecBook`** — a contiguous price-sorted `Vec` per side; locate by binary search.
- **`BTreeBook`** — a `BTreeMap` per side; locate by node descent (a pointer chase).
- **`RevVecBook`** — a `Vec` ordered best-first; locate by linear scan from the top.
- **`FlatBook`** — one dense array per side spanning the price range; locate by direct
  index (`bid_qty[px - base]`).

`Px(i64)` and `Qty(i64)` are integer ticks and lots — there is no `f64` anywhere in the
crate. The hot `apply` path allocates nothing (storage is sized at setup), takes no locks,
and does no I/O.

**The differential oracle (`book/tests/oracle.rs`)** is the load-bearing correctness
artifact. It drives all implementations through the same randomized and adversarial event
streams (negative and extreme prices, crossed books, remove-absent, clear-then-rebuild,
realloc churn) and asserts they produce identical observable state — best bid/ask, the
full `top_n` ladder, depth, last trade — at every step. `BTreeBook`, `SortedVecBook`, and
`RevVecBook` agree everywhere; `FlatBook` joins the four-way check on the bounded price
band that is its defined domain. Because the four are proven observationally identical, the
benchmark differences are pure performance, not behavior.

**The freeze.** `book` was frozen after the oracle passed (git tag `book-v1-frozen`); its
six source files carry a "FROZEN — do not modify" header. New implementations are additive
(new file, new export, extend the oracle), never edits to the trait or the existing
structures. From that point the frozen core drove the Phase 3 feed, the Phase 4–5
benchmark sweeps, the Phase 8 engine, and the Phase 9 profiling harness **unmodified** —
the same discipline that, in an earlier project, let one frozen protocol core drive eleven
server I/O models from blocking to io_uring without a line changed. A core you cannot stop
editing is a core you have not actually specified.

---

## 4. The corpus boundary

`feed` turns events into a replayable artifact and back:

- **The corpus format** is a flat binary of fixed 40-byte `BookEvent` records behind a
  32-byte header (`MDFEED\0\0` magic, version, record size, count — `feed/src/corpus.rs`).
  It is tick-space only: no floats, no heap strings, no framing. Replay is a zero-copy
  iterator over the mapped bytes.
- **The synthetic generator** produces deterministic steady / burst / flash-crash profiles
  from a seeded `SplitMix64`, so a load shape is reproducible bit-for-bit across runs and
  hosts.
- **The recorder** (the only async, float-touching component) connects to a live exchange
  feed, and at the parse edge converts each decimal price/size string to integer ticks
  with **exact `i128` arithmetic** — `scale_to_int` multiplies through a power-of-ten
  scale and divides by the symbol's tick/step size with no `f64` ever constructed
  (`feed/src/bin/recorder.rs`). A value that does not divide evenly is a typed error, not a
  silent rounding. The recorder's job ends at "exchange float-string → `Px`/`Qty`
  integer"; from there the corpus carries only integers.

Why this boundary is load-bearing: **a replayable corpus is the precondition for a
falsifiable benchmark.** Because every run replays the identical committed byte stream (the
corpora are fingerprinted in `env.json`), a latency number is reproducible up to machine
timing noise and a reviewer can re-derive it. If the benchmark consumed a live feed, no
number could be checked twice. The quarantine guarantees the measured path never pays for
async runtime or float conversion the way the recorder does — so the "no async / no float
in the measured path" claim is structural, not aspirational.

---

## 5. The lock-free primitives (`sync`)

Two primitives carry the concurrency, and a single decision shapes both: **zero
`unsafe`.**

### 5.1 The seqlock — single-writer / many-reader snapshot

The seqlock publishes the top-of-book (`TopOfBook`: bid/ask price+qty plus a monotonic
`stamp`) under a version counter. The single writer's `store` increments `seq` to odd
(write in progress), writes the payload, then increments to even with `Release`. A reader's
`load` takes an `Acquire` snapshot of `seq`, reads the payload, then re-reads `seq` behind
an `Acquire` fence; if `seq` is even and unchanged, no write straddled the read and the
snapshot is consistent — otherwise it retries (`sync/src/seqlock.rs`). The ordering is
carried entirely by the `seq` counter: payload words are `Relaxed`, and the
`Release`/`Acquire` pairings (a writer's even-marker happens-before the reader's confirming
load) make a torn snapshot **detectable and discarded**, never returned. The writer is
**wait-free** — it never inspects or waits on a reader — which is the property a
`Mutex<TopOfBook>` cannot offer and the reason reader count does not tax writer latency
(measured in [`BENCHMARKS.md`](BENCHMARKS.md) §4.1).

### 5.2 The SPMC broadcast ring

The ring is a single-producer / many-independent-consumer broadcast bus. Each slot holds
its payload as `[AtomicU64; W]` plus an `AtomicU64` **stamp** encoding the position and a
WRITING bit. The producer marks the slot busy (`Release` fence), overwrites the words
(`Relaxed`), publishes the stamp at the new position (`Release`), then advances the write
cursor (`Release`) — `sync/src/ring.rs`. A consumer reads the cursor (`Acquire`), reads the
slot's words, then re-checks the stamp behind an `Acquire` fence: if the stamp still names
its position the record is clean; if the producer has moved on, it is an **overrun** and
the consumer resyncs to the oldest resident record. The per-slot stamp is what lets a
broadcast bus detect lapping without coordinating with consumers — the producer **never
blocks** (it overwrites on wrap), so no consumer can stall it. Slots are
`#[repr(align(64))]`, one per cache line, with the write cursor isolated on its own line, a
static `size_of::<Slot<W>>() % 64 == 0` assertion enforcing it. The EPYC re-run's 64 B
cache line validates the alignment, and `perf c2c` directly measured the intended effect:
no false-sharing HITM on the aligned slots, and the only true-sharing HITM on the single
shared write cursor ([`BENCHMARKS.md`](BENCHMARKS.md) §4.2).

### 5.3 The zero-`unsafe` decision

The textbook seqlock and the textbook Vyukov broadcast slot both use `UnsafeCell` plus a
non-atomic `memcpy` of the payload under the version guard. **Under Rust's memory model
that is a data race and therefore undefined behavior** — two threads access the same
non-atomic bytes without a happens-before edge, even though the version counter will later
discard a torn read. The discard does not retroactively make the race defined. The sound
alternative is to make the payload itself atomic: store each word as an `AtomicU64`
accessed `Relaxed`, and carry ordering with the version/stamp counter and `Acquire`/
`Release` fences. A torn read then reads *stale-but-valid* atomic words and is detected by
the counter check — defined behavior throughout. Both primitives took this route, the whole
workspace is `#![forbid(unsafe_code)]`, and the unsafe budget went unspent. Atomics, not
`UnsafeCell`, are the correct tool for concurrent shared mutation here; the
compiler-enforced absence of `unsafe` is a capstone, not a constraint worked around.

---

## 6. The engine assembly

`engine` wires the frozen book and the two primitives into one pinned pipeline. The
producer step is one function — apply, publish, broadcast — and the consumers are
independent:

```
  corpus replay (feed)
        │  BookEvent (integer ticks)
        ▼
  ┌─────────────────────── producer thread (pinned, core 0) ───────────────────────┐
  │  EngineProducer::process(ev):                                                   │
  │     book.apply(ev)                      // frozen OrderBook: mutate state       │
  │     top = (best_bid, best_ask, stamp)                                           │
  │     seqlock.store(top)        ──────────────►  SeqLock<TopOfBook>  (latest snap)│
  │     ring.push(pack(ev))       ──────────────►  SpmcRing<5>         (full stream)│
  └────────────────────────────────────────────────────────────────────────────────┘
                                    │ seqlock           │ ring (broadcast)
                  ┌─────────────────┼───────────────────┼─────────────────┐
                  ▼                 ▼                   ▼                  ▼
            consumer 0         consumer 1          consumer 2         consumer K-1
          (pinned core 1)    (pinned core 2)     (pinned core 3)    (pinned core K)
          poll() loop:
            Item(ev)   -> unpack, light derived work (mid-price), record latency
            Overrun{n} -> resync derived state from seqlock snapshot, skip n
            Empty      -> spin until producer advances
```

`EngineProducer::process` applies the event to the frozen book, stores the new
top-of-book to the seqlock under a fresh stamp, and pushes the packed event to the ring —
no allocation, no lock (`engine/src/lib.rs`). Each `EngineConsumer::poll` drains the ring
from its private cursor. The packing (`engine/src/pack.rs`) is explicit integer
bit-manipulation — `size_of::<BookEvent>() == 40 == 5 * 8`, so an event serializes into
exactly five `u64` words — and the consumer **validates the discriminants at unpack**, so a
corrupt word becomes a typed error, never undefined behavior, mirroring the recorder's
validation at the float-string edge.

**The overrun → resync composition** is the point where the two primitives compose. The
ring guarantees the *full* stream but may lap a slow consumer; the seqlock guarantees the
*latest* state and never laps. So when a consumer's `poll` returns `Overrun { skipped }`,
it does not lose correctness — it loads a consistent snapshot from the seqlock, rebases its
derived state (the mid-price) from that snapshot, and resumes from the ring's oldest
resident record. A consumer that falls behind under a burst self-heals from the
always-current seqlock instead of corrupting or stalling. Lossy-but-detected on the ring,
backed by always-current on the seqlock, is the deliberate contract — and the producer
stays wait-free throughout (it never waits on any consumer).

---

## 7. Measurement methodology

`bench` is the coordinated-omission-correct harness, and its discipline is what makes the
numbers trustworthy:

- **Service time vs response time are never blurred.** Service-time benchmarks (the
  per-op service sweep, the read path, whole-corpus throughput, the primitive
  micro-benchmarks) time the operation with no arrival process. Response-time benchmarks
  (sustained load, end-to-end) run open-loop and stamp each event's scheduled arrival, then
  record `completion − scheduled_arrival` — so a system that falls behind charges the
  backlog to every late event's tail. The CO correction is itself unit-tested
  (`co_correct_records_accumulating_lag`).
- **Hygiene on every cell:** inputs and outputs wrapped in `black_box`; the pinned core
  warmed before recording; ≥1,000,000 samples per service cell (10,000,384 lookups per
  branch-experiment cell); the measured clock read-read floor (~10 ns on the EPYC re-run
  host; 7 ns on the archived laptop) reported and **never subtracted**; threads pinned; the
  CPU governor recorded (`performance` on the EPYC box, pinned to one CCD-0 core).
- **Every number is sourced.** Each benchmark writes a committed CSV under
  `bench/results/`, and the environment (CPU, caches, governor, kernel, rustc, pinned core,
  clock floor, corpus fingerprints) is captured in `env.json`. The writeups re-derive every
  headline from those CSVs and cite the file inline; nothing is computed by hand or
  invented. Plots are generated from the same CSVs by `bench plot`.

This is the measure-never-guess principle made mechanical: a claim that cannot point at a
committed file is not made.

---

## 8. Principles made concrete

Each NORTH-STAR engineering principle maps to a specific decision in this repo:

| principle | concrete decision |
|---|---|
| Sans-IO discipline | `book` has zero I/O, async, or dependencies; the same frozen core drives feed, bench, engine, and profiling unchanged (§3) |
| Measure, never guess | four implementations behind one trait, judged by `throughput.csv` — the verdict inverted the intuition (§3, [`BENCHMARKS.md`](BENCHMARKS.md) §3.2) |
| Distributions, not averages | every benchmark reports p50/p99/p99.9 + histograms (`.hgrm`); the CO-correct sustained/e2e tail is where saturation shows |
| Mechanical sympathy | thread pinning (producer on one CCD-0 core, readers across CCDs), cache-line-aligned ring slots (`align(64)`, validated by the EPYC 64 B line + `perf c2c`), the recorded clock floor (~10 ns EPYC), `target-cpu=native` for valid microarchitecture profiling |
| One abstraction, many impls | the `OrderBook` trait is the product; the differential oracle proves the four instances identical, so no logic is duplicated (§3) |
| Honesty is the signal | the real-data inversion, the SortedVec=memory-bound refutation (PMU-free-predicted, then confirmed at 50.5 % AMD backend-bound-memory / 0.1 % bad-spec), the ring's true-sharing throughput decline (now measured via `perf c2c`) are featured, not hidden (§1, [`BENCHMARKS.md`](BENCHMARKS.md) §7) |
| Simple and fast beats clever | the zero-`unsafe` atomic-payload seqlock/ring over the `UnsafeCell` + memcpy shortcut — sound and measured at the clock floor, no cleverness the telemetry did not justify (§5.3) |
| Scope discipline | phase specs + a `CLAUDE.md` guardrail; `book` frozen at `book-v1-frozen`; one session = one deliverable, ends green + commit |

---

## 9. General-purpose substrate

The seqlock and the SPMC broadcast ring are general-purpose, market-data-agnostic
concurrency primitives — a single-writer snapshot cell and a single-producer broadcast bus
reusable in any system that needs lock-free state publication and fan-out.

---

*Numbers: [`BENCHMARKS.md`](BENCHMARKS.md). Microarchitecture: [`PROFILING.md`](PROFILING.md).
Licensed `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`).*

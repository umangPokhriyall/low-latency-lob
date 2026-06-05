# engine — Phase 8 Specification: The Pinned End-to-End Assembly and Production-to-Consumption Latency

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md` … `docs/specs/phase7-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 8 spec.** The frozen `book` (four impls, sourced verdict), the deterministic `feed`, the loom-verified `sync` seqlock and SPMC broadcast ring, and the `bench` harness are all built. The workspace is `#![forbid(unsafe_code)]` end to end.
**Scope:** the `engine` crate — the first assembly of all parts into one pinned pipeline (feed → frozen book → seqlock snapshot → SPMC ring → independent consumers) — and the end-to-end, coordinated-omission-correct production-to-consumption latency study under load.
**Audience:** Claude Code. Authoritative. This phase is the rehearsal for the flagship's assembly; it invents nothing, it composes verified parts and measures the whole honestly.

---

## 1. Phase 8 in one paragraph

Every component has been built and measured in isolation; Phase 8 wires them into one hot path and measures what the composed system actually does. A single pinned producer thread replays a corpus, applies each event to the frozen book, publishes the new top-of-book to the seqlock, and broadcasts the event through the SPMC ring; pinned consumer threads each drain the ring independently, poll the seqlock, and resync from it when the producer laps them. The headline measurement is the **production-to-consumption latency distribution** — coordinated-omission-correct against the replay schedule — and the rate at which the whole pipeline saturates. The honest expectation, set by Phase 7, is that producer throughput degrades as consumers are added because every consumer polls the shared write cursor (true sharing, not false sharing); Phase 8's job is to assemble correctly, measure that reality on real and synthetic corpora, and report it without flattering the system — because this composed pipeline is the exact shape of the flagship's observability substrate, and an honest number here is what makes the flagship's design decisions falsifiable later.

### 1.1 What Phase 7 established (informs this phase)
- **True sharing on the write cursor** (`ring_bench.csv`): producer throughput falls with consumer count (38.7 / 20.8 / 15.4 Mev/s at K=1/2/4) because every consumer reads `write.v`. Latency stays flat (push p50 7–8 ns, recv 9–10 ns). This degradation is expected in the assembly and must be measured, not hidden.
- **The resync correctness lesson:** `resync` must re-load the write position freshly (Acquire); a stale snapshot let a cursor move backward (caught by stress, not loom). The engine's overrun handling relies on the now-correct `Consumer::resync`.
- **Zero-`unsafe` capstone:** all five crates `forbid(unsafe_code)`. `engine` keeps it.
- **Book verdict:** on the real BTCUSDT sample, `BTreeBook` leads (23.1 Mev/s); `FlatBook` collapses (recenter storm). The engine's headline run uses the **real-data champion**.

### 1.2 Frozen / reused
- `book` frozen and reused via its public API; the engine selects an impl by monomorphization (`Engine<B: OrderBook>`), never `dyn`.
- `feed` reused at default features (no async) for replay; `sync` reused as-is (no change to the verified primitives); `bench` reused for measurement (Benchmark 7) and gains a `bench → engine` dependency edge.
- `engine` owns the `BookEvent ↔ [u64; W]` packing that Phase 7 deferred (§3.1). No `book`/`feed`/`sync` source changes.

---

## 2. Architecture

### 2.1 The pipeline
```
feed::Corpus ──▶ [Producer thread, pinned core A]
                    for each event:
                      book.apply(&ev)                  // frozen book (B = monomorphized impl)
                      seqlock.store(top_of_book)        // publish consistent snapshot
                      ring.push(pack(&ev))              // broadcast the event stream
                 [Consumer thread k, pinned core B/C/…] (independent)
                    loop:
                      match ring.try_recv():
                        Item(rec)        -> unpack+validate, light derived work, record latency
                        Overrun{skipped} -> resync derived state from seqlock.load()
                        Empty            -> spin/yield
```

### 2.2 Engine = pipeline logic; bench = threads, pinning, timing
`engine` provides the **logic** of one producer step and one consumer step; it does not own threads, pinning, pacing, or measurement. `bench` (Benchmark 7) spawns and pins the threads, paces the replay, and records latencies with the Phase 4 harness. This keeps `engine` a clean, deterministic library and keeps all measurement in `bench` (consistent with Phases 4–7). A tiny `engine` demo binary (unpinned smoke run) is allowed for the README; it does no measurement.

### 2.3 Book impl selection
`Engine<B: OrderBook>` is generic and monomorphized. The **headline** configuration is `BTreeBook` (the Phase 5 real-data champion) on the real BTCUSDT corpus; the benchmark may also run other impls for contrast. No `dyn` in the hot path.

### 2.4 Composition: seqlock as the overrun resync point
A consumer that the producer laps receives `Recv::Overrun { skipped }`; it then loads the seqlock top-of-book and rebases its derived state from that consistent snapshot before resuming at the ring's oldest resident position. This is the Phase 7 §2.6 composition made concrete and is the engine's most important correctness behavior to demonstrate.

### 2.5 No `unsafe`
`engine` is `#![forbid(unsafe_code)]`. Packing is explicit integer bit-manipulation (no transmute); the pipeline uses only the safe public APIs of the verified primitives. The workspace-wide zero-`unsafe` invariant holds.

---

## 3. Engine logic (code stubs)

### 3.1 `engine/src/pack.rs` — `BookEvent ↔ [u64; W]` (engine-owned, explicit, validated)
```rust
use book::{BookEvent, EventKind, Px, Qty, Side};

pub const W: usize = 5; // size_of::<BookEvent>() == 40 == 5 * 8

#[must_use]
pub fn pack(ev: &BookEvent) -> [u64; W] {
    [
        ev.seq,
        ev.ts,
        ev.px.ticks() as u64,                 // i64 bit pattern
        ev.qty.lots() as u64,
        (ev.side as u64) | ((ev.kind as u64) << 8),
    ]
}

#[derive(Debug)]
pub enum UnpackError { BadSide(u64), BadKind(u64) }

pub fn unpack(rec: &[u64; W]) -> Result<BookEvent, UnpackError> {
    let side = match rec[4] & 0xFF {
        0 => Side::Bid, 1 => Side::Ask, b => return Err(UnpackError::BadSide(b)),
    };
    let kind = match (rec[4] >> 8) & 0xFF {
        0 => EventKind::Level, 1 => EventKind::Trade, 2 => EventKind::Clear,
        b => return Err(UnpackError::BadKind(b)),
    };
    Ok(BookEvent {
        seq: rec[0], ts: rec[1],
        px: Px(rec[2] as i64), qty: Qty(rec[3] as i64),
        side, kind,
    })
}
```
Discriminant validation at unpack mirrors the corpus boundary: the ring carries opaque words; the consumer validates at the edge. A round-trip unit test (`unpack(pack(ev)) == ev`) is required.

### 3.2 `engine/src/lib.rs` — producer & consumer logic
```rust
use std::sync::Arc;
use book::{OrderBook, Px, Qty, Side};
use sync::{Consumer, Producer, Recv, RingHandle, SeqLock, SpmcRing, TopOfBook};
use crate::pack::{pack, unpack, W};

pub struct Engine<B: OrderBook> { /* construction wiring */ _b: core::marker::PhantomData<B> }

pub struct EngineProducer<B: OrderBook> {
    book: B,
    top: Arc<SeqLock>,
    out: Producer<W>,
    stamp: u64,
}

impl<B: OrderBook> EngineProducer<B> {
    /// One producer step: apply, publish snapshot, broadcast. Hot path; no alloc.
    #[inline]
    pub fn process(&mut self, ev: &book::BookEvent) {
        self.book.apply(ev);
        let (bp, bq) = self.book.best_bid().unwrap_or((Px::ZERO, Qty::ZERO));
        let (ap, aq) = self.book.best_ask().unwrap_or((Px::ZERO, Qty::ZERO));
        self.stamp += 1;
        self.top.store(TopOfBook {
            bid_px: bp.ticks(), bid_qty: bq.lots(),
            ask_px: ap.ticks(), ask_qty: aq.lots(),
            stamp: self.stamp,
        });
        self.out.push(pack(ev));
    }
}

pub enum Observed { Event(book::BookEvent), Overrun { skipped: u64, snapshot: TopOfBook }, Idle }

pub struct EngineConsumer {
    inbox: Consumer<W>,
    top: Arc<SeqLock>,
    pub seen: u64,
    pub resyncs: u64,
    pub last_mid: i64,
}

impl EngineConsumer {
    /// One consumer step. Light derived work; resync from the seqlock on overrun.
    #[inline]
    pub fn poll(&mut self) -> Observed {
        match self.inbox.try_recv() {
            Recv::Item(rec) => {
                let ev = unpack(&rec).expect("engine producer emits valid events");
                self.seen += 1;
                Observed::Event(ev)
            }
            Recv::Overrun { skipped } => {
                let snap = self.top.load();           // resync from the consistent snapshot
                self.last_mid = (snap.bid_px + snap.ask_px) / 2;
                self.resyncs += 1;
                Observed::Overrun { skipped, snapshot: snap }
            }
            Recv::Empty => Observed::Idle,
        }
    }
}

impl<B: OrderBook> Engine<B> {
    /// Wire book + seqlock + ring; return the producer side and a handle that mints consumers.
    pub fn new(ring_capacity: usize) -> (EngineProducer<B>, EngineHandle) { todo!() }
}

pub struct EngineHandle { ring: RingHandle<W>, top: Arc<SeqLock> }
impl EngineHandle {
    pub fn consumer(&self) -> EngineConsumer { /* ring.consumer() + top.clone() */ todo!() }
}
```

---

## 4. End-to-end correctness test (`engine/tests/pipeline.rs`)

Deterministic, single-threaded driving where possible plus a small threaded run:
1. **Ordered, complete delivery (keeping-up consumers):** drive a known corpus through `process`; one or more `EngineConsumer`s drain concurrently fast enough to never be lapped (large ring, paced producer). Assert each consumer observes **every** event, in order, untorn (unpack succeeds; `seq`/`ts` strictly increasing as produced).
2. **Snapshot validity:** at quiescence, each consumer's last seqlock `load()` equals the producer's current top-of-book (`best_bid`/`best_ask` from the book). No torn snapshot ever observed.
3. **Overrun → resync:** a deliberately-stalled consumer against a small ring gets `Overrun`, resyncs from the seqlock (a valid snapshot), and resumes consistently; assert the resync count > 0 and post-resync delivery is ordered.
4. **Final-state consistency:** after the producer applies the whole corpus, its book's top-of-book matches an independently-computed expected top-of-book (replay determinism sanity).

---

## 5. End-to-end benchmark (`bench`, Benchmark 7)

Add `bench → engine` to `bench/Cargo.toml`. Add `bench/src/benches/e2e.rs` and an `e2e` subcommand, reusing the Phase 4 harness (clock, recorder, pinning, `black_box`, recorded clock floor) and the §3 methodology.

**Schedule & CO-correctness.** The producer thread paces the replay to a schedule and stamps each event's `ts` with its scheduled arrival (ns); the consumer computes `latency = clock.now_ns() - ev.ts`. Measuring against the **scheduled** arrival (not push time) makes this the coordinated-omission-correct **production-to-consumption** latency across the whole pipeline (apply + seqlock store + ring push + propagation + recv). Two schedules: real-arrival replay of `btcusdt-sample.mdf` (speed 1) and a synthetic fixed-rate sweep (`rate_eps` increasing to saturation).

**Setup.** Producer pinned to core A running `EngineProducer::<BTreeBook>::process` (headline; other impls optional); `K ∈ {1,2,4,8}` consumers pinned, each looping `poll` and recording end-to-end latency on `Observed::Event`, counting `Overrun`s. `black_box` the observed events.

**Measure & report.**
- **End-to-end latency** p50/p99/p99.9 vs `rate_eps`, per `K` (the headline distribution).
- **Pipeline saturation rate** per `K` (highest rate with bounded p99).
- **Producer throughput vs `K`** — expected to **decline** (Phase 7 true sharing on the write cursor); report it honestly and attribute it correctly (true sharing, not false sharing — the slots are `align(64)`-isolated).
- **Overrun rate** vs `K` and rate.

**`bench/results/e2e.csv`**
```
book,schedule,consumers,target_rate_eps,achieved_rate_eps,samples,clock_overhead_ns,e2e_p50_ns,e2e_p99_ns,e2e_p999_ns,e2e_max_ns,producer_mev_s,overrun_rate,saturated
```
Plots (cite the CSV): e2e p99 vs rate (per K); producer throughput vs K (the true-sharing curve). A short `bench/results/e2e.md` (interim, Writing-Standard-clean) reports the headline real-corpus end-to-end latency, the saturation rates, the producer-throughput-vs-K decline with its true-sharing attribution, and the overrun behaviour — sourced to the CSV, governor recorded.

---

## 6. The true-sharing reality (report it; do not engineer around it this phase)

The assembly inherits Phase 7's write-cursor true sharing: producer throughput falls as consumers are added because every `Consumer::try_recv` reads the producer's `write.v`. This is **inherent to SPMC broadcast** with a single shared progress counter and is **not** false sharing (the `align(64)` slots are isolated). Phase 8 **measures and reports** this; it does **not** modify the verified `sync` primitives to fix it. Record the honest curve.

**Mitigation hypothesis (documented for the writeup / flagship, NOT implemented here):** a consumer that caches the last-seen `write.v` and re-reads it only when it has drained up to the cached value would amortize the shared-cursor read across a batch, reducing true-sharing pressure at the cost of slightly staler "is there new data" visibility. This is a `sync` enhancement (a batched `recv` path) that would require its own loom/stress re-verification, so it is out of scope for the assembly phase. Note it in `e2e.md` as the next optimization and as a direct input to the flagship's output-multiplexer design. (This is the measure-first, optimize-with-evidence discipline: the assembly produced the number that justifies the future change.)

---

## 7. Synergy to the flagship

This pipeline is the flagship's **observability substrate** in miniature: one writer (the guest/book) producing a stream, a consistent live snapshot (seqlock = VM state), a broadcast event bus (ring = guest output multiplexer), and independent pinned consumers (orchestrator, log sink, viewer) that resync on overrun. Phase 8 proves the composition works end to end and produces the latency/throughput/true-sharing numbers that will shape the flagship's design — including whether the write-cursor mitigation (§6) is worth its complexity. The flagship is assembly, not invention, precisely because this phase rehearses the assembly.

---

## 8. Engineering Standard — governs this phase

1. **Compose verified parts; change none of them.** `book` frozen; `sync` primitives untouched; the engine uses public APIs only.
2. **Monomorphized, never `dyn`.** `Engine<B: OrderBook>`; the hot path inlines `apply`.
3. **No `unsafe`.** `engine` is `#![forbid(unsafe_code)]`; packing is explicit, validated at unpack.
4. **CO-correct end-to-end latency.** Measured production→consumption against the **scheduled** arrival, never push time. Service vs response time kept distinct.
5. **Measure, never guess.** Phase 4 methodology (recorded clock floor, pinning, warmup, `black_box`); the headline runs on **real** market data with the real-data book champion.
6. **Honesty is the signal.** The producer-throughput-vs-K decline is reported and correctly attributed (true sharing, not false sharing); the mitigation is a documented hypothesis, not an unmeasured claim.
7. **Composition demonstrated.** Overrun → seqlock resync is exercised and asserted, not just described.
8. **Green-gate discipline.** `cargo build`/`clippy -D warnings`/`test` green before each commit; one session → meaningful conventional commit(s) → STOP. Never commit red.

---

## 9. Phase 8 Definition of Done

1. `engine` crate: `pack`/`unpack` (validated, round-trip tested), `Engine<B>`, `EngineProducer<B>::process`, `EngineConsumer::poll` (with seqlock resync on overrun), `EngineHandle::consumer`; generic over the four impls; `#![forbid(unsafe_code)]`; a small unpinned demo binary.
2. End-to-end correctness test (§4): ordered/complete delivery for keeping-up consumers; valid (untorn) seqlock snapshots; overrun → resync exercised (resyncs > 0, post-resync ordered); final-state consistency. Green.
3. Benchmark 7 (§5): `bench → engine` edge added; `e2e.csv` across schedule × K × rate; CO-correct production-to-consumption latency, saturation rate, producer-throughput-vs-K, overrun rate — on real BTCUSDT (headline, `BTreeBook`) + synthetics. Plots cite the CSV; `e2e.md` written (interim, governor recorded).
4. True-sharing curve reported and correctly attributed (§6); mitigation hypothesis documented, not implemented; `sync` primitives unchanged.
5. Quarantine & freeze: `cargo tree -p bench` shows no `tokio`; `engine` links no async; six frozen-logic `book` files byte-identical to `book-v1-frozen`; `book/` + `feed/` + `sync/src/{seqlock,ring}.rs` untouched this phase (`git diff <pre-phase-8-commit>..HEAD -- book/ feed/ sync/src` empty).
6. Zero-`unsafe` invariant intact: `grep -rn "unsafe" --include='*.rs' book sync feed bench engine` finds nothing outside comments/docs; `engine` is `#![forbid(unsafe_code)]`.
7. `cargo build`/`clippy -D warnings`/`test` clean at every commit; meaningful conventional commits on `main`.

After Phase 8 the full pipeline runs, composed and measured. Next is Phase 9 (the microarchitecture profiling writeup) and Phase 10 (the DoD close-out: BENCHMARKS.md, ARCHITECTURE.md, README, x-thread).

---

# Appendix A — `CLAUDE.md` update for Phase 8

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md … phase7-spec.md  (as before)
- docs/specs/phase8-spec.md    — CURRENT: engine end-to-end assembly + production-to-consumption latency

## Hard rules
1. book frozen; sync primitives (seqlock, ring) UNCHANGED; feed unchanged. Phase 8
   adds the engine crate + one bench benchmark (bench -> engine edge).
2. The pipeline: feed replay -> book.apply -> seqlock.store(top) -> ring.push(pack(ev))
   on a pinned producer; pinned independent consumers poll/try_recv, resync from the
   seqlock on Overrun. engine = logic; bench = threads/pinning/timing.
3. Engine<B: OrderBook> monomorphized (NO dyn). Headline = BTreeBook (real-data
   champion) on the real BTCUSDT corpus. engine owns BookEvent<->[u64;5] packing,
   validated at unpack (no transmute).
4. #![forbid(unsafe_code)] in engine; workspace zero-unsafe invariant holds.
5. End-to-end latency is CO-correct: production->consumption vs the SCHEDULED arrival
   (event.ts = scheduled ns; latency = now - ts), never push time. Phase 4 methodology.
6. TRUE SHARING on the write cursor (Phase 7) will make producer throughput fall with
   K. MEASURE and REPORT it, attribute it correctly (true not false sharing). Do NOT
   modify the verified sync primitives to fix it; document the batched-recv mitigation
   as a hypothesis for the flagship. e2e.md is interim; record the governor.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test), commit,
list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (2 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | Engine assembly + correctness | `engine` crate (pack, producer/consumer, demo) + `pipeline.rs` (§3–§4) | round-trip + ordered-delivery + snapshot + overrun-resync + final-state tests green |
| 2 | End-to-end benchmark + findings | Benchmark 7 (§5) + `e2e.md` + DoD | `e2e.csv` + plots; true-sharing curve reported; DoD §9 verified |

Session 1 is the assembly and its correctness proof. Session 2 reuses the Phase 4 harness for the measurement. Keep them separate for clean commits and a safety margin.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase8-spec.md` §1–§4, §8. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement the `engine` crate — `engine/src/pack.rs` (`pack`/`unpack` with validated discriminants, `W=5`, round-trip test), `engine/src/lib.rs` (`Engine<B: OrderBook>`, `EngineProducer<B>::process` = apply + seqlock.store(top) + ring.push(pack(ev)), `EngineConsumer::poll` = try_recv → unpack/validate light work, Overrun → seqlock resync, `EngineHandle::consumer`), monomorphized (no `dyn`), `#![forbid(unsafe_code)]`, plus a small unpinned demo binary. Write `engine/tests/pipeline.rs` per §4 (ordered/complete delivery for keeping-up consumers; valid untorn seqlock snapshots; overrun → resync with resyncs>0 and post-resync ordered; final-state consistency). Touch no frozen/verified code (book, sync primitives, feed). Run the three gates; confirm the workspace zero-`unsafe` grep is still clean. Commit `feat(engine): pinned end-to-end pipeline (book + seqlock + ring)`. List changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase8-spec.md` §5–§6, §8, §9, and Phase 4's methodology §3. Execute **Session 2 only**: add `bench -> engine` to `bench/Cargo.toml`; implement Benchmark 7 (`bench/src/benches/e2e.rs`) + an `e2e` subcommand per §5 — pinned producer running `EngineProducer::<BTreeBook>::process` with a paced schedule stamping `event.ts` = scheduled ns, `K ∈ {1,2,4,8}` pinned consumers recording `now - ev.ts` (CO-correct production→consumption), real-arrival BTCUSDT replay + synthetic fixed-rate sweep to saturation, `black_box`, recorded clock floor. Write `bench/results/e2e.csv`; render the e2e-p99-vs-rate (per K) and producer-throughput-vs-K plots (cite the CSV); write `bench/results/e2e.md` (interim, governor recorded) reporting the real-corpus end-to-end latency, saturation rates, the producer-throughput-vs-K decline attributed to write-cursor TRUE sharing (not false sharing), the overrun behaviour, and the §6 batched-recv mitigation hypothesis. Do NOT modify the sync primitives. Confirm the §9.5 freeze check and the zero-`unsafe` grep. Run the three gates. Verify Phase 8 DoD §9 item by item and report each. Commit `feat(bench): end-to-end production-to-consumption latency benchmark`. STOP. The pipeline is assembled and measured.
```

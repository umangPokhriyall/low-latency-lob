# sync — Phase 7 Specification: The SPMC Broadcast Ring, Per-Slot Sequencing, loom Verification, and the False-Sharing Benchmark

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md` … `docs/specs/phase6-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 7 spec.** The seqlock is done and loom-verified (Phase 6). `book` is frozen; `feed`/`bench` are built.
**Scope:** the second crown-jewel primitive — a single-producer / many-independent-consumer **broadcast ring buffer** — bounded power-of-two, `#[repr(align(64))]` slots, per-slot sequence numbers (Vyukov-style), writer-never-blocks (lossy overwrite with overrun detection), loom-verified ordering, a real-thread no-loss / overrun / no-tear stress test, and a throughput + false-sharing benchmark.
**Audience:** Claude Code. Authoritative. Judged on lock-free correctness, not throughput; a lost/duplicated/torn entry that escapes, or an unsound shortcut, fails it.

---

## 1. Phase 7 in one paragraph

The engine has one writer producing a stream of events and several independent consumers — a strategy/aggregator, a recorder, a live viewer — each of which must observe the **whole** stream without ever stalling the writer. This is a broadcast bus: every consumer reads every item from its own cursor; the producer overwrites on wrap and never blocks; a consumer that falls more than a ring-length behind is lapped and must *detect* that overrun rather than silently corrupt. The hard, honest result of this phase is that a *sound* broadcast ring — one with no undefined behavior under Rust's memory model — stores its payload in **atomic words**, not an `UnsafeCell`, because broadcast slots are read by many consumers concurrently while the producer overwrites them, which is not the exclusive ownership that makes Vyukov's queue sound; the `UnsafeCell` + raw-copy shortcut is a data race, and the only `unsafe` design that would be sound (gating the producer on the slowest consumer) blocks the writer. So this ring, like the seqlock, is built sound with atomics and **no `unsafe`** — and the project ends with two loom-verified lock-free primitives and zero `unsafe` anywhere, which is the judgment signal, not a missed opportunity.

### 1.1 Frozen / reused / the unsafe decision
- `book` frozen and untouched; `feed` untouched. Phase 7 touches only `sync` (the ring) and `bench` (one benchmark). `bench → sync` edge exists from Phase 0.
- **`sync` runtime dependencies remain none** (std atomics; `#[repr(align(64))]` hand-rolled). `loom` stays a dev-dependency. The ring records carry `[u64; W]` words; the `BookEvent ↔ [u64; W]` packing lives in the **engine** (Phase 8), keeping the ring generic and sound.
- **The unsafe decision (read this):** the sound broadcast ring uses atomic-word slots and **no `unsafe`**. Rationale: in a broadcast ring a slot is read by many consumers *concurrently* while the producer *overwrites* it; that is not exclusive access, so a non-atomic `ptr::read` of the payload racing the producer's write is a **data race = UB** even though the stamp check discards the torn value (the same memory-model fact that sank the generic `UnsafeCell<T>` seqlock in Phase 6). Vyukov's `UnsafeCell` payload is sound only because his *queue* gives each slot exclusive ownership (producer XOR one consumer); broadcast has no such exclusivity. The one `unsafe` design that would be sound — gate the producer on the minimum consumer cursor so it never overwrites an unread slot — makes the producer **block on the slowest consumer**, violating writer-never-blocks. We therefore choose the sound atomic-word ring. On x86 a relaxed atomic load/store of an aligned `u64` is a plain `mov`; the §6 benchmark confirms the atomic copy is not a bottleneck. (`sync` keeps `#![deny(unsafe_op_in_unsafe_fn)]`; the §9 capstone confirms the *entire workspace* contains zero `unsafe`.)

---

## 2. Design

### 2.1 Broadcast, not a queue
Every consumer reads every item from its own cursor (the brief's "consumers fully independent, each tracks its own read cursor"; the flagship's "stream guest output to many readers"). This is fan-out/multicast, not work distribution. There is no shared read cursor, no CAS, no consumer coordination. The producer does not track consumers at all.

### 2.2 Writer-never-blocks ⇒ lossy overwrite + overrun detection
The producer writes monotonically and overwrites `slot[p & mask]` unconditionally — it never inspects consumer state, so it is **wait-free**. A consumer keeping within a ring-length of the producer reads every item exactly once, in order, untorn. A consumer that falls more than `capacity` behind is lapped; it **detects** the overrun via the per-slot sequence number, learns how many items it skipped, and resyncs to the oldest still-resident item. No silent loss; no torn value ever returned.

### 2.3 Sound atomic-word slots (no `unsafe`) — `UnsafeCell` rejected
Each slot holds the payload as `[AtomicU64; W]` plus a stamp `AtomicU64`. The producer overwrites words with `Relaxed` stores under a two-phase stamp (mark-busy → write → publish); a consumer reads words `Relaxed` under a per-slot **seqlock double-check** keyed by position. Atomic word accesses cannot race (no UB); the stamp protocol discards any value read across an overwrite. (Rejected: `UnsafeCell<[u64; W]>` + `ptr::read`/`write` — a data race under concurrent broadcast overwrite, §1.1. Rejected: producer-gating on the min consumer cursor — sound but blocks the writer.)

### 2.4 Slot & ring layout (cache-line discipline)
```rust
const WRITING: u64 = 1 << 63;   // high bit set while a slot is being overwritten
const EMPTY:   u64 = u64::MAX;  // initial stamp (no real position aliases it)

#[repr(align(64))]              // one slot per cache line: adjacent slots never false-share
struct Slot<const W: usize> {
    stamp: AtomicU64,           // position currently stored; WRITING bit set mid-overwrite
    words: [AtomicU64; W],
}

#[repr(align(64))]             // producer's write position on its OWN line (no false sharing w/ slots)
struct WritePos { v: AtomicU64 }

pub struct SpmcRing<const W: usize> {
    slots: Box<[Slot<W>]>,     // len = capacity, a power of two
    mask: u64,                 // capacity - 1
    write: WritePos,
}
```
`#[repr(align(64))]` forces `size_of::<Slot<W>>()` to a multiple of 64, so each slot occupies whole cache line(s) and the producer writing `slot[i]` never false-shares with a consumer reading `slot[j≠i]`. `WritePos` sits on its own line. A static assertion checks `size_of::<Slot<W>>() % 64 == 0`. loom/std atomic switch as in Phase 6.

### 2.5 Single-producer enforced at the type level
```rust
let ring = SpmcRing::<W>::with_capacity(cap);   // cap power of two (assert)
let (producer, handle) = ring.split();          // Producer is NOT Clone => one writer
let mut c1 = handle.consumer();                  // each consumer independent
let mut c2 = handle.consumer();
```
`Producer<W>` is `Send + !Clone` (the single-writer contract enforced by the type). `RingHandle<W>` (an `Arc<SpmcRing<W>>`) hands out `Consumer<W>`s. Each `Consumer` owns a private `cursor` and starts at the producer's current write position (joins live); a `consumer_from_oldest()` variant starts at the oldest resident item.

### 2.6 Composition with the seqlock (synergy, note it)
A lapped consumer that detects an overrun resyncs its derived state from the Phase 6 seqlock top-of-book snapshot, then resumes from the ring's oldest resident position. The two primitives compose into the realistic "lossy event bus + consistent resync point" pattern — which is exactly the flagship's "stream output (ring) + poll current state (seqlock)" shape.

---

## 3. Memory ordering — the proof (centerpiece; comment every ordering)

```rust
impl<const W: usize> SpmcRing<W> {
    /// SINGLE-PRODUCER push. Wait-free; never blocks on consumers (overwrites on wrap).
    pub fn push(&self, rec: [u64; W]) {
        let p = self.write.v.load(Relaxed);                 // single producer owns the write position
        let slot = &self.slots[(p & self.mask) as usize];

        // (P1) Mark the slot busy at position p BEFORE overwriting any word. The Release
        //      fence orders this stamp store ahead of the word stores, so a consumer
        //      mid-read of the PRIOR generation observes the stamp change and detects
        //      the overwrite (its s2 check fails -> overrun) rather than reading torn words.
        slot.stamp.store(p | WRITING, Relaxed);
        fence(Release);

        // (P2) Overwrite the payload (Relaxed atomics: no data race; published by (P3)).
        for i in 0..W { slot.words[i].store(rec[i], Relaxed); }

        // (P3) Publish ready at position p (Release): a consumer whose Acquire load sees
        //      stamp == p is guaranteed to see these word stores.
        slot.stamp.store(p, Release);

        // (P4) Advance the write position (Release): a consumer's Acquire load of `write`
        //      establishes that positions < new value have been published.
        self.write.v.store(p.wrapping_add(1), Release);
    }
}

pub enum Recv<const W: usize> { Item([u64; W]), Empty, Overrun { skipped: u64 } }

impl<const W: usize> Consumer<W> {
    pub fn try_recv(&mut self) -> Recv<W> {
        let w = self.ring.write.v.load(Acquire);            // (R0) how far the producer has advanced
        if self.cursor >= w { return Recv::Empty; }         // caught up

        let slot = &self.ring.slots[(self.cursor & self.ring.mask) as usize];
        let s1 = slot.stamp.load(Acquire);                  // (R1) pairs with (P3): see published words
        if s1 != self.cursor {
            // WRITING bit set (overwrite in progress) or stamp > cursor (already lapped):
            // we have been overrun. (s1 < cursor cannot occur while cursor < w.)
            return self.resync(w);
        }

        let mut rec = [0u64; W];                            // (R2) read payload (Relaxed)
        for i in 0..W { rec[i] = slot.words[i].load(Relaxed); }

        fence(Acquire);                                     // (R3) order the word loads BEFORE the s2 load
        let s2 = slot.stamp.load(Relaxed);
        if s2 != self.cursor {
            // producer began overwriting during our read -> torn -> overrun.
            return self.resync(w);
        }

        self.cursor += 1;
        Recv::Item(rec)                                     // clean: words are exactly generation `cursor`
    }

    fn resync(&mut self, w: u64) -> Recv<W> {
        let oldest = w.saturating_sub(self.ring.capacity() as u64); // oldest position still resident
        let skipped = oldest.saturating_sub(self.cursor);
        self.cursor = oldest;
        Recv::Overrun { skipped }
    }
}
```

**Why it is correct (put this in the doc comment):**
- **No torn item is ever returned.** A clean return requires `s1 == cursor` (stable, WRITING clear) *and* `s2 == cursor` with an Acquire fence between the word loads and `s2`. If the producer started overwriting generation `cursor` (with `cursor+CAP`) at any point during the read, it first set the WRITING bit / changed the stamp ((P1) ordered ahead of the word writes by the Release fence), so `s2 != cursor` and the read is discarded.
- **No loss / no duplication for a keeping-up consumer.** If `cursor` never trails `write` by more than `capacity`, `slot[cursor & mask]` still holds generation `cursor` (`stamp == cursor`), so the consumer reads every position once, in monotonic order (`cursor += 1` only on a clean read).
- **Overrun is detected, never silent.** If the producer lapped the consumer, `stamp > cursor` (or WRITING set), caught at `(R1)`/`(R3)`; `resync` reports `skipped` and jumps to the oldest resident position.
- **The producer is wait-free.** `push` does a bounded number of `Relaxed`/`Release` stores and one fence; it never reads consumer state and never loops.
- **Soundness.** Every payload access is an atomic `Relaxed` op, so concurrent overwrite-during-read is not a data race; the torn value is discarded before it is interpreted. No `unsafe`, no UB.
- **Progress honesty.** Producer wait-free; consumers are *not* lock-free (a fast producer can force perpetual overruns), but they always make defined progress (each `try_recv` returns) and never block the producer. State this; do not claim "lock-free."

### 3.1 Single-thread unit tests
push→recv round-trip of W-word records; wrap past `capacity` with a single fast consumer (no overrun, all items in order); tiny capacity + a deliberately-stalled consumer (single-thread simulation: push `capacity+k`, then recv → `Overrun { skipped }` with the exact skip count, then ordered items resume); `Empty` when caught up; the static `size_of::<Slot<W>>() % 64 == 0` assertion.

---

## 4. loom verification (`sync/tests/loom_ring.rs`, `#[cfg(loom)]`)

Apply the Phase-6 lessons up front: consumer retry/`Empty`-poll spins must go through `loom::thread::yield_now` under `cfg(loom)` (otherwise loom cannot see progress), and the model will likely need a bounded `LOOM_MAX_PREEMPTIONS` — document the bound and the reason.

**Consistency witness:** the producer pushes records whose words are a deterministic function of the position — e.g. `rec[k] = pos * (W as u64) + k`. A consumer that receives `Recv::Item(rec)` recovers `pos = rec[0] / W` and asserts `rec[k] == pos*W + k` for all `k` (self-consistent, untorn) and that successive delivered positions are strictly increasing (allowing for `Overrun { skipped }` jumps, after which the next position must equal the resync target). A torn item fails the witness; a duplicate or out-of-order delivery fails monotonicity.

**Model (small):** one producer pushing 2–3 records into a capacity-2 ring (so overrun is reachable in-model), two consumers each doing a couple of `try_recv`s, draining with `yield_now`. Assert the witness + monotonicity on every `Item`; assert any `Overrun` has a plausible `skipped`. Run: `RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_ring --release`. Verify teeth: removing the `(R3)` fence, or the `(P1)` mark-busy + fence, or weakening `(P3)` to `Relaxed`, must make loom find a torn/lost item (do not commit the broken variants; note the result).

---

## 5. Real-thread stress (`sync/tests/stress_ring.rs`)

Three required scenarios, std threads only, integer accounting (no floats — respect the `sync` `f64` ban), bounded runtime:
1. **No-loss path:** capacity comfortably large, `K` consumers that keep up, producer pushes `N` (e.g. 1–10M) witness records. Assert each consumer delivers **every** position `0..N` exactly once, in order, untorn, with **zero overruns**. This proves the lossless behaviour when consumers keep pace.
2. **Overrun-detection path:** small capacity + a deliberately-throttled consumer against a full-tilt producer. Assert overruns are **reported** (never silent), `skipped` counts are consistent (delivered positions plus skipped account monotonically for all produced positions), and every delivered `Item` is still self-consistent and in order.
3. **No-tear / no-dup under contention:** full-tilt producer + `K = max(2, available_parallelism()-1)` consumers; assert zero witness violations and zero duplicates across millions of deliveries; producer completes its full budget regardless of `K` (writer-never-blocks check).

---

## 6. Benchmark (`bench`, Benchmark 6) — throughput, latency, false-sharing evidence

Reuse the Phase 4 harness (`clock`, `recorder`, pinning, `black_box`, recorded clock floor). Add `bench/src/benches/ring.rs` and a `ring` subcommand.

- **Throughput:** pinned producer pushing full-tilt; sweep `K ∈ {1,2,4,8}` pinned consumers draining. Report producer push throughput (Mev/s) and per-consumer drain throughput.
- **False-sharing evidence:** producer push throughput must stay ~**flat** as `K` rises (consumers read distinct lines; the write position is isolated). A throughput collapse as `K` grows would indicate false sharing — the test for the `#[repr(align(64))]` discipline. Report the curve.
- **Latency:** `push` latency distribution (expected near the clock floor) and `try_recv` latency distribution; `black_box` payloads.
- **Overrun rate** vs consumer speed (full-tilt vs paced producer).

**`bench/results/ring_bench.csv`**
```
mode,consumers,capacity,words,samples,clock_overhead_ns,push_p50_ns,push_p99_ns,recv_p50_ns,recv_p99_ns,producer_mev_s,overrun_rate
```
Plots (cite the CSV): producer throughput vs `K` (the false-sharing test — expect flat); push/recv p99 vs `K`. A short `bench/results/ring.md` (interim, Writing-Standard-clean) reports throughput, the flat-throughput false-sharing result, latencies, and overrun behaviour.

---

## 7. Synergy to the flagship

This ring is the sandbox's **host↔guest output multiplexer**: the guest-facing writer streams stdout/stderr/event records into the ring and never blocks; the orchestrator, a log sink, and a live viewer each attach an independent `Consumer` and replay the whole stream, a lagging viewer detecting overrun and resyncing. Together with the Phase 6 seqlock (the VM live-state read path), the flagship inherits both halves of its observability substrate — built, verified, and measured in isolation here.

---

## 8. Engineering Standard — governs this phase

1. **Soundness over the unsafe shortcut.** Atomic-word slots, no `unsafe`; the `UnsafeCell` broadcast copy is a documented data race; producer-gating is rejected because it blocks the writer.
2. **Ordering is argued, not asserted.** Every `Acquire`/`Release`/`fence`/`Relaxed` carries a comment naming its pairing (§3); the doc comment holds the no-tear / no-loss / overrun-detection / wait-free argument.
3. **Verified, then corroborated.** loom model-checks the ordering with a position-witness (§4); the real-thread stress (§5) corroborates the no-loss, overrun-detection, and no-tear/dup paths on hardware. Both required.
4. **Writer wait-free; consumers not lock-free.** Stated honestly; no "lock-free" overclaim. Overruns are detected, never silent.
5. **No false sharing.** `#[repr(align(64))]` slots + isolated write position; proven by the static size assertion AND the flat producer-throughput-vs-`K` benchmark.
6. **Measure, never guess.** Benchmark obeys Phase 4 methodology; the atomic-copy-is-cheap claim (§1.1) is confirmed with numbers, not assumed.
7. **Frozen respect.** `book` (six frozen-logic files) and `feed` untouched this phase; `sync` runtime-dep-free; `loom` dev-only.
8. **Green-gate discipline.** `cargo build`/`clippy -D warnings`/`test` green **plus** the loom run green before the relevant commits. One session → meaningful conventional commit(s) → STOP. Never commit red.

---

## 9. Phase 7 Definition of Done

1. `sync/src/ring.rs`: `SpmcRing<W>`, `Producer<W>` (`!Clone`), `RingHandle`, `Consumer<W>`, `Recv` per §2–§3; `#[repr(align(64))]` slots + isolated `WritePos`; fully-commented ordering with the no-tear/no-loss/overrun/wait-free argument; `size_of % 64` static assert; single-thread unit tests green. No `unsafe`.
2. loom verification (§4) green under `--cfg loom` with the position-witness + monotonicity; preemption bound and reason documented; teeth note (removing `(R3)`/`(P1)` or weakening `(P3)` makes loom fail).
3. Real-thread stress (§5): no-loss path (every position exactly once, zero overruns), overrun-detection path (reported, accounted), no-tear/no-dup under contention (zero violations), producer completes its budget regardless of `K`; bounded runtime.
4. Benchmark 6 (§6): `ring_bench.csv` across the `K` × mode sweep; throughput, latencies, overrun rate reported; the producer-throughput-vs-`K` false-sharing plot is flat; plots cite the CSV; `ring.md` written (interim).
5. `sync` has no runtime deps; `loom` dev-only; `cargo tree -p sync` (default) shows only `sync`. `cargo tree -p bench` shows no `tokio`.
6. **Freeze respected (corrected check):** `git diff book-v1-frozen -- book/src/price.rs book/src/event.rs book/src/book.rs book/src/btree.rs book/src/sorted_vec.rs book/src/rev_vec.rs` is empty (six frozen-logic files), and `book/` + `feed/` are untouched this phase (`git diff <pre-phase-7-commit>..HEAD -- book/ feed/` empty).
7. **Zero-`unsafe` capstone:** a workspace grep shows **no `unsafe`** in any crate (`grep -rn "unsafe" --include='*.rs' book sync feed bench` returns nothing outside comments/docs). Optionally tighten `sync` to `#![forbid(unsafe_code)]` so the whole workspace is compiler-enforced unsafe-free; if done, note it.
8. `cargo build`/`clippy -D warnings`/`test` clean and the loom run green at the relevant commits; meaningful conventional commits on `main`.

After Phase 7 both crown-jewel primitives are proven and measured. Next is Phase 8 (`engine`: the pinned end-to-end hot path that assembles the frozen book, the seqlock, and this ring).

---

# Appendix A — `CLAUDE.md` update for Phase 7

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md … phase6-spec.md  (as before)
- docs/specs/phase7-spec.md    — CURRENT: sync SPMC broadcast ring (ordering, loom, stress, false-sharing bench)

## Hard rules
1. book frozen/done; feed done. Phase 7 touches only sync (the ring) + bench (one benchmark).
2. The SPMC ring is a BROADCAST bus: single producer (wait-free, overwrites on wrap,
   never blocks), many INDEPENDENT consumers (own cursor, read the whole stream).
   Lossy overwrite + OVERRUN DETECTION (never silent loss); no torn item returned.
3. SOUND with NO unsafe: payload is atomic words ([AtomicU64; W]) read/written Relaxed,
   ordered by a per-slot stamp (Vyukov-style position + WRITING bit; seqlock double-check).
   UnsafeCell+ptr broadcast copy is a DATA RACE (UB) — rejected. Producer-gating is sound
   but BLOCKS the writer — rejected. Vyukov's UnsafeCell is sound only for single-consume
   QUEUES (exclusive slots), not broadcast.
4. #[repr(align(64))] slots + isolated WritePos (no false sharing); proven by static
   size assert AND a flat producer-throughput-vs-K benchmark.
5. Ordering ARGUED in comments (each Acquire/Release/fence named) and VERIFIED by loom
   (position-witness, yield_now under cfg(loom), documented preemption bound) AND
   corroborated by real-thread stress (no-loss, overrun-detection, no-tear/dup).
6. Writer wait-free; consumers NOT lock-free. No overclaim.
7. ZERO-unsafe capstone: the WHOLE workspace contains no unsafe (both lock-free
   primitives are sound atomic constructions). Optionally forbid(unsafe_code) in sync.
8. sync runtime deps: none; loom dev-only. Bench obeys Phase 4 methodology; ring.md interim.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test; loom
session also runs --cfg loom green), commit, list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (3 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | Ring + ordering + loom | `ring.rs` (§2–§3) + unit tests + `loom_ring.rs` (§4) | unit tests green; loom green under `--cfg loom` |
| 2 | Real-thread stress | `stress_ring.rs` (§5) | no-loss, overrun-detection, no-tear/dup all green |
| 3 | False-sharing benchmark | Benchmark 6 in `bench` (§6) + `ring.md` + DoD + capstone | `ring_bench.csv` + plots; flat throughput vs K; DoD §9 verified |

Session 1 is load-bearing (ordering + loom; budget for the loom preemption bound). Session 3 reuses the Phase 4 harness and runs the zero-unsafe capstone.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase7-spec.md` §1–§4, §8. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement `sync/src/ring.rs` exactly per §2–§3 — `#[repr(align(64))] Slot<W>` (stamp + `[AtomicU64; W]`), isolated `WritePos`, `SpmcRing<W>` with power-of-two capacity, `split()` into a `!Clone Producer<W>` + `RingHandle`, `Consumer<W>` with private cursor + `try_recv -> Recv<W>`, `push`/`resync`, the precise `Acquire`/`Release`/`fence`/`Relaxed` ordering with every op commented and the no-tear/no-loss/overrun/wait-free argument in the doc comment. Atomic words only — NO `unsafe`. Add the §3.1 single-thread unit tests incl. the `size_of::<Slot<W>>() % 64 == 0` static assertion and the single-thread overrun test. Write `sync/tests/loom_ring.rs` per §4 (capacity-2 ring, one producer, two consumers, position-witness + monotonicity, `yield_now` under `cfg(loom)`). Wire re-exports in `sync/src/lib.rs`. Run the three gates AND `RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_ring --release` (bound `LOOM_MAX_PREEMPTIONS` and document it). `cargo tree -p sync` shows no runtime deps. Commit `feat(sync): SPMC broadcast ring + loom-verified ordering`. List changes + the loom result, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase7-spec.md` §5, §8. Execute **Session 2 only**: implement `sync/tests/stress_ring.rs` per §5 — (1) no-loss path (large capacity, keeping-up consumers, every position `0..N` exactly once, zero overruns), (2) overrun-detection path (small capacity + throttled consumer vs full-tilt producer; overruns reported and `skipped` accounted; delivered items self-consistent and ordered), (3) no-tear/no-dup under contention (`K=max(2,available_parallelism()-1)` consumers, full-tilt; zero witness violations, zero dups; producer completes its budget regardless of `K`). Integer accounting only (no floats). std threads; bounded runtime. Run the three gates. Commit `test(sync): ring no-loss / overrun-detection / no-tear stress`. List changes + deliveries, overruns, violations (must be 0), STOP.

**Session 3**
> Read `CLAUDE.md` and `phase7-spec.md` §6, §8, §9, and Phase 4's methodology §3. Execute **Session 3 only**: implement Benchmark 6 (`bench/src/benches/ring.rs`) + a `ring` subcommand per §6 — pinned producer + `K ∈ {1,2,4,8}` pinned consumers, push/recv latency into `Recorder`s, producer throughput, overrun rate, `black_box`, recorded clock floor. Write `bench/results/ring_bench.csv`, render the producer-throughput-vs-`K` plot (must be ~flat = false-sharing-free) and push/recv-p99-vs-`K` plot (cite the CSV), and `bench/results/ring.md` (interim). Run the zero-`unsafe` capstone: `grep -rn "unsafe" --include='*.rs' book sync feed bench` returns nothing (optionally add `#![forbid(unsafe_code)]` to `sync` and note it). Confirm the §9.6 corrected freeze check. Run the three gates. Verify Phase 7 DoD §9 item by item and report each. Commit `feat(bench): ring throughput + false-sharing benchmark`. STOP. Both lock-free primitives are complete.
```

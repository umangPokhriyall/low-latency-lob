# sync — Phase 6 Specification: The Seqlock Snapshot Cell, Memory Ordering, loom Verification, and the Contention Benchmark

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md` … `docs/specs/phase5-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 6 spec.** The order-book shootout is complete (`book` frozen, four impls, sourced verdict). `feed` and `bench` are built.
**Scope:** the first crown-jewel concurrency primitive — a single-writer / many-reader **seqlock** over a top-of-book snapshot — its precise memory ordering, **loom** model-checked verification, a real-thread torn-read stress test, and a read-latency-under-write-contention benchmark.
**Audience:** Claude Code. Authoritative. This phase is judged on memory-model correctness, not throughput; a torn read that escapes, or a hand-wavy ordering argument, fails it.

---

## 1. Phase 6 in one paragraph

A market-data engine has one writer mutating the book and many readers (a GUI, an aggregator, the flagship's orchestrator) that must observe a *consistent* top-of-book snapshot at high frequency without ever stalling the writer. A mutex inverts the priority — readers block the writer; a per-field atomic read tears across writes — a reader sees this write's bid and the next write's ask. The seqlock resolves both: the writer is wait-free (never blocked by any reader) and publishes via a version counter; readers take an optimistic snapshot and retry if a write straddled their read, so a torn value is detected and discarded, never returned. The entire signal of this phase is that the `Acquire`/`Release`/`fence` ordering is *correct* — argued field by field, **verified by loom's exhaustive interleaving model**, and corroborated by a multi-million-iteration real-thread stress test — and that the read path stays fast under write contention while the writer's latency is provably independent of reader count.

### 1.1 Frozen / reused / dependency posture
- `book` is frozen and reused: `TopOfBook` is assembled from `book`'s public `Px`/`Qty` semantics (stored as plain `i64` ticks). No `book` change.
- `bench` (not frozen) gains one benchmark (§6) that consumes `sync`. The `bench → sync` edge already exists from Phase 0.
- **`sync` runtime dependencies: none** (std atomics only; cache padding is hand-rolled `#[repr(align(64))]`). **Dev-dependency: `loom`** (model checker, `#[cfg(loom)]` only) — permitted; the zero-dep rule was `book`-specific.
- **`unsafe` posture:** `sync` keeps `#![deny(unsafe_op_in_unsafe_fn)]` (not `forbid`), but **Phase 6 introduces no `unsafe`.** The seqlock is sound with an atomic-scalar payload (§2.2). The unsafe budget is reserved for Phase 7's SPMC ring, where the slot buffer genuinely requires it. Using `unsafe` only where it is actually necessary — not where a sound atomic design suffices — is the engineering-judgment signal.

---

## 2. The seqlock design

### 2.1 The protocol
A version counter `seq` (even = stable, odd = write in progress). The writer increments to odd, writes the payload, increments to even. A reader snapshots `seq`, reads the payload, re-reads `seq`; it accepts the snapshot only if `seq` was even and unchanged across the read. Any concurrent write changes `seq` (even→odd→even, +2), so a straddling read is detected and retried.

### 2.2 Decision: atomic-scalar payload (sound, no `unsafe`) — not a generic `UnsafeCell<T>`
The payload is a fixed set of scalars stored as **atomics**, accessed `Relaxed`, with the version counter providing all happens-before ordering. A reader may load fields from across a write boundary, but the `seq` check discards that snapshot before returning it — and because every field access is an atomic `Relaxed` op, **there is no data race and no UB**, only a possibly-stale value that gets filtered. (Rejected: a generic `SeqLock<T: Copy>` over `UnsafeCell<T>` copied with `ptr::read`/`write` + fences — the Linux-kernel pattern. In Rust's memory model a non-atomic read of `T` racing a writer's non-atomic write is a **data race = undefined behavior**, regardless of whether the torn value is later discarded; C's `volatile` gives the kernel semantics Rust does not. A formally-UB primitive is exactly the kind of thing a Principal Engineer dismisses, so it is out. The word-atomic-copy generic seqlock is sound but needless complexity for a fixed scalar snapshot — "simple and fast beats clever and fast." The atomic-scalar design generalizes to the flagship's state snapshot, which is also scalars.)

### 2.3 Types (`sync/src/seqlock.rs`)
```rust
//! Single-writer / many-reader seqlock over a top-of-book snapshot.
//! Writer is WAIT-FREE (never blocked by readers). Readers are optimistic and
//! retry on a straddling write (a seqlock reader is NOT lock-free — a continuous
//! writer can starve a reader — but the writer's progress is never impeded).
//! Sound with NO unsafe: payload fields are atomics accessed Relaxed; the version
//! counter (Acquire/Release) carries all ordering; torn snapshots are detected by
//! the version check and discarded, never returned. See §3 for the ordering proof.

// loom/std atomic switch (loom model-checks only the code that uses loom's atomics).
#[cfg(loom)]
use loom::sync::atomic::{fence, AtomicI64, AtomicU32, AtomicU64, Ordering};
#[cfg(not(loom))]
use std::sync::atomic::{fence, AtomicI64, AtomicU32, AtomicU64, Ordering};
use Ordering::{Acquire, Relaxed, Release};

/// The value a reader receives — plain, `Copy`, no atomics. `stamp` is a monotonic
/// write counter used for provenance and as the stress/loom consistency witness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TopOfBook {
    pub bid_px: i64,
    pub bid_qty: i64,
    pub ask_px: i64,
    pub ask_qty: i64,
    pub stamp: u64,
}

/// Cache-line aligned so an array of cells does not false-share, and so the version
/// and payload the single writer touches sit together.
#[repr(align(64))]
pub struct SeqLock {
    seq: AtomicU32, // even = stable, odd = write in progress
    bid_px: AtomicI64,
    bid_qty: AtomicI64,
    ask_px: AtomicI64,
    ask_qty: AtomicI64,
    stamp: AtomicU64,
}
```

### 2.4 Writer model
Single writer (the engine's book thread). The API documents that concurrent `store` from multiple threads is unsupported (it would corrupt the parity). A `&self` API is used so many readers and the one writer share an `&SeqLock`; the single-writer contract is a documented invariant, optionally enforced by a `&mut`-taking writer handle in a later phase. For Phase 6, document the contract; do not add multi-writer machinery.

---

## 3. Memory ordering — the proof (the centerpiece; comment it exactly)

```rust
impl SeqLock {
    #[must_use]
    pub fn new(init: TopOfBook) -> Self { /* all fields from `init`, seq = 0 (even) */ todo!() }

    /// SINGLE-WRITER store. Wait-free; never blocked by readers.
    pub fn store(&self, t: TopOfBook) {
        let s = self.seq.load(Relaxed);          // single writer => no other writer races this
        // Enter write: mark odd. Relaxed is enough for the value; the fence below
        // orders this BEFORE the payload writes so a reader cannot observe new
        // payload while seq still reads even.
        self.seq.store(s.wrapping_add(1), Relaxed);
        fence(Release);                           // (W1) odd-marker happens-before payload writes

        self.bid_px.store(t.bid_px, Relaxed);     // payload: Relaxed; ordering is carried by seq
        self.bid_qty.store(t.bid_qty, Relaxed);
        self.ask_px.store(t.ask_px, Relaxed);
        self.ask_qty.store(t.ask_qty, Relaxed);
        self.stamp.store(t.stamp, Relaxed);

        // Leave write: mark even with RELEASE. (W2) This publishes all payload
        // stores: a reader whose Acquire-load observes this even value is guaranteed
        // to see these payload values.
        self.seq.store(s.wrapping_add(2), Release);
    }

    /// Many-reader load. Returns a snapshot from a single consistent write.
    #[must_use]
    pub fn load(&self) -> TopOfBook {
        loop {
            let s1 = self.seq.load(Acquire);      // (R1) Acquire: pairs with (W2); see that write's payload
            if s1 & 1 != 0 {                      // odd => write in progress; retry
                core::hint::spin_loop();
                continue;
            }
            let t = TopOfBook {                   // payload: Relaxed loads
                bid_px: self.bid_px.load(Relaxed),
                bid_qty: self.bid_qty.load(Relaxed),
                ask_px: self.ask_px.load(Relaxed),
                ask_qty: self.ask_qty.load(Relaxed),
                stamp: self.stamp.load(Relaxed),
            };
            fence(Acquire);                        // (R2) order the payload loads BEFORE the s2 load,
                                                   //      so s2 cannot be reordered ahead of them
            let s2 = self.seq.load(Relaxed);
            if s1 == s2 {                          // even and unchanged => no write straddled the read
                return t;
            }
            // a write occurred during the read => the snapshot may be torn; discard, retry
        }
    }
}
```

**Why it is correct (state this argument in the doc comment):**
- **No torn snapshot is ever returned.** A write changes `seq` from even `s` to odd `s+1` to even `s+2`. If any write *starts* during the reader's payload loads, the reader's `s2` differs from `s1` (it is now ≥ `s+1`), so the snapshot is discarded. If `s1 == s2` and even, no write was in progress at `s1` and none started before `s2`, so all payload loads came from the single write that produced version `s1`.
- **The writer is wait-free.** `store` performs a bounded number of `Relaxed` stores, one fence, and one `Release` store; nothing it does waits on any reader.
- **Ordering pairings:** `(W2)` Release on the closing even-store synchronizes-with `(R1)` Acquire on `s1`, so a reader observing the new even version sees the payload. `(W1)` orders the odd-marker before payload writes; `(R2)` orders payload loads before the `s2` load. The payload accesses are `Relaxed` *atomics* — race-free by construction — so there is no UB even on a discarded torn read.
- **Progress honesty:** the writer is wait-free; readers are **not** lock-free — a continuously-storing writer can starve a reader into endless retry. This is the seqlock's inherent trade and must be stated, not glossed. In the engine the writer stores at the feed rate (bounded), so reads converge.

### 3.1 Single-thread unit tests
`new(init).load() == init`; `store(x); load() == x`; round-trips for several distinct snapshots; `Default` snapshot is all-zero.

---

## 4. loom verification (`sync/tests/loom_seqlock.rs`, `#[cfg(loom)]`)

loom exhaustively explores thread interleavings *and* the permitted memory-ordering reorderings, so it is the right tool to prove the §3 ordering rather than assert it. Keep the model **small** (loom is exponential): one writer, two readers, a handful of operations.

**The consistency witness:** the writer only ever stores snapshots in which every field is a deterministic function of `stamp` — e.g. `bid_px = stamp*4`, `bid_qty = stamp*4+1`, `ask_px = stamp*4+2`, `ask_qty = stamp*4+3`. A *consistent* snapshot therefore satisfies `bid_qty==bid_px+1 && ask_px==bid_px+2 && ask_qty==bid_px+3`; a *torn* snapshot (fields from two stamps) violates it. The reader asserts the witness on every returned snapshot.

**Model:**
```rust
#[cfg(loom)]
#[test]
fn loom_no_torn_reads() {
    loom::model(|| {
        let cell = Arc::new(SeqLock::new(snapshot_for(0)));
        let w = { let c = cell.clone(); loom::thread::spawn(move || {
            c.store(snapshot_for(1));
            c.store(snapshot_for(2));
        })};
        let r1 = { let c = cell.clone(); loom::thread::spawn(move || {
            assert_consistent(c.load());
        })};
        let r2 = { let c = cell.clone(); loom::thread::spawn(move || {
            assert_consistent(c.load());
        })};
        w.join().unwrap(); r1.join().unwrap(); r2.join().unwrap();
        assert_consistent(cell.load());
    });
}
```
`assert_consistent` checks the witness relations and that `stamp ∈ {0,1,2}`. Use `loom::sync::Arc`. Run: `RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_seqlock --release`. If the explored state space is too large, bound it (`LOOM_MAX_PREEMPTIONS=3`) and document the bound; do **not** weaken the witness.

A deliberately-wrong variant (e.g., dropping fence `(R2)` or using `Relaxed` on `(W2)`) should make loom find a failing interleaving — note this in the doc as the evidence the test has teeth (do not commit the broken variant).

---

## 5. Real-thread torn-read stress test (`sync/tests/stress_seqlock.rs`)

loom proves the model; the stress test exercises real hardware and the std atomics. One writer thread stores monotonically-stamped snapshots (the §4 witness) as fast as it can for a fixed iteration/time budget; `N` reader threads (e.g., `available_parallelism()-1`, min 2) loop `load()` and assert the witness on every snapshot, tracking the max `stamp` seen for monotonic sanity and counting retries. Assert **zero** witness violations across millions of reads. Also assert **writer-never-blocks**: the writer completes its full iteration budget regardless of reader count (a coarse check that readers do not stall it). std threads only; no deps. Keep runtime bounded (a few seconds) so it fits CI.

---

## 6. Read-latency-under-write-contention benchmark (`bench`, Benchmark 5)

Reuse the Phase 4 harness (`clock`, `recorder`, pinning, `black_box`, recorded clock floor). Add `bench/src/benches/seqlock.rs` and a `seqlock` subcommand.

**Setup:** pin one writer thread (core A) storing snapshots; pin `K` reader threads (cores B, C, …) each timing `load()` into its own `Recorder` and counting retries; `black_box` the returned snapshot. Sweep `K ∈ {1, 2, 4, 8}` (capped at available cores) and writer mode ∈ {`full_tilt`, `paced@feed_rate`}. Separately record the **writer's** `store` latency distribution to show it is independent of `K` (writer-never-blocks, quantified).

**`bench/results/seqlock_read.csv`**
```
readers,writer_mode,samples,clock_overhead_ns,read_p50_ns,read_p99_ns,read_p999_ns,read_max_ns,mean_retries_per_load,write_p50_ns,write_p99_ns
```
Plots (cite the CSV): read p99 vs reader count; writer store p50/p99 vs reader count (expected flat — the proof that readers don't tax the writer). A short `bench/results/seqlock.md` states the read latency, the retry rate under contention, and the writer-independence result, sourced to the CSV (Writing Standard from phase4-spec §10 applies; this is interim, not the Phase 10 writeup).

---

## 7. Synergy to the flagship

This seqlock is the sandbox's **live-state read path**: the orchestrator polls a microVM's current state (status, resource counters, the head index into the Phase 7 output ring) at high frequency without ever stalling the VM's writer thread — the identical single-writer/many-reader pattern, the identical ordering proof, over a different scalar snapshot. Building and verifying it here, in isolation, means the flagship inherits a proven primitive rather than inventing one under deadline.

---

## 8. Engineering Standard — governs this phase

1. **Soundness over cleverness.** No `unsafe` this phase; the atomic-scalar payload makes torn reads impossible to *return* and impossible to be UB. The rejected `UnsafeCell<T>` generic is a documented data race.
2. **Ordering is argued, not asserted.** Every `Acquire`/`Release`/`fence`/`Relaxed` carries a comment naming its pairing and purpose (§3). The doc comment contains the no-torn-read argument.
3. **Verified, then corroborated.** loom model-checks the ordering (§4); the real-thread stress test (§5) corroborates on hardware. Both are required; neither substitutes for the other.
4. **Honest progress guarantees.** State plainly: writer wait-free; readers not lock-free (starvable). No overclaiming "lock-free."
5. **Measure, never guess.** The contention numbers obey the Phase 4 methodology (recorded clock floor, pinning, warmup, `black_box`); writer-independence is shown with numbers, not claimed.
6. **No false sharing.** `#[repr(align(64))]` on the cell; note it.
7. **Frozen respect.** `book` untouched; `sync` runtime-dep-free; `loom` is a dev-dependency only.
8. **Green-gate discipline.** `cargo build`/`clippy -D warnings`/`test` green, **plus** the loom run green, before the relevant commits. One session → meaningful conventional commit(s) → STOP. Never commit red.

---

## 9. Phase 6 Definition of Done

1. `sync/src/seqlock.rs`: `TopOfBook` + `SeqLock` per §2–§3, `#[repr(align(64))]`, loom/std atomic switch, fully-commented ordering with the no-torn-read argument; single-thread unit tests green. No `unsafe`.
2. loom verification (§4) green under `--cfg loom` with the consistency witness; the preemption bound (if any) documented; a note that removing a fence/Release makes loom fail (teeth).
3. Real-thread stress (§5): millions of reads, `N` readers, zero witness violations, writer completes its budget regardless of `K`; bounded runtime.
4. Benchmark 5 (§6): `seqlock_read.csv` committed across the `K` × writer-mode sweep; read latency + retry rate + writer-independence reported; plots cite the CSV; `seqlock.md` written (interim, Writing-Standard-clean).
5. `sync` has no runtime deps; `loom` dev-dep only; `cargo tree -p sync` (default) shows only `sync`.
6. `book` byte-for-byte unchanged (`git diff book-v1-frozen -- book/` empty); `feed/src` unchanged.
7. `cargo build`/`clippy -D warnings`/`test` clean and the loom run green at the relevant commits; meaningful conventional commits on `main`.

After Phase 6 the seqlock snapshot cell is proven and measured. Next is Phase 7 (`sync`: the SPMC cache-line-aligned ring buffer — where the `unsafe` budget is finally spent).

---

# Appendix A — `CLAUDE.md` update for Phase 6

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, the four-impl shootout, DoD culture
- docs/specs/phase0-spec.md    — workspace, tick types, guardrail
- docs/specs/phase1-spec.md    — event model, OrderBook trait, BTreeBook
- docs/specs/phase2-spec.md    — Vec impls, differential oracle, FREEZE (book-v1-frozen)
- docs/specs/phase3-spec.md    — feed: corpus, replay, synthetic, recorder
- docs/specs/phase4-spec.md    — bench harness, depth sweep, CO-correct study, crossover
- docs/specs/phase5-spec.md    — FlatBook, four-way oracle, final verdict
- docs/specs/phase6-spec.md    — CURRENT: sync seqlock (memory ordering, loom, stress, contention)

## Hard rules
1. book is FROZEN/done. sync gains the seqlock; bench gains one benchmark (consumes sync).
2. SEQLOCK is SOUND with NO unsafe: payload fields are atomics accessed Relaxed,
   the version counter (Acquire/Release) carries ordering, torn snapshots are
   detected by the seq check and discarded. The generic UnsafeCell<T> seqlock is a
   DATA RACE (UB) in Rust's model — rejected. unsafe budget is for Phase 7's ring.
3. Memory ordering is ARGUED in comments (each Acquire/Release/fence named) and
   VERIFIED by loom (#[cfg(loom)], consistency-witness model) AND corroborated by a
   real-thread stress test (zero torn reads over millions of iterations).
4. Progress guarantees stated honestly: writer wait-free; readers NOT lock-free
   (starvable). No "lock-free" overclaim.
5. sync runtime deps: none (std atomics; #[repr(align(64))] hand-rolled). loom is a
   DEV-dependency only. sync keeps #![deny(unsafe_op_in_unsafe_fn)] (not forbid).
6. Contention numbers obey the Phase 4 methodology (recorded clock floor, pinning,
   warmup, black_box); writer-independence shown with numbers. seqlock.md is interim.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test; the
loom session also runs --cfg loom green), commit, list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (3 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | SeqLock + ordering + loom | `seqlock.rs` (§2–§3) + unit tests + `loom_seqlock.rs` (§4) | unit tests green; loom run green under `--cfg loom` |
| 2 | Real-thread stress | `stress_seqlock.rs` (§5) | millions of reads, zero witness violations, writer completes budget |
| 3 | Contention benchmark | Benchmark 5 in `bench` (§6) + `seqlock.md` + DoD | `seqlock_read.csv` + plots committed; DoD §9 verified |

Session 1 is the load-bearing one — the ordering and its loom proof. Session 3 reuses the Phase 4 harness. Keep them separate for clean commits and to give the loom run its own window (loom can be slow; bound preemptions if needed).

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase6-spec.md` §1–§4, §8. Update `CLAUDE.md` per Appendix A. Add `loom` as a `[dev-dependencies]` to `sync` (pin via `cargo add --dev`). Execute **Session 1 only**: implement `sync/src/seqlock.rs` exactly per §2–§3 — `TopOfBook`, `#[repr(align(64))] SeqLock`, the loom/std atomic switch, `new`/`store`/`load` with the precise `Acquire`/`Release`/`fence`/`Relaxed` ordering, every ordering commented with its pairing, and the no-torn-read argument in the doc comment. No `unsafe`. Add the §3.1 single-thread unit tests. Write `sync/tests/loom_seqlock.rs` per §4 (one writer, two readers, the stamp-derived consistency witness, `assert_consistent` on every load). Wire re-exports in `sync/src/lib.rs`. Run the three gates AND `RUSTFLAGS="--cfg loom" cargo test -p sync --test loom_seqlock --release` (bound `LOOM_MAX_PREEMPTIONS` and document it if the space is large). `cargo tree -p sync` shows no runtime deps. Commit `feat(sync): seqlock snapshot cell + loom-verified memory ordering`. List changes + the loom result, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase6-spec.md` §5, §8. Execute **Session 2 only**: implement `sync/tests/stress_seqlock.rs` per §5 — one writer storing monotonically-stamped witness snapshots for a bounded budget, `N = max(2, available_parallelism()-1)` reader threads asserting the witness on every `load()` (zero violations across millions of reads), tracking max stamp and retry counts, plus a coarse writer-never-blocks check (writer completes its budget regardless of `N`). std threads only; bounded runtime. Run the three gates. Commit `test(sync): real-thread torn-read stress (zero violations)`. List changes + reads performed and violation count (must be 0), STOP.

**Session 3**
> Read `CLAUDE.md` and `phase6-spec.md` §6, §8, and Phase 4's methodology §3. Execute **Session 3 only**: implement Benchmark 5 (`bench/src/benches/seqlock.rs`) and a `seqlock` subcommand per §6 — pinned writer + `K ∈ {1,2,4,8}` pinned readers, reader `load()` latency + retry count into per-thread `Recorder`s, writer `store` latency recorded separately, writer modes {full_tilt, paced}, `black_box`, recorded clock floor. Write `bench/results/seqlock_read.csv`, render the read-p99-vs-K and writer-store-vs-K plots (cite the CSV), and write `bench/results/seqlock.md` (interim, Writing-Standard-clean: read latency, retry rate under contention, the writer-independence result). Confirm `git diff book-v1-frozen -- book/` empty and `feed/src` unchanged. Run the three gates. Verify Phase 6 DoD §9 item by item and report each. Commit `feat(bench): seqlock read-latency-under-contention benchmark`. STOP. The seqlock primitive is complete.
```

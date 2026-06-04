# CLAUDE.md — Build Guardrail (Low-Latency Market-Data Engine)

## What this repo is
A single-symbol limit-order-book engine built as falsifiable proof-of-work: a frozen
sans-IO core (`book`) driving four order-book implementations, two lock-free concurrency
primitives (`sync`: seqlock + SPMC ring), a deterministic replay feed, a
coordinated-omission-correct bench harness, and a microarchitecture profiling writeup.
The market-data framing is the vehicle; the LOB shootout + primitives + profiling are
the deliverable. Authoritative plan: docs/specs/kickoff-brief.md.

## Hard bans (a reviewer fails the repo on sight if any appear)
- No `f64`/`f32` in `book`, `sync`, `feed` (library), `bench`, or `engine`. Prices and
  quantities are integer ticks/lots (`Px(i64)`, `Qty(i64)`). Floats may exist ONLY inside
  the Phase 3 recorder binary, at the parse edge, converted to integers before anything
  is written to the corpus.
- No async runtime (tokio/async-std) in any measured path. Async is permitted ONLY in the
  Phase 3 `recorder` binary target, never in the replay path, the book, the primitives,
  the harness, or the engine.
- No Redis, no network IPC, no message broker. The hot path is in-process and lock-free.
- No heap allocation in the hot path (the per-event `apply` loop, the seqlock write, the
  SPMC publish). Allocate at setup, not per event.
- No `unsafe` outside the `sync` crate. Every `unsafe` block in `sync` carries a
  `// SAFETY:` justification.

## The corpus boundary (load-bearing)
The recorder's job ends at "exchange float-string -> `Px`/`Qty` integer tick." The corpus
is a flat binary of `BookEvent` records in tick-space. Nothing downstream of the corpus
ever sees a float or a heap `String`. If this boundary blurs, the "no async / no float in
the measured path" claim becomes a lie a reviewer will catch.

## Freeze rule
`book` is frozen after Phase 2 (the differential oracle passing). After that it must drive
every book variant and harness unmodified — exactly as the Rust-Tcp-Server `core` drove all
11 server models unchanged.

## Numbers
Every performance claim traces to a committed file under `bench/results/`. Invent nothing.
No averages without the distribution (p50/p99/p99.9 + histogram). Report coordinated-omission
correctly. An honest negative result with a profile beats a fake win.

## Session discipline
- One phase = one session = one deliverable = one commit = STOP.
- Read docs/specs/phaseN-spec.md before starting; do only what it scopes.
- A session ends ONLY when green: 
    cargo build --workspace --all-targets
    cargo test  --workspace
    cargo clippy --workspace --all-targets -- -D warnings
  all pass, then a single commit, then STOP.
- Never begin the next phase. Future phases are off-limits until their spec exists and is handed over.

## Naming
`book` is the sans-IO core (named `book`, not `core`, to avoid shadowing Rust's built-in
`core` crate). `sync` = primitives. `feed` = event source. `bench` = harness. `engine` = assembly.

## Host
Binaries build with `target-cpu=native` by design (valid microarch profiling). They are
host-specific; the benchmark host is documented in the README.

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

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
- docs/specs/phase6-spec.md    — sync seqlock (memory ordering, loom, stress, contention)
- docs/specs/phase7-spec.md    — sync SPMC broadcast ring (ordering, loom, stress, false-sharing bench)
- docs/specs/phase8-spec.md    — engine end-to-end assembly + production-to-consumption latency
- docs/specs/phase9-spec.md    — top-down microarchitecture teardown (docs/PROFILING.md)
- docs/specs/phase10-spec.md   — CURRENT: DoD close-out + distribution-ready artifacts

## Hard rules
1. System is DONE. Phase 10 writes docs (BENCHMARKS/ARCHITECTURE/README/x-thread/
   SELF-AUDIT), fixes ONE host-dependent test honestly, adds license + CI. book/feed/
   sync primitive logic unchanged; six frozen book files byte-identical; zero unsafe holds.
2. Build EVERY artifact ONLY from committed CSVs + PROFILING.md. Re-derive headline
   numbers from the CSVs; cite each inline; invent nothing.
3. PUBLIC FRAMING = pure low-latency systems / LOB artifact. The microVM-sandbox / AI-
   infra angle is INTERNAL strategy, NOT the public framing. Primitives may be noted as
   general-purpose substrate; do not headline the flagship.
4. Honesty is the signal: FEATURE the real-data inversion, the SortedVec=memory-bound
   refutation (std binary search already branchless), the true-sharing decline, and the
   perf-unavailable-but-PMU-free rigor. No marketing/hype/emoji; numbers carry it.
5. Distribution doctrine: proof-first, findings-not-hype, lead with the link, engage
   technically. Service vs response time never blurred; units + conditions always.
6. cargo test --workspace must end GREEN: fix e2e::high_rate_produces_overruns host-
   robustly (or #[ignore] with a documented reason + note where the property is still
   covered). No silent property deletion.
7. SELF-AUDIT is the HUMAN's gate; the agent writes the study aid (hardest mechanisms +
   canonical explanations), stating the human owns comprehension.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test), commit,
list changes, STOP.

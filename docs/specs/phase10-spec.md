# Phase 10 Specification: The Definition-of-Done Close-Out and Distribution-Ready Artifacts

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md` … `docs/specs/phase9-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 10 spec — the final phase.** The system is built, frozen, verified, measured, and explained: the four-impl `book` (frozen), `feed`, the loom-verified `sync` seqlock and SPMC ring, the `bench` harness, the `engine` pipeline, and `docs/PROFILING.md`. The workspace is `#![forbid(unsafe_code)]`; perf was unavailable and the analysis stands PMU-free.
**Scope:** the close-out the Definition of Done requires — the publishable, sourced, honest writeups (`docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`, the 60-second `README.md`, the distribution thread `docs/x-thread.md`), the self-audit comprehension aid, one test-hygiene fix, and the distribution-readiness polish (license, CI) — so the repo qualifies a senior reader on sight and is ready to ship.
**Audience:** Claude Code. Authoritative. No new system behavior; this phase produces the artifacts that convert finished work into recognized work.

---

## 1. Phase 10 in one paragraph

A finished artifact nobody can read converts to nothing. Ten phases produced a low-latency limit-order-book engine whose every claim is backed by a committed number and whose hardest mechanisms are loom-verified and microarchitecturally explained; this phase makes that legible to a Principal Engineer in sixty seconds and to the systems community in a thread, built strictly from the committed artifacts so it invents nothing and inherits the project's honesty. The distribution leads with the findings that make the work undismissable — that the "obviously optimal" flat array lost by ~100× on real market data because its span blew the cache, that the data-structure crossover is locality-gated not depth-gated, that both lock-free primitives are loom-verified with zero `unsafe` across the whole workspace, and that the microarchitecture analysis is rigorous even though the host denied hardware counters — because an honest negative result with a profile is the elite signal, not the liability. The phase ends with the self-audit the DoD demands: the comprehension gate that the human, not the agent, must pass.

### 1.1 Status, the findings to feature, and the one wart
- **The whole system is done and committed.** Phase 10 adds documentation, one test fix, and distribution polish only.
- **Headline findings (the differentiators) — re-derive each from the committed CSVs, do not copy these numbers blind:**
  - Locality-gated crossover (`service_sweep.csv`): `D*=256` concentrated, `D*=2` uniform.
  - Real-data inversion (`throughput.csv` + `cache_experiment.csv` + `flat_memory.csv`): on the real BTCUSDT book, `BTreeBook` leads (~23 Mev/s) while `FlatBook` collapses (~0.09 Mev/s, ~10,926 ns/event) because its span explodes to ~88 MiB (~11× LLC). "The tradeoff and the failure are one number."
  - Branchless / misprediction (`branch_experiment.csv`): ~35.9 ns misprediction penalty on a branchy search over random keys, eliminated by a `select_unpredictable` cmov; **honest refutation**: `SortedVecBook` is memory-bound, not bad-speculation-bound, because `std::partition_point` is already branchless on this toolchain.
  - Seqlock (`seqlock_read.csv`): read p50 ~11 ns, writer-independent of reader count, 13.45M reads with zero torn-read violations, loom-verified.
  - SPMC ring (`ring_bench.csv`): push p50 ~7–8 ns, recv ~9–10 ns; producer throughput declines with K (true sharing on the write cursor, not false sharing); zero loss/dup/tear over 10.2M deliveries; loom-verified.
  - End-to-end (`e2e.csv`): synthetic pipeline floor ~110–140 ns; CO-correct.
  - **Zero `unsafe`** across all five crates; both lock-free primitives loom-verified; perf unavailable, analysis PMU-free with no fabricated counters.
- **The one wart to fix (§8):** `e2e::high_rate_produces_overruns` fails on a fast host (the consumer never laps), confirmed failing identically on clean HEAD — host-dependent, not a regression. Make `cargo test` green honestly.

### 1.2 Frozen / reused
`book`, `feed`, `sync` primitives untouched. Phase 10 writes docs, may make the **one** host-dependent `engine`/`bench` test robust (engine tests are not frozen), and adds repo-root distribution files (license, CI, README). All numbers come from committed CSVs; PROFILING.md supplies the mechanistic depth. The interim per-phase findings docs (`RESULTS.md`, `seqlock.md`, `ring.md`, `e2e.md`) remain as working notes; `BENCHMARKS.md` consolidates them into the single public artifact.

---

## 2. Framing, audience, distribution doctrine

- **Public framing: a pure low-latency systems artifact.** Present it as a limit-order-book / market-data engine and a study of data structures + lock-free primitives under coordinated-omission-correct measurement. The microVM-sandbox / AI-infra connection is **internal strategy and is not the public framing** (kickoff brief: "strip the web3/terminal framing entirely; it is a pure systems artifact"). `ARCHITECTURE.md` may note the seqlock and SPMC ring are **general-purpose reusable substrate**, without making the flagship the headline.
- **Audience:** a Principal/Staff systems engineer (depth, rigor, honesty) and the broader low-latency / Rust / systems community (the thread). Both must be able to verify claims against committed files.
- **Distribution doctrine (NORTH-STAR §7):** proof-first — the artifact is the opener; post findings, not hype; lead with the link; engage technically; no request-led outreach. The writeups embody this.

---

## 3. `docs/BENCHMARKS.md` — the consolidated public benchmark writeup

The single authoritative, sourced, honest distribution of every committed number. Built **only** from the committed CSVs (`service_sweep`, `read_path`, `sustained`, `throughput`, `flat_memory`, `seqlock_read`, `ring_bench`, `e2e`, `branch_experiment`, `cache_experiment`), with `PROFILING.md` referenced for mechanism. Required structure:
1. **TL;DR** — the handful of headline numbers that matter, each sourced inline, plus the one-line honest verdict ("which structure when").
2. **Environment & methodology** — host CPU/caches/governor, the coordinated-omission correction (response vs service time, never blurred), the measured clock floor, ≥1M-sample cells, pinning, `black_box`; threats to validity (governor, single host, timer floor, perf unavailable → PMU-free). Reproducibility: the exact `bench` commands.
3. **The order-book shootout** — the four impls behind one trait; the locality-gated crossover (`service_sweep`, with the figure); the real-data inversion (`throughput` + `cache_experiment` + `flat_memory`) explained via the memory hierarchy; the read path (`read_path`); the sourced "which structure when" matrix.
4. **The concurrency primitives** — seqlock (`seqlock_read`: read latency, writer-independence, retry rate, loom-verified, zero torn reads); SPMC broadcast ring (`ring_bench`: push/recv latency, the true-sharing-on-write-cursor throughput decline correctly attributed, overrun behaviour, loom-verified, zero loss/dup/tear). State the honest progress guarantees (writer wait-free; readers/consumers not lock-free).
5. **End-to-end pipeline** — CO-correct production-to-consumption latency, saturation, the true-sharing reality (`e2e`).
6. **Microarchitecture** — a summary of the taxonomy (SortedVec memory-bound, BTree memory-bound/pointer-chase, RevVec core/retiring rising with depth, FlatBook retiring until recenter) with the misprediction finding and the `std`-is-already-branchless refutation; link to `PROFILING.md` for depth.
7. **Honest findings & surprises** — the real-data inversion, the SortedVec refutation, perf-unavailable-but-rigorous, zero-`unsafe` loom-verified primitives. Feature these; they are the signal.
8. **Reproducibility & artifacts** — the committed CSVs/plots as the source of truth; how to regenerate.
Writing Standard throughout (§10).

---

## 4. `docs/ARCHITECTURE.md` — the design writeup (the template)

The "load this — it is the template" document. Required structure:
1. **Thesis** — sans-IO discipline, measure-never-guess, one-abstraction-many-implementations, honesty-as-signal — stated, then shown.
2. **The crate DAG** — `book` (frozen sans-IO core) ← `feed`, `sync`, `bench`, `engine`; what each crate owns; the dependency graph as a small diagram (ASCII or mermaid). The async/float quarantine (recorder behind a feature; corpus boundary).
3. **The sans-IO `book`** — one `OrderBook` trait, four implementations, the differential oracle, the freeze (`book-v1-frozen`); how the frozen core drove every harness unmodified.
4. **The corpus boundary** — `feed`: deterministic replay, the quarantined recorder, exact integer (no-`f64`) conversion; why a replayable corpus is the precondition for falsifiable benchmarks.
5. **The lock-free primitives** — the seqlock (single-writer/many-reader, the memory-ordering argument in brief, loom-verified) and the SPMC broadcast ring (atomic-word slots, per-slot stamp, overrun detection, loom-verified); the **zero-`unsafe`** decision and why atomics — not `UnsafeCell` — are the sound tool for concurrent shared mutation under Rust's memory model.
6. **The engine assembly** — the pinned pipeline (replay → apply → seqlock publish → ring broadcast → independent consumers), the overrun→resync composition.
7. **Measurement methodology** — the CO-correct harness; how every number is sourced.
8. **Principles made concrete** — each NORTH-STAR engineering principle mapped to a specific decision in the repo.
9. **General-purpose substrate (brief)** — the seqlock and ring are reusable beyond market data (one neutral sentence; no flagship headline).
A pipeline diagram is required.

---

## 5. `README.md` — the 60-second front door

Pinned-ready; a Principal Engineer must grasp the depth in under a minute. Required:
- One-line statement of what it is.
- 3–5 headline numbers (sourced to BENCHMARKS).
- A compact architecture/pipeline diagram.
- The credibility signals as crisp lines: coordinated-omission-correct benchmarks; two loom-verified lock-free primitives; **zero `unsafe`** workspace-wide; a frozen sans-IO core with a differential oracle; microarchitecture teardown.
- 1–2 of the standout honest findings (the real-data inversion; the locality-gated crossover).
- Build / test / run / reproduce commands (incl. the loom run and the `bench` subcommands).
- Links to `docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`, `docs/PROFILING.md`.
- Badges (build/CI, no-`unsafe`) once CI exists (§9).
Concise, no marketing language; numbers and architecture carry it.

---

## 6. `docs/x-thread.md` — the distribution thread

A technical thread (≈8–14 posts) for the low-latency / Rust / systems community, **findings-led, sourced, no hype**. Requirements:
- **Hook (post 1):** lead with the single most counterintuitive honest finding (the flat array losing ~100× on real data, or the locality-gated crossover) + the repo link. Proof-first.
- **Body:** one finding per post, each concrete and numerical — the crossover, the real-data inversion (with the cache mechanism), the seqlock + ring (loom-verified, zero `unsafe`), the misprediction 2×2 + the `std`-already-branchless refutation, the perf-unavailable-but-PMU-free rigor. Mechanism, not adjectives.
- **Close:** the repo link again and an invitation to technical critique (engage, do not request).
- Keep each post within platform length; mark post breaks clearly. No emoji-as-hype; the voice is the NORTH-STAR voice (opinionated, numbers over adjectives, honest). Every number traceable to BENCHMARKS/PROFILING.

---

## 7. `docs/SELF-AUDIT.md` — the comprehension gate (human-owned)

The DoD's "passing self-audit (I can explain it)" is **the human's** gate, not the agent's. The agent produces the **study aid**: enumerate the hardest mechanisms and give each a canonical, sourced one-paragraph explanation plus a "re-derive from memory" prompt, explicitly framed for the human to self-test. Required mechanisms:
1. The seqlock memory ordering (why no torn read is returned; the Acquire/Release/fence pairings; writer wait-free, readers not lock-free).
2. The SPMC ring overrun + tear detection (the per-slot stamp protocol; why broadcast forbids the `UnsafeCell` shortcut; the resync correctness lesson).
3. The LOB crossover (why locality, not depth, gates it; the retired-work mechanism).
4. The real-data inversion (the memory hierarchy meeting book width; FlatBook's span vs LLC).
5. The branchless/misprediction finding (the 2×2 signature; why `std` is already branchless; `select_unpredictable`).
6. Coordinated-omission correctness (service vs response time; the schedule-relative correction).
The document states plainly that the actual self-audit is Umang's to pass (the human owns comprehension; AI owns mechanical velocity — NORTH-STAR §4).

---

## 8. Test-hygiene fix (make `cargo test` green honestly)

`e2e::high_rate_produces_overruns` is host-dependent: on a fast host the consumer keeps up, no overruns fire, and the `overruns > 0` assertion fails — confirmed failing identically on clean HEAD, so it is not a regression. Fix it honestly (engine/bench tests are not frozen):
- Preferred: make the test **host-robust** — drive overruns deterministically (e.g., a deliberately-stalled consumer / sufficiently small ring + paced producer so a lap is guaranteed regardless of host speed), so the property is still tested. Document the change.
- Acceptable fallback: mark it `#[ignore]` with a clear comment explaining the host-dependence and pointing to the deterministic overrun coverage that already exists (the `sync` ring stress + `engine` pipeline overrun→resync test), so the property remains covered elsewhere.
Either way `cargo test --workspace` must be green, and the change is documented (no silent deletion of a property).

---

## 9. Whole-project DoD + distribution-readiness

**Distribution-readiness polish:**
- `LICENSE-MIT` + `LICENSE-APACHE` at repo root (workspace declared `MIT OR Apache-2.0` in Phase 0); reference them in README.
- A minimal CI workflow (`.github/workflows/ci.yml`): `cargo build --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and (optionally, as a separate job) the loom run. Green CI is the "qualify on sight" badge.
- `.gitignore` already excludes `/target`; confirm committed artifacts (corpora, results, plots) are tracked.

**Final whole-project DoD (NORTH-STAR §6) — verify and report each:**
1. Working system behind a clean abstraction — the frozen `OrderBook` trait + four impls + the lock-free primitives + the engine pipeline.
2. Reproducible benchmark with committed numbers — all CSVs/plots committed; exact commands documented.
3. Teardown writeup — `BENCHMARKS.md` + `PROFILING.md`, authoritative, sourced, honest.
4. 60-second README — present, pinned-ready.
5. Passing self-audit — `SELF-AUDIT.md` study aid present; the human gate noted.
6. Distribution ready — `x-thread.md`, license, CI; lead-with-the-link doctrine honored.
7. Honesty intact — surprises/refutations featured, not hidden.
8. Invariants intact — six frozen `book` files byte-identical to `book-v1-frozen`; `book/`+`feed/`+`sync/src` primitive logic unchanged this phase; zero `unsafe` workspace-wide; `cargo test --workspace` green; quarantine (`cargo tree -p bench` no `tokio`).
9. `cargo build`/`clippy -D warnings`/`test` clean; meaningful conventional commits on `main`.

---

## 10. Engineering & Writing Standard — governs this phase

1. **Build only from committed data.** Every number in every artifact cites its CSV (or PROFILING.md). Re-derive headlines from the CSVs; invent nothing.
2. **Honesty is the signal.** Feature the real-data inversion, the SortedVec refutation, the true-sharing decline, the perf-unavailability — these make the work undismissable. No marketing words, no emoji-as-hype, no exclamation.
3. **Public framing is the pure systems artifact.** Flagship/AI-infra is internal; primitives noted as general-purpose substrate at most.
4. **Service vs response time never blurred; units + conditions always.** The reader can reproduce any claim.
5. **Concise and senior.** Numbers and architecture carry the weight; the 60-second README earns its name.
6. **One test fix, documented; no silent property deletion.**
7. **Green-gate discipline.** `cargo build`/`clippy -D warnings`/`test` green before each commit; one session → meaningful conventional commit → STOP. Never commit red.

---

# Appendix A — `CLAUDE.md` update for Phase 10

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md … phase9-spec.md  (as before)
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
```

---

# Appendix B — Claude Code execution plan (3 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | BENCHMARKS + test fix | `docs/BENCHMARKS.md` (§3) + the §8 host-dependent test fix | writeup sourced + Writing-Standard-clean; `cargo test` green |
| 2 | ARCHITECTURE + polish | `docs/ARCHITECTURE.md` (§4) + LICENSE files + CI workflow | architecture writeup + diagram; license + CI committed |
| 3 | README + thread + self-audit + DoD | `README.md` (§5), `docs/x-thread.md` (§6), `docs/SELF-AUDIT.md` (§7), final DoD (§9) | 60-second README; thread findings-led; self-audit aid; whole-project DoD verified |

Session 1 is the largest writeup and unblocks the green-test claim. Session 2 is the architecture doc + repo polish. Session 3 is the front door, the distribution thread, the self-audit aid, and the final whole-project verification.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase10-spec.md` §1–§3, §8, §10. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: first, fix `e2e::high_rate_produces_overruns` per §8 — make it host-robust by guaranteeing a lap (stalled consumer / small ring + paced producer) so the overrun property is tested regardless of host speed; if that is impractical, `#[ignore]` it with a comment explaining the host-dependence and pointing to the existing deterministic overrun coverage. `cargo test --workspace` must end green. Then author `docs/BENCHMARKS.md` per §3 — the consolidated public benchmark writeup built ONLY from the committed CSVs (re-derive every headline number from them; cite each inline), covering TL;DR, environment & methodology (CO-correctness, clock floor, governor, perf-unavailable → PMU-free), the order-book shootout (locality-gated crossover + real-data inversion explained via the cache hierarchy + the which-structure-when matrix), the concurrency primitives (seqlock + ring, loom-verified, honest progress guarantees), end-to-end latency, a microarchitecture summary linking PROFILING.md, and the honest-findings section featuring the inversion + the SortedVec refutation + perf-unavailability + zero-unsafe. Obey the Writing Standard; re-read and fix violations. Touch no frozen code. Run the three gates. Commit `docs: consolidated benchmark writeup + host-robust e2e test`. List changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase10-spec.md` §4, §9, §10. Execute **Session 2 only**: author `docs/ARCHITECTURE.md` per §4 — thesis (sans-IO, measure-never-guess, one-abstraction-many-impls, honesty), the crate DAG with a diagram, the frozen sans-IO `book` + differential oracle + freeze, the corpus boundary (feed + quarantine + exact integer conversion), the two lock-free primitives + the zero-`unsafe` decision (why atomics not `UnsafeCell`), the engine assembly + overrun→resync, the measurement methodology, NORTH-STAR principles mapped to concrete decisions, and one neutral sentence on general-purpose substrate (no flagship headline). Include the pipeline diagram. Add `LICENSE-MIT` and `LICENSE-APACHE` at repo root and a minimal `.github/workflows/ci.yml` (build + clippy -D warnings + test; optional separate loom job). Confirm the freeze + zero-`unsafe` invariants. Run the three gates. Commit `docs: architecture writeup + license + CI`. List changes, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase10-spec.md` §5–§7, §9, §10. Execute **Session 3 only**: author the 60-second `README.md` per §5 (what it is, 3–5 sourced headline numbers, a compact pipeline diagram, the credibility signals incl. CO-correct / loom-verified / zero-`unsafe` / frozen-core+oracle / microarch teardown, 1–2 standout honest findings, build/test/run/reproduce commands, links to BENCHMARKS/ARCHITECTURE/PROFILING, CI + no-`unsafe` badges); `docs/x-thread.md` per §6 (a findings-led, sourced, hype-free technical thread leading with the most counterintuitive honest finding + the link, one finding per post, closing with the link + an invitation to critique); and `docs/SELF-AUDIT.md` per §7 (the six hardest mechanisms with canonical sourced explanations + re-derive prompts, framed as the human's comprehension gate). Build everything from committed data; obey the Writing Standard; public framing is the pure systems artifact. Confirm the §9 final whole-project DoD item by item and report each, plus the freeze + zero-`unsafe` + green-test invariants. Run the three gates. Commit `docs: README, distribution thread, and self-audit (project close-out)`. STOP. The project is complete and distribution-ready.
```

# Phase 9 Specification: The Top-Down Microarchitecture Teardown (`docs/PROFILING.md`)

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md` … `docs/specs/phase8-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 9 spec.** All code is built and measured: the frozen four-impl `book`, `feed`, the loom-verified `sync` primitives, the `bench` harness, and the `engine` end-to-end pipeline. The workspace is `#![forbid(unsafe_code)]`.
**Scope:** the microarchitecture teardown the kickoff brief names the highest-signal artifact — a top-down (TMAM) analysis of the hot `apply` loop that *explains* the Phase 4/5 latency crossover and the real-data verdict mechanistically, with a bad-speculation → branchless demonstration, written up in `docs/PROFILING.md`.
**Audience:** Claude Code. Authoritative. This phase produces no shipped behavior change; it produces evidence and the writeup that interprets it. Every number is sourced; the analysis is robust to whether hardware counters are available.

---

## 1. Phase 9 in one paragraph

Every prior phase measured *what* the system does; this phase explains *why* at the level the CPU actually executes. The four frozen book implementations form a natural microarchitectural taxonomy — binary search is bound by branch misprediction, the B-tree by memory latency, the linear scan by retired work as depth grows, the flat array by nothing until its span blows the cache — and a top-down analysis with hardware counters maps each to its dominant bottleneck and thereby explains the Phase 4 crossover and the Phase 5 real-data inversion from first principles. The brief's "bad-speculation → branchless" story is delivered two ways at once: structurally, because `FlatBook`'s direct index *is* the branchless answer the shootout converged on, and instrumentally, by a focused branchless-binary-search experiment that quantifies the misprediction penalty the frozen `SortedVecBook` pays. Because the host may not grant PMU access, the analysis is built to stand on PMU-free behavioral signatures — the misprediction signature (branchy code is slow only on unpredictable input; branchless code is flat) and the cache-hierarchy signature (latency steps at L1/L2/LLC footprint boundaries) — with `perf` counters as confirming evidence when available, never as a single point of failure.

### 1.1 What we already know (to be explained mechanistically) and the constraints
From committed data:
- **Crossover is locality-gated** (`service_sweep.csv`): `SortedVecBook` ~14 ns flat; `RevVecBook` near-free when touches are top-concentrated but ~697 ns at uniform depth-2048; `FlatBook` flat ~14 ns. Phase 9 must explain *why* binary search is flat-but-not-fastest, why linear scan degrades, and why the flat array is constant.
- **Real-data inversion** (`throughput.csv`): on the real wide book, `BTreeBook` leads and `FlatBook` collapses (recenter storm). Phase 9 must explain this via the memory hierarchy.
- **Two hard constraints:**
  1. **The `book` is frozen** (six logic files byte-identical to `book-v1-frozen`; the only past edit was the Phase 5 `FlatBook` addition). Phase 9 **does not modify any book impl.** The branchless rewrite is a `bench` experiment, not a change to the shipped core.
  2. **`perf`/PMU access is not guaranteed** in the build environment. Phase 9 collects hardware counters **if available** and **always** collects the PMU-free behavioral evidence, so the writeup is complete either way.

### 1.2 Frozen / reused
`book`, `feed`, `sync` untouched. All new code is profiling scaffolding in `bench` (a `profile` subcommand + the experiments) and the `docs/PROFILING.md` writeup. The branchless variant is a `bench`-local function, explicitly not a book impl.

---

## 2. The top-down method (TMAM) and counter collection

### 2.1 The taxonomy to confirm
Intel Top-Down Microarchitecture Analysis classifies issue-slots into **Retiring**, **Bad Speculation**, **Frontend Bound**, **Backend Bound** (= **Memory Bound** + **Core Bound**). Hypotheses (to confirm or refute with evidence):
| Impl | `apply` locate cost | Predicted dominant category |
|---|---|---|
| `SortedVecBook` | binary search (data-dependent branch) | **Bad Speculation** (branch-miss) |
| `BTreeBook` | node pointer chase | **Memory Bound** (LLC/load latency) |
| `RevVecBook` | linear scan from best | **Retiring/Core Bound**, rising with depth (more retired iterations) |
| `FlatBook` | direct index | **Retiring** (minimal); **Memory Bound** only during recenter / wide span |

### 2.2 The isolated hot-loop harness (`bench profile`)
Add a `bench profile` subcommand that runs a **tight, untimed** hot loop so an external profiler attributes cycles to `apply` cleanly: build a book at depth D (untimed), warm up, then execute `--iters N` `apply` calls at the chosen locality, each wrapped in `black_box` to defeat elision, with **no per-op timing** (the profiler measures externally). Args: `--impl {btree|sorted|rev|flat} --op {apply|search} --depth D --locality {concentrated|uniform} --iters N`. Pin the thread; honor `target-cpu=native` (already set).

### 2.3 perf invocation (run if available; capture raw output)
```
perf stat -e cycles,instructions,branches,branch-misses,\
L1-dcache-loads,L1-dcache-load-misses,LLC-loads,LLC-load-misses \
  ./target/release/bench profile --impl sorted --op apply --depth 2048 --locality uniform --iters 200000000

perf stat --topdown ./target/release/bench profile --impl <X> ...   # TMAM L1 (Tiger Lake supports it)
```
Capture raw output to `bench/results/perf/<impl>_<op>_d<D>_<locality>.txt`. Parse into `bench/results/perf/perf_summary.csv`:
```
impl,op,depth,locality,iters,cycles,instructions,ipc,branch_miss_rate,l1d_miss_rate,llc_miss_rate,td_retiring,td_bad_spec,td_frontend,td_backend
```
(TMAM columns blank if `--topdown` unsupported.) **If perf is unavailable or permission-denied** (`perf_event_paranoid` too high / no `CAP_PERFMON`): record that fact in `perf_summary.csv` as a note and in `PROFILING.md` as a threat to validity, and rely on §3. Do **not** fabricate counters.

---

## 3. PMU-free behavioral evidence (always required — the robustness)

These experiments prove the mechanisms *without* hardware counters, using the cycle-accurate `quanta` clock. They are mandatory; perf is corroboration.

### 3.1 The branch-misprediction signature (`bench branch-exp` → `branch_experiment.csv`)
Branch misprediction has an observable signature: a branchy search is **slow only on unpredictable input**, while a branchless search is **flat across input predictability**. Measure a 2×2:
- **variant** ∈ {`branchy` (std `binary_search`/`partition_point` over the level array), `branchless` (§4)}
- **key_pattern** ∈ {`predictable` (sequential / repeated keys → the comparison branch is predicted well), `random` (uniform random keys via `feed::rng::SplitMix64` → comparisons mispredict)}
Over a sorted level array at depth D (sweep D), ≥10M lookups per cell, `black_box` inputs/outputs, recorded clock floor.
```
variant,key_pattern,depth,samples,clock_overhead_ns,p50_ns,p99_ns,mean_ns
```
**Expected signature:** `branchy/random` ≫ `branchy/predictable`; `branchless/{predictable,random}` ≈ flat and ≈ `branchy/predictable`. The `branchy/random − branchy/predictable` gap *is* the misprediction penalty, measured with no PMU. The flatness of `branchless` proves the data-dependent branch was the cause.

### 3.2 The cache-hierarchy signature (`bench cache-exp` → `cache_experiment.csv`)
Read the host cache sizes from `/sys/devices/system/cpu/cpu0/cache/index*/size`. Sweep book depth so the per-side footprint crosses L1 → L2 → LLC, measuring `apply` p50 per impl; annotate the footprint (`depth * size_of::<(Px,Qty)>()` for the Vecs; node estimate for BTree) and the cache boundaries.
```
impl,depth,footprint_bytes,cache_level_crossed,samples,clock_overhead_ns,p50_ns,p99_ns
```
**Expected signature:** contiguous Vecs degrade gracefully and stay flat while resident; `BTreeBook` shows elevated latency even when resident (scattered nodes defeat the prefetcher) and steps at boundaries; `FlatBook` is flat until its span exceeds the cache (the real-book regime). This is the PMU-free memory-bound story and directly explains the real-data inversion.

---

## 4. The branchless rewrite experiment (bench-local; not a book change)

Implement, in `bench`, a branchless lower-bound to contrast with the frozen branchy binary search:
```rust
/// Branchless lower_bound over a sorted slice: the comparison becomes a cmov,
/// so there is no data-dependent branch and latency is independent of key
/// predictability. (Experiment only — the frozen SortedVecBook is unchanged.)
#[inline]
fn branchless_lower_bound(arr: &[i64], key: i64) -> usize {
    let mut base = 0usize;
    let mut len = arr.len();
    while len > 1 {
        let half = len / 2;
        let mid = base + half;
        base = if arr[mid] < key { mid } else { base }; // cmov, not a branch
        len -= half;
    }
    base + usize::from(arr.get(base).is_some_and(|&v| v < key))
}
```
Verify it matches `std`'s result on randomized inputs (a correctness unit test against `partition_point`), then use it as the `branchless` variant in §3.1. Report the before/after: the misprediction penalty eliminated, sourced to `branch_experiment.csv` (and, if perf ran, the branch-miss-rate drop in `perf_summary.csv`).

**Framing for `PROFILING.md` (state explicitly):** the frozen core is *not* changed, for two reasons — (1) the freeze doctrine (the core drives everything unmodified), and (2) the real-data verdict already chose `FlatBook`, whose direct index is the branchless answer *structurally*, so the per-instruction branchless binary search is a quantified alternative, not a needed patch. This is the honest resolution of "branchless rewrite" under a frozen core: the principle is realized by the winning data structure; the experiment measures the headroom of the instruction-level alternative.

---

## 5. Data artifacts

Committed under `bench/results/perf/`:
- Raw `perf stat` / `--topdown` outputs (`*.txt`) — if perf available.
- `perf_summary.csv` — parsed counters (or the unavailability note).
- `branch_experiment.csv` (§3.1), `cache_experiment.csv` (§3.2).
- Plots (plotters, cite their CSV): the misprediction 2×2 (branchy vs branchless, predictable vs random) vs depth; the cache-footprint latency curve with L1/L2/LLC boundaries annotated.

---

## 6. `docs/PROFILING.md` — the top-down narrative (the elite artifact)

Built **only** from committed data (§5 + the existing Phase 4/5/7/8 CSVs). Required structure:
1. **Method & environment.** TMAM top-down in one paragraph; the host CPU, cache sizes (from `/sys`), governor; **PMU availability** stated plainly; the PMU-free corroboration approach; threats to validity (governor, single host, timer floor, perf permissions).
2. **The hot loop.** What `apply` does per impl (the locate step is the variable).
3. **The taxonomy, confirmed.** Each impl → its dominant top-down category, backed by counters (if available) **and** its PMU-free signature: `SortedVecBook` = Bad Speculation, `BTreeBook` = Memory Bound, `RevVecBook` = Core/Retiring rising with depth, `FlatBook` = Retiring (until recenter = Memory Bound).
4. **Bad-speculation → branchless.** The §3.1 misprediction signature (the 2×2, sourced) and the §4 branchless before/after; the explicit freeze + "FlatBook is the structural branchless answer" framing.
5. **Memory-bound story.** The §3.2 cache-footprint curve; pointer-chase vs contiguous; where the real book exceeds LLC, explaining `FlatBook`'s recenter collapse and `BTreeBook`'s real-data lead.
6. **Closing the loop.** How the microarchitecture *explains* the Phase 4 crossover and the Phase 5 real-data inversion — the mechanistic "why" behind every prior number. This is the payoff: measure-then-explain, end to end.
7. **Reproducibility.** Exact `bench profile` / `perf` / experiment commands and the environment.

**Writing Standard (paramount here):** declarative and sourced (every figure cites its CSV; e.g. "branchy/random p50 = X ns vs branchy/predictable = Y ns at depth D → ~(X−Y) ns misprediction penalty (`branch_experiment.csv`)"); units + conditions always; no marketing, no emoji; honesty is the signal — if perf was unavailable, say so and lean on the PMU-free evidence; if a hypothesis was refuted (e.g. SortedVec turns out memory-bound at some depth, not bad-spec), report that with the number; never blur the experiments. This is the highest-signal artifact in the repo — it must read as written by a senior low-latency engineer, numbers carrying the weight.

---

## 7. Optional extension — pipeline-stage breakdown

If session budget allows (do not jeopardize §1–§6): decompose the Phase 8 synthetic end-to-end floor (~110–140 ns) into stages — `apply`, `seqlock.store`, `ring.push`, cross-core propagation, `recv` — by timing each stage in isolation in the engine producer/consumer path (a `bench e2e-breakdown`), committing `e2e_breakdown.csv` and a short addendum to `PROFILING.md`. Explicitly optional; the apply-loop teardown (§2–§6) is the required core.

---

## 8. Engineering Standard — governs this phase

1. **Explain the measurements; invent nothing.** Every claim in `PROFILING.md` traces to a committed file; the analysis interprets data, it does not assert.
2. **Robust to the environment.** PMU-free evidence is mandatory and self-sufficient; perf is corroboration. Never fabricate counters; record unavailability honestly.
3. **Frozen respect.** No book impl changes. The branchless variant is a `bench` experiment, clearly framed.
4. **Measurement hygiene.** Isolated hot loop, warmup, pinning, `black_box`, recorded clock floor, ≥10M samples per experiment cell; governor recorded.
5. **Honesty is the signal.** Refuted hypotheses reported with numbers; the freeze + FlatBook framing stated; threats to validity enumerated.
6. **Green-gate discipline.** `cargo build`/`clippy -D warnings`/`test` green before each commit; one session → meaningful conventional commit → STOP. Never commit red.

---

## 9. Phase 9 Definition of Done

1. `bench profile` (isolated hot loop), `bench branch-exp` (§3.1 + the §4 branchless variant with its correctness test), `bench cache-exp` (§3.2) implemented; `#![forbid(unsafe_code)]` holds; zero-`unsafe` grep still clean.
2. Data collected and committed: `perf_summary.csv` + raw `perf` outputs **if available** (else the documented unavailability note); `branch_experiment.csv` showing the misprediction signature; `cache_experiment.csv` showing the footprint/cache signature; the two plots citing their CSVs.
3. The branchless lower-bound matches `std` on randomized inputs (correctness test green) and yields the flat misprediction profile.
4. `docs/PROFILING.md` complete per §6: TMAM taxonomy confirmed per impl (counters and/or PMU-free signatures), the bad-speculation→branchless story with the freeze/FlatBook framing, the memory-bound story, and the mechanistic explanation of the Phase 4 crossover + Phase 5 real-data inversion — every number sourced, Writing-Standard-clean, PMU availability + governor stated.
5. Freeze & quarantine: six frozen-logic `book` files byte-identical to `book-v1-frozen`; `book/`+`feed/`+`sync/src` untouched this phase (`git diff <pre-phase-9-commit>..HEAD -- book/ feed/ sync/src` empty); `cargo tree -p bench` shows no `tokio`.
6. `cargo build`/`clippy -D warnings`/`test` clean at every commit; meaningful conventional commits on `main`.

After Phase 9 the project's measurements are explained at the microarchitecture level. Next is Phase 10 (the DoD close-out: `BENCHMARKS.md`, `ARCHITECTURE.md`, the 60-second README, the distribution thread).

---

# Appendix A — `CLAUDE.md` update for Phase 9

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md … phase8-spec.md  (as before)
- docs/specs/phase9-spec.md    — CURRENT: top-down microarchitecture teardown (docs/PROFILING.md)

## Hard rules
1. book/feed/sync are FROZEN/done. Phase 9 adds ONLY bench profiling scaffolding
   (profile/branch-exp/cache-exp subcommands + a bench-local branchless variant)
   and docs/PROFILING.md. No book impl is modified — the branchless rewrite is an
   EXPERIMENT in bench, not a change to the frozen core.
2. PMU access is NOT guaranteed. Collect perf counters IF available (raw output +
   perf_summary.csv); ALWAYS collect the PMU-free behavioral evidence:
   - misprediction signature: branchy slow only on RANDOM keys; branchless flat.
   - cache signature: latency vs footprint crossing L1/L2/LLC (read sizes from /sys).
   Never fabricate counters; record unavailability as a threat to validity.
3. The four frozen impls are a microarchitecture taxonomy: SortedVec=Bad Speculation,
   BTree=Memory Bound, RevVec=Core/Retiring(rising with depth), FlatBook=Retiring
   (until recenter=Memory Bound). PROFILING.md confirms each and EXPLAINS the Phase 4
   crossover + Phase 5 real-data inversion mechanistically.
4. The branchless answer is realized STRUCTURALLY by FlatBook's direct index; the
   bench branchless-binary-search experiment QUANTIFIES the instruction-level
   alternative. State the freeze + FlatBook framing; don't patch the core.
5. Measurement hygiene: isolated untimed hot loop for perf; black_box; pinning;
   warmup; recorded clock floor; >=10M samples/cell; governor recorded.
6. PROFILING.md is built ONLY from committed data, Writing-Standard-clean. Highest-
   signal artifact: numbers carry the weight, honesty (incl. refuted hypotheses,
   perf unavailability) is the signal.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test),
commit, list changes + headline numbers, STOP.
```

---

# Appendix B — Claude Code execution plan (2 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | Profiling harness + experiments + data | `bench profile/branch-exp/cache-exp` + branchless variant; perf (if avail) + PMU-free CSVs + plots | CSVs committed; branchless correctness green; misprediction + cache signatures captured |
| 2 | The writeup | `docs/PROFILING.md` (§6) + DoD | top-down narrative complete, sourced, Writing-Standard-clean; DoD §9 verified |

Session 1 collects the evidence; Session 2 writes the elite artifact from it. Keep them separate so the data is reviewable before the writeup and the writeup gets its own clean commit.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase9-spec.md` §1–§5, §8. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement in `bench` the `profile` subcommand (§2.2: isolated untimed `apply` hot loop, `--impl/--op/--depth/--locality/--iters`, pinned, `black_box`), the `branch-exp` subcommand (§3.1: branchy vs the §4 `branchless_lower_bound` × predictable vs random keys via `feed::rng::SplitMix64`, ≥10M lookups/cell, sweep depth), and the `cache-exp` subcommand (§3.2: read host cache sizes from `/sys`, sweep depth across L1/L2/LLC, `apply` p50 per impl, footprint-annotated). Add a correctness test that `branchless_lower_bound` matches `std`'s `partition_point` on randomized inputs. Run perf if available (`perf stat` + `--topdown`, raw → `bench/results/perf/*.txt`, parsed → `perf_summary.csv`); if perf is unavailable/denied, record that note and proceed. Write `branch_experiment.csv`, `cache_experiment.csv`, and the two plots (cite their CSVs). `#![forbid(unsafe_code)]`; touch no frozen code. Run the three gates. Commit `feat(bench): microarchitecture profiling harness + branch/cache experiments`. Report the misprediction 2×2 headline + whether perf was available, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase9-spec.md` §6–§9, and the Phase 4/5 CSVs. Execute **Session 2 only**: author `docs/PROFILING.md` per §6 — method & environment (TMAM, host caches from `/sys`, governor, PMU availability, threats to validity); the hot loop; the confirmed taxonomy per impl (counters if available AND PMU-free signatures); the bad-speculation→branchless story (the §3.1 2×2 + §4 before/after + the freeze/FlatBook framing); the memory-bound story (§3.2 curve, pointer-chase vs contiguous, real-book-exceeds-LLC); and the closing mechanistic explanation of the Phase 4 crossover + Phase 5 real-data inversion. Build it ONLY from committed data; cite each figure to its CSV; obey the Writing Standard (re-read and fix violations). Optionally (only if budget allows, §7) add the pipeline-stage breakdown. Confirm the §9.5 freeze check and the zero-`unsafe` grep. Run the three gates. Verify Phase 9 DoD §9 item by item and report each. Commit `docs: top-down microarchitecture teardown (PROFILING.md)`. STOP. The profiling artifact is complete.
```

# SELF-AUDIT — the comprehension gate

A repo is "done" only when its hardest mechanisms can be re-derived from memory. The
Definition of Done's "passing self-audit (I can explain it)" is **the human's gate, not
the agent's** — AI owns mechanical velocity; the human owns comprehension (NORTH-STAR §4).
Generation must never outrun comprehension.

This document is the **study aid**, not the audit itself. It enumerates the six hardest
mechanisms in the system, gives each a canonical, sourced explanation, and ends each with
a *re-derive from memory* prompt. The actual self-audit is Umang's to pass: close this
file, answer the prompt cold, and check back against the explanation and its source. If
you cannot re-derive it, you do not yet own it.

Sources: the code under `book/`, `sync/`, `engine/`, the committed CSVs under
`bench/results/`, [`BENCHMARKS.md`](BENCHMARKS.md), and [`PROFILING.md`](PROFILING.md).

---

## 1. The seqlock memory ordering — why no torn read is ever returned

**Canonical explanation.** The seqlock (`sync/src/seqlock.rs`) publishes a `TopOfBook`
under an `AtomicU32` version counter `seq` (even = stable, odd = write in progress). The
single writer's `store`: load `seq`, set it odd (`Relaxed`), emit a **`Release` fence
(W1)**, write the five payload words (`Relaxed`), then set `seq` even again with a
**`Release` store (W2)**. The reader's `load`: take `seq` with an **`Acquire` load (R1)`**;
if odd, retry — a write is mid-flight. If even, read the payload (`Relaxed`), emit an
**`Acquire` fence (R2)`**, re-read `seq` (`Relaxed`), and accept the snapshot only if it
is **even and unchanged** from R1. The ordering is carried entirely by `seq`: (W1) orders
the odd-marker ahead of the payload writes, so a reader cannot see new payload while `seq`
still looks even; (W2)'s `Release` pairs with (R1)'s `Acquire` so a reader observing the
even value sees all payload writes; (R2) prevents the second `seq` read from being hoisted
above the payload loads. If a write straddles the read, the second `seq` read differs (or
is odd) and the snapshot — possibly torn — is **discarded and retried**, never returned.
The payload is atomic words accessed `Relaxed`, so a straddling overwrite is *stale data*,
never a data race. The writer is **wait-free** (it never inspects a reader, so reader
count does not tax it — `seqlock_read.csv`: write p50 flat 10/10/11 ns at K=1/2/4);
readers are **not lock-free** (a fast enough writer forces a retry, though the measured
retry rate is ≤0.0053/load).

**Re-derive from memory.** Write out the `store` and `load` sequences with each atomic's
ordering. Which two operations pair to publish the payload? What does the `Acquire` *fence*
in the reader prevent? Why is the payload `Relaxed` and not UB? Why is the writer wait-free
but the reader not lock-free?

---

## 2. The SPMC ring — overrun and tear detection via the per-slot stamp

**Canonical explanation.** The ring (`sync/src/ring.rs`) is a single-producer broadcast
bus; each slot holds `[AtomicU64; W]` words plus an `AtomicU64` **stamp** encoding the
position that last wrote it (and a WRITING bit). Producer `push` at position `p`: store
`stamp = p | WRITING` (**P1**), `Release` fence, overwrite the words (`Relaxed`, **P2**),
publish `stamp = p` (**`Release`, P3**), advance the write cursor (**`Release`, P4**).
Consumer `try_recv` at `cursor`: load the write cursor (`Acquire`, R0) — if `cursor >= w`,
`Empty`; else read the slot's `stamp` (`Acquire`, R1) and require it equal `cursor`
exactly; read the words (`Relaxed`, R2); `Acquire` fence (**R3**); re-read the stamp and
require it still equal `cursor`. The stamp is the whole protocol: if it is not `cursor`,
the slot already holds a *later* generation — the consumer was **lapped** (overrun); if it
changes *between* R1 and R3, the producer began overwriting mid-read — a **tear**, and the
(possibly torn) value is discarded. The (P1) mark-busy ordered ahead of the word writes is
what makes a straddling overwrite visible to R3. **Why broadcast forbids the `UnsafeCell`
shortcut:** a non-atomic memcpy of the payload under the stamp guard is a data race (UB)
under Rust's memory model — multiple consumers read the same bytes the producer may be
writing, with no happens-before edge; the later stamp-check discard does not retroactively
define the race. Atomic words make a torn read *stale-but-valid*, hence defined. **The
resync correctness lesson:** on overrun, `resync` re-loads the write cursor **freshly**
(not the stale R0 snapshot) — under a full-capacity lap the R0 value can be `capacity`
behind, and computing `oldest` from it could move the cursor *backward* and re-deliver seen
positions. A fresh load guarantees `oldest >= cursor`, so the cursor stays monotonic and
`skipped` is exact (caught by the stress test; loom's tiny model could not reach a
full-`cap` lap in that window).

**Re-derive from memory.** What does the stamp encode, and what are the two distinct
failures it detects (lapped vs torn)? Why must (P1) precede the word writes? Why is the
`UnsafeCell` + memcpy version a data race even though the stamp would discard the torn
read? Why does `resync` re-read the write position instead of reusing the R0 snapshot?

---

## 3. The order-book crossover — why locality, not depth, gates it

**Canonical explanation.** `service_sweep.csv` (`update` p50) shows the depth `D*` at which
the best-first linear scan (`RevVecBook`) loses to the binary search (`SortedVecBook`)
depends on **touch locality**, not on depth: `D*=256` under concentrated (top-of-book)
touches, `D*=2` under uniform touches. The mechanism is **retired instruction count**.
`RevVecBook`'s locate cost is its scan length = the number of levels it walks from the
best price. Concentrated touches land 1–2 levels from the top regardless of book depth, so
the scan is short and flat in depth (rev p50 ~8 ns at depths 8–128 concentrated). Uniform
touches spread across the ladder, so the expected scan is ≈depth/2 — linear in depth,
reaching **697 ns at depth 2048** against the binary search's depth-robust **14 ns** and
the flat array's **14 ns**. This is confirmed to be retired work, not a cache effect, by
`cache_experiment.csv`: `RevVecBook` costs 5,271 ns at depth 16384 on a 262 KiB **L2-
resident** footprint — too fast-fitting to be a cache miss, too slow to be anything but
instruction count. And it is not misprediction: the scan has no binary search, and
`branch_experiment.csv` shows even binary searches are branchless here.
([`PROFILING.md`](PROFILING.md) §6.1.)

**Re-derive from memory.** Why does the same structure win at depth 2048 under
concentrated touches and lose catastrophically under uniform touches? What is `RevVecBook`
actually paying for, and what single experiment proves it is retired work and not a cache
miss?

---

## 4. The real-data inversion — the memory hierarchy meeting book width

**Canonical explanation.** `throughput.csv` (ns/event) shows the ranking invert between
synthetic and real data: on `steady`, `FlatBook` leads (8.85) and `BTreeBook` trails
(22.30); on the real `btcusdt-sample`, `BTreeBook` leads (43.05) and `FlatBook` collapses
to **10,926.62 ns/event** — last by ~254×. The mechanism is **book width meeting the cache
hierarchy**. `FlatBook`'s memory is proportional to price *span*, not to occupied levels:
`flat_memory.csv` gives a per-side span of 131,088 bytes (L2-resident) on every synthetic
corpus but **92,049,312 bytes (~88 MiB, ~11× the 8 MiB LLC)** on the real BTCUSDT book,
whose prices range over millions of ticks. Across that span every access is a guaranteed
LLC/DRAM miss, and each event at a new extreme price triggers an `ensure_range`
recenter/grow that reallocates and copies the whole array — a recenter storm. `BTreeBook`
wins precisely because its memory tracks the *number of levels*, not the span, so its
compact `O(log n)` nodes absorb a wide, sparse book. The structure optimal for a tiny
dense set is pessimal for a huge sparse one; "the tradeoff and the failure are one number"
— the 88 MiB span. ([`PROFILING.md`](PROFILING.md) §5.2, §6.2.)

**Re-derive from memory.** What is `FlatBook`'s memory proportional to, and what is
`BTreeBook`'s? Give the one number (and its cache multiple) that explains both the win on
synthetic and the collapse on real data. Why is a single in-span flat access cheap but a
real-corpus stream of them ruinous?

---

## 5. The branchless / misprediction finding — the 2×2 and why `std` is already branchless

**Canonical explanation.** `branch_experiment.csv` measures a lower-bound search as a 2×2
of variant (branchy / branchless / std) × key predictability (predictable / random),
swept by depth, with the array L1-resident so memory cannot confound it. At depth 256 the
**branchy** search is 7.019 ns on predictable keys and **36.093 ns on random keys** —
a **+29.07 ns pure-misprediction penalty** (each of ~8 comparisons mispredicts ~half the
time on random keys, ~15+ cycles per flush). The **branchless** variant (driven by
`std::hint::select_unpredictable`, a `cmov`) and the **`std`** variant
(`slice::partition_point`) are **flat** across predictability (3.118 ns predictable vs
3.118 ns random at depth 16). This **refutes** the predicted "SortedVecBook is
bad-speculation-bound": `std`'s binary search already compiles branchless on this toolchain
(rustc 1.95), so the shipped sorted book pays no misprediction penalty — it is memory-bound
by its dependent-load chain (§3 of [`PROFILING.md`](PROFILING.md)). The penalty is real but
already dodged: structurally by `FlatBook`'s direct index (no search at all) and
instrumentally by `std`'s branchless search. A recorded subtlety: the naive ternary
`select` form lowered *back* to a conditional jump on this toolchain; only
`select_unpredictable` reliably pinned the `cmov`. ([`PROFILING.md`](PROFILING.md) §4.)

**Re-derive from memory.** Sketch the 2×2 and the one cell that isolates misprediction.
Why must the array be L1-resident for that isolation? Why is the predicted
"bad-speculation-bound" verdict refuted for the shipped code, and what makes the residual
gap at large depth memory rather than misprediction?

---

## 6. Coordinated-omission correctness — service vs response time

**Canonical explanation.** The two questions a latency study asks are different and must
never be blurred. **Service time** is the cost of the operation itself, with no arrival
process — the service sweep, read path, throughput, and primitive micro-benchmarks measure
it. **Response time** is `completion − scheduled_arrival` under an open-loop arrival
schedule — the sustained (`sustained.csv`) and end-to-end (`e2e.csv`) benchmarks measure
it. The coordinated-omission trap is to record `completion − operation_start`: when the
system falls behind, the events that *should* have arrived during the stall are never
timed, so the measured latency hides exactly the backlog the load test exists to expose.
The correction is **schedule-relative**: the producer stamps each event's `ts` with its
scheduled arrival (ns from a shared clock base) *before* processing, and the consumer
records `now − ts`. So a backlog is charged to every late event and lands in the tail — e.g.
in `e2e.csv` the synthetic floor is ~110–140 ns p50 while the pipeline keeps up, then p50
and p99 jump to the millisecond scale at saturation as the producer falls behind the
schedule. The correction is itself unit-tested (`co_correct_records_accumulating_lag`).
The 7 ns clock floor is reported and never subtracted. ([`BENCHMARKS.md`](BENCHMARKS.md)
§2, §5.)

**Re-derive from memory.** Define service vs response time and name two benchmarks of each
in this repo. What does the naive `completion − operation_start` form hide, and why? What
exactly is subtracted in the CO-correct form, and where does a backlog show up?

---

*The study aid is the agent's; the comprehension is the human's. Close the file and
re-derive each before calling the repo done.*

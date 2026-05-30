# phase0-spec.md — Demolition + Workspace Skeleton + Guardrail

*Authoritative spec for a single autonomous Claude Code session. Subordinate to `NORTH-STAR.md` and `docs/specs/kickoff-brief.md`. Scope is exactly Phase 0 — nothing in this file authorizes work on Phase 1 or later. One session = one deliverable = one commit = STOP.*

---

## 0. The single deliverable (one sentence)

Strip the legacy `Web3-Terminal` repo to bare metal, stand up a clean five-crate sans-IO workspace (`book`, `sync`, `feed`, `bench`, `engine`) that builds + clippies + tests green with a near-empty dependency tree, land the fixed-point tick types (`Px`, `Qty`) in `book` with tests, and commit the `CLAUDE.md` guardrail — then STOP.

If at the end the repo still contains `f64`, `tokio`, `redis`, `dashmap`, or any order-book logic, **Phase 0 has failed.** This phase is demolition and scaffolding only; no behavior is built.

---

## 1. Preconditions — read before touching anything

1. Read `docs/specs/kickoff-brief.md` in full (the refactor decision table in §1 is binding).
2. Read `NORTH-STAR.md` §3 (engineering philosophy) and §6 (DoD culture).
3. Confirm the working tree is the legacy repo: `arb_v1/`, `arbitrage/`, `collector/`, root `Cargo.toml` with `members = ["arb_v1","arbitrage","collector"]`.

Do not begin until the above are loaded.

---

## 2. Naming decision (binding)

The sans-IO core referred to as `core` in the kickoff brief is implemented as a crate named **`book`**. Rationale: a Cargo package named `core` shadows Rust's built-in `core` crate in the extern prelude, making `use core::sync::atomic::*` (needed by `sync` in Phase 6) ambiguous. `book` is collision-free and names the thing precisely. Wherever the brief says "the `core` crate," it means `book`.

---

## 3. Target end-state tree

```
Web3-Terminal/
├── .cargo/
│   └── config.toml            # target-cpu=native (mechanical sympathy; host-specific by design)
├── .gitignore                 # ignores /target ONLY — never bench/results or feed/corpus
├── CLAUDE.md                  # the guardrail (full content in §8)
├── Cargo.toml                 # workspace root (full content in §5)
├── Cargo.lock                 # regenerated from scratch
├── README.md                  # one-line stub; authoritative README is Phase 10
├── docs/
│   ├── specs/
│   │   ├── kickoff-brief.md   # already present
│   │   └── phase0-spec.md     # this file
│   └── legacy-reference/
│       └── binance-ws-parsing.rs.txt   # NON-COMPILED reference, for the Phase 3 recorder only
├── book/
│   ├── Cargo.toml
│   └── src/lib.rs             # Px, Qty + tests  (the ONLY crate with real code this phase)
├── sync/
│   ├── Cargo.toml
│   └── src/lib.rs             # stub — Phases 6-7
├── feed/
│   ├── Cargo.toml
│   └── src/lib.rs             # stub — Phase 3
├── bench/
│   ├── Cargo.toml
│   ├── src/main.rs            # stub — Phase 4
│   └── results/.gitkeep       # committed-numbers dir must exist now
└── engine/
    ├── Cargo.toml
    ├── src/main.rs            # stub — Phase 8
    └── (feed/corpus/.gitkeep lives under feed/, see below)

feed/corpus/.gitkeep           # committed-corpus dir must exist now
```

---

## 4. Task A — Demolition (exact)

1. **Preserve, then delete the collector.** Copy `collector/src/exchanges/binance.rs` → `docs/legacy-reference/binance-ws-parsing.rs.txt` (rename to `.txt` so it is never compiled and never enters the dependency graph). This is the only legacy artifact kept, and it is kept *only* as a parsing reference for the Phase 3 recorder. Then delete the entire `collector/` directory.
2. **Delete** `arb_v1/` and `arbitrage/` directories entirely.
3. **Delete** the root `Cargo.lock` (it will be regenerated against the new, minimal dependency set).
4. After demolition, the following must be true (these are gates, verify them):
   - `arb_v1/`, `arbitrage/`, `collector/` no longer exist.
   - No tracked `.rs` file (outside `docs/legacy-reference/`) contains `f64`, `tokio`, `redis`, `dashmap`, `serde_json`, `simd_json`, `reqwest`, or `tokio_tungstenite`.

Demolition removes the Redis-IPC bus, the f64 data model, the async runtime, the arbitrage logic, and the multi-exchange fan-in — per kickoff-brief §1 DROP table.

---

## 5. Task B — Workspace skeleton (exact file contents)

### `Cargo.toml` (workspace root)

```toml
[workspace]
resolver = "3"
members = ["book", "sync", "feed", "bench", "engine"]

[workspace.package]
edition      = "2024"
rust-version = "1.85"
license      = "MIT OR Apache-2.0"

[workspace.lints.rust]
missing_debug_implementations = "warn"
rust_2018_idioms              = "warn"

[workspace.lints.clippy]
all      = { level = "deny",  priority = -1 }
pedantic = { level = "warn",  priority = -1 }

[profile.release]
opt-level     = 3
lto           = "fat"
codegen-units = 1
panic         = "unwind"     # keep unwinding: the bench harness must catch & report the input that breaks

[profile.bench]
inherits = "release"
debug    = true              # keep symbols for perf / flamegraph in Phase 9
```

### `.cargo/config.toml`

```toml
# Microarchitecture profiling (Phase 9) is only valid against a known target.
# Binaries are host-specific BY DESIGN; the benchmark host is documented in the README (Phase 10).
[build]
rustflags = ["-C", "target-cpu=native"]
```

### Crate dependency DAG (declare now — locks the architecture)

```
book   : leaf, zero external deps
sync   : leaf, zero external deps (unsafe permitted here ONLY)
feed   : -> book
bench  : -> book, feed, sync
engine : -> book, feed, sync
```

### Per-crate `Cargo.toml` template

Each crate's manifest opts into the workspace lints and (except `sync`) forbids unsafe via the crate root attribute (see §6). Example for `feed`:

```toml
[package]
name         = "feed"
version      = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
license.workspace      = true

[lints]
workspace = true

[dependencies]
book = { path = "../book" }
```

`bench` and `engine` add `feed` and `sync` path deps; `book` and `sync` have empty `[dependencies]`. No external crates are added in Phase 0 — the dependency tree must stay empty of third-party code. (HdrHistogram, core_affinity, tokio, etc. arrive in the phases that need them.)

### Stub crate roots

- `sync/src/lib.rs`, `feed/src/lib.rs`: a module doc comment naming the phase that fills them. Empty otherwise (an empty lib compiles warning-free).
- `bench/src/main.rs`, `engine/src/main.rs`: `fn main() {}` with a doc comment naming the phase. (Empty `main` compiles warning-free.)
- `README.md`: one line — `# Low-latency market-data engine — see docs/specs/kickoff-brief.md. Authoritative README lands in Phase 10.`
- `bench/results/.gitkeep`, `feed/corpus/.gitkeep`: empty files so the committed-artifact directories exist and `.gitignore` correctness is provable.

---

## 6. Task C — `book/src/lib.rs` (the only real code this phase)

Representation is **locked in Phase 0**: `Px` and `Qty` are `#[repr(transparent)]` newtypes over `i64`. Methods may be added through Phase 2; the representation may not change. `book` is frozen after Phase 2.

Reference implementation — use this:

```rust
//! `book` — the sans-IO limit-order-book core (the `core` of the kickoff brief,
//! renamed to avoid shadowing Rust's built-in `core` crate).
//!
//! INVARIANTS (locked in Phase 0, enforced for the life of the repo):
//! - No floating point anywhere in this crate. Prices and quantities are integers.
//! - No I/O, no async, no allocation in the hot path, no third-party dependencies.
//! - The float-string -> integer-tick conversion happens exactly ONCE, at the
//!   recorder edge (Phase 3). Nothing downstream of the corpus ever sees a float.
#![forbid(unsafe_code)]

use core::ops::{Add, AddAssign, Sub, SubAssign};

/// Price as an integer number of ticks (the symbol's minimum price increment).
/// `repr(transparent)` => ABI-identical to `i64`, a genuinely zero-cost newtype.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Px(pub i64);

/// Quantity as an integer number of lots (the symbol's minimum size increment).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Qty(pub i64);

impl Px {
    pub const ZERO: Px = Px(0);
    #[inline] #[must_use] pub const fn ticks(self) -> i64 { self.0 }
    /// Signed tick distance `self - other` (positive when `self` is the higher price).
    #[inline] #[must_use] pub const fn diff(self, other: Px) -> i64 { self.0 - other.0 }
}

impl Qty {
    pub const ZERO: Qty = Qty(0);
    #[inline] #[must_use] pub const fn lots(self) -> i64 { self.0 }
    #[inline] #[must_use] pub const fn is_zero(self) -> bool { self.0 == 0 }
}

impl Add<i64> for Px { type Output = Px; #[inline] fn add(self, t: i64) -> Px { Px(self.0 + t) } }
impl Sub<i64> for Px { type Output = Px; #[inline] fn sub(self, t: i64) -> Px { Px(self.0 - t) } }
impl Add for Qty { type Output = Qty; #[inline] fn add(self, r: Qty) -> Qty { Qty(self.0 + r.0) } }
impl Sub for Qty { type Output = Qty; #[inline] fn sub(self, r: Qty) -> Qty { Qty(self.0 - r.0) } }
impl AddAssign for Qty { #[inline] fn add_assign(&mut self, r: Qty) { self.0 += r.0; } }
impl SubAssign for Qty { #[inline] fn sub_assign(&mut self, r: Qty) { self.0 -= r.0; } }

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn px_is_zero_cost_over_i64() {
        assert_eq!(size_of::<Px>(), size_of::<i64>());
        assert_eq!(size_of::<Qty>(), size_of::<i64>());
        assert_eq!(size_of::<Option<Px>>(), size_of::<i64>() * 2); // sanity on layout
    }

    #[test]
    fn px_orders_like_an_integer() {
        assert!(Px(100) > Px(99));
        assert!(Px(-1) < Px(0));
        let mut v = [Px(3), Px(1), Px(2)];
        v.sort_unstable();
        assert_eq!(v, [Px(1), Px(2), Px(3)]);
    }

    #[test]
    fn px_diff_is_signed() {
        assert_eq!(Px(105).diff(Px(100)), 5);
        assert_eq!(Px(100).diff(Px(105)), -5);
        assert_eq!(Px(100).diff(Px(100)), 0);
    }

    #[test]
    fn px_tick_arithmetic() {
        assert_eq!(Px(100) + 5, Px(105));
        assert_eq!(Px(100) - 5, Px(95));
    }

    #[test]
    fn qty_arithmetic_and_zero() {
        assert!(Qty::ZERO.is_zero());
        let mut q = Qty(10);
        q += Qty(5);
        assert_eq!(q, Qty(15));
        q -= Qty(15);
        assert!(q.is_zero());
    }
}
```

Note: `use core::ops::...` inside the `book` crate is unambiguous precisely because the crate is named `book`, not `core` — this is the naming decision (§2) paying off immediately.

---

## 7. Task D — unsafe quarantine

- `book`, `feed`, `bench`, `engine`: each crate root carries `#![forbid(unsafe_code)]`.
- `sync`: does **not** forbid unsafe (the seqlock and SPMC ring need it in Phases 6-7). It carries `#![deny(unsafe_op_in_unsafe_fn)]` instead, and a module-doc line stating that unsafe is permitted but every `unsafe` block must carry a `// SAFETY:` comment.

This makes "unsafe lives in exactly one crate" a structural, greppable fact.

---

## 8. Task E — `CLAUDE.md` (full content to write at repo root)

```markdown
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
```

---

## 9. Phase 0 Definition of Done (every box is a gate)

- [ ] `arb_v1/`, `arbitrage/`, `collector/` deleted; `collector/src/exchanges/binance.rs` preserved as `docs/legacy-reference/binance-ws-parsing.rs.txt` (non-compiled).
- [ ] Grep is clean: no `f64`, `tokio`, `redis`, `dashmap`, `serde_json`, `simd_json`, `reqwest`, `tokio_tungstenite` in any tracked `.rs` outside `docs/legacy-reference/`. Verify:
      `grep -rIn --include='*.rs' -E '\bf6?4\b|tokio|redis|dashmap|serde_json|simd_json|reqwest|tungstenite' book sync feed bench engine` returns nothing.
- [ ] Five crates exist: `book`, `sync`, `feed`, `bench`, `engine`. Dependency DAG matches §5 (`book`/`sync` are leaves; `feed`→`book`; `bench`/`engine`→`book`,`feed`,`sync`).
- [ ] `cargo tree --workspace` shows ZERO third-party dependencies (only the five path crates).
- [ ] Unsafe quarantine: `#![forbid(unsafe_code)]` present in `book`, `feed`, `bench`, `engine`; absent in `sync` (which has `#![deny(unsafe_op_in_unsafe_fn)]`).
- [ ] `book` exposes `Px(pub i64)` and `Qty(pub i64)`, both `#[repr(transparent)]`, with the §6 tests present and passing (including the `size_of` zero-cost assertion).
- [ ] `.cargo/config.toml` sets `target-cpu=native`. `[profile.release]` has `lto="fat"`, `codegen-units=1`, `opt-level=3`.
- [ ] `CLAUDE.md` (root) contains the §8 content verbatim.
- [ ] `.gitignore` ignores `/target` and editor dirs but NOT `bench/results/` or `feed/corpus/`; both dirs exist with `.gitkeep`.
- [ ] Green: `cargo build --workspace --all-targets`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` all pass.
- [ ] Exactly one commit, message:
      `chore(phase-0): demolish legacy stack, scaffold sans-IO workspace, add tick types + guardrail`
- [ ] STOP. Do not start Phase 1.

---

## 10. Explicit non-goals (do NOT do in this session)

- Do not define `BookEvent`, `Side`, or the `OrderBook` trait (Phase 1).
- Do not implement any book data structure (Phases 1, 2, 5).
- Do not write the seqlock or the SPMC ring (Phases 6, 7).
- Do not write the recorder, the replay iterator, or generate any corpus (Phase 3).
- Do not write the bench harness or produce any numbers (Phase 4).
- Do not add tokio, HdrHistogram, core_affinity, or any third-party crate.
- Do not write README content beyond the one-line stub.

If a task feels like it belongs to a later phase, it does. Stop and leave it.

---

## 11. Completion report (what to print before STOP)

A three-line summary: (1) crates created + the dependency DAG in one line; (2) confirmation the grep gate and `cargo tree` zero-dep gate passed; (3) the commit hash. Then STOP.

# feed — Phase 3 Specification: Corpus Format, Deterministic Replay, Synthetic Generator, and the Quarantined Recorder

**Companion to:** `NORTH-STAR.md`, `docs/specs/kickoff-brief.md`, `docs/specs/phase0-spec.md`, `docs/specs/phase1-spec.md`, `docs/specs/phase2-spec.md`, and the root `CLAUDE.md`. Read all of them first.
**This is the complete, authoritative Phase 3 spec.** `book` is frozen (tag `book-v1-frozen`); `feed` consumes its public API and must not change it.
**Scope:** the `feed` crate — a committed binary corpus format, a deterministic allocation-free replay source, a seeded synthetic load generator (steady / burst / flash-crash), and a feature-gated, quarantined Binance recorder that captures a real session into a corpus.
**Audience:** Claude Code. Authoritative.

---

## 1. Phase 3 in one paragraph

A benchmark you cannot replay byte-for-byte is not a benchmark — it is an anecdote. Live exchange feeds are unrepeatable (you cannot re-run a flash crash) and their network jitter contaminates the very latency distributions Phase 4 exists to measure. Phase 3 therefore severs measurement from the network: a one-shot **recorder** captures a real Binance session once and converts it, at the edge, into a flat binary **corpus** of tick-space `BookEvent`s; a deterministic **synthetic generator** produces the same corpus shape under controllable load profiles; and a **replay** source hands the frozen `book` impls a resident, ordered `&[BookEvent]` slice with zero allocation and zero copying on the hot path. After Phase 3, every Phase 4 run replays identical bytes and therefore identical event sequences — the precondition for a falsifiable shootout.

### 1.1 Frozen / reused / the async quarantine
- **`book` is frozen and reused unmodified.** `feed` constructs events only via the public `BookEvent::level` / `trade` / `clear` constructors and reads `Px`/`Qty`/`Side`/`EventKind`. If `feed` appears to need a `book` change, the design is wrong — STOP and ask.
- **The async quarantine is the defining constraint of this crate.** `tokio`, `tokio-tungstenite`, `serde`, and `serde_json` are **optional dependencies behind the `recorder` feature**, used only by the `recorder` binary. The default build — the `feed` library, the replay path, the synthetic generator, the `gen` binary, and every downstream crate (`bench`, `engine`) — links **none** of them. The quarantine is enforced by Cargo, not by discipline (§2).
- **The corpus boundary is absolute.** Float parsing happens only inside the recorder, at the string→integer-tick edge, and nothing downstream of the written corpus ever sees a float or a heap `String`. The recorder performs the conversion with **exact integer arithmetic (no `f64`)** (§6.3).
- **`#![forbid(unsafe_code)]` holds across the whole crate**, including the recorder binary. The corpus is loaded by explicit, safe, validated deserialization — never by transmuting mapped bytes (§3.3).

---

## 2. Workspace additions & dependencies

```
feed/src/lib.rs              # EDIT  — module decls + public re-exports
feed/src/corpus.rs           # NEW   — binary format, Corpus (load/save/replay), validation
feed/src/synthetic.rs        # NEW   — Profile, GenConfig, generate()
feed/src/rng.rs              # NEW   — SplitMix64 (public; reproducible generation)
feed/src/bin/gen.rs          # NEW   — default-feature binary: writes the canonical synthetic corpora
feed/src/bin/recorder.rs     # NEW   — feature-gated async Binance recorder (quarantined)
feed/corpus/*.mdf            # NEW   — committed corpora (synthetic + one short real sample)
feed/corpus/*.meta.json      # NEW   — provenance sidecars
feed/README.md               # NEW   — corpus format spec + regenerate/record instructions
```

### 2.1 `feed/Cargo.toml` — the quarantine (exact shape)
```toml
[package]
name = "feed"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[features]
default = []
recorder = ["dep:tokio", "dep:tokio-tungstenite", "dep:futures-util", "dep:serde", "dep:serde_json"]

[dependencies]
book = { path = "../book" }
tokio             = { version = "*", features = ["rt-multi-thread", "macros", "net", "time", "signal"], optional = true }
tokio-tungstenite = { version = "*", features = ["rustls-tls-webpki-roots"], optional = true }
futures-util      = { version = "*", optional = true }
serde             = { version = "*", features = ["derive"], optional = true }
serde_json        = { version = "*", optional = true }

[[bin]]
name = "gen"
# default features only — pure lib, no async

[[bin]]
name = "recorder"
required-features = ["recorder"]
```
**Pin exact versions.** Resolve the latest stable of each optional crate via `cargo add <crate> --optional --features ...` (the agent has crates.io access) and verify the `tokio-tungstenite` TLS feature name against its current docs (TLS feature names change between releases). Replace every `"*"` with the pinned version before committing.

### 2.2 Quarantine proof (a DoD gate, not a suggestion)
- `cargo tree -p feed` (default) shows **only `book`**.
- `cargo tree -p feed --features recorder` shows the async stack.
- `cargo build -p feed` and `cargo build -p bench` never compile `tokio`.
- The `recorder` binary builds only under `cargo build -p feed --features recorder --bin recorder`.

---

## 3. The corpus binary format (`.mdf`)

A corpus is a header followed by a flat array of fixed-size records. Little-endian, explicitly (de)serialized field-by-field — **portable across architectures and sound on corrupt input.**

### 3.1 Header — 32 bytes, little-endian
| Offset | Field | Type | Value |
|---|---|---|---|
| 0  | `magic`       | `[u8; 8]` | `b"MDFEED\0\0"` |
| 8  | `version`     | `u32` | `1` |
| 12 | `record_size` | `u32` | `40` |
| 16 | `count`       | `u64` | number of records |
| 24 | `meta`        | `u64` | reserved, `0` (later: profile/symbol tag) |

### 3.2 Record — 40 bytes, little-endian (mirrors `BookEvent`'s frozen layout)
| Offset | Field | Type | Notes |
|---|---|---|---|
| 0  | `seq`  | `u64` | monotonic emission counter |
| 8  | `ts`   | `u64` | event time, nanoseconds |
| 16 | `px`   | `i64` | price in integer ticks |
| 24 | `qty`  | `i64` | quantity in integer lots (0 ⇒ remove, for `Level`) |
| 32 | `side` | `u8`  | `0=Bid, 1=Ask` (aggressor for `Trade`) |
| 33 | `kind` | `u8`  | `0=Level, 1=Trade, 2=Clear` |
| 34 | `pad`  | `[u8; 6]` | written as zero; ignored on read |

`record_size = 40` equals `size_of::<BookEvent>()` (asserted frozen in Phase 1), so disk and memory record sizes agree and `count = (file_len - 32) / 40` is exact.

### 3.3 Decision: explicit LE (de)serialization, **not** mmap-and-transmute
Phase 1 locked `BookEvent` at `#[repr(C)]` 40 bytes "so the corpus can mmap a flat `[BookEvent]`." Phase 3 refines that intent: the corpus is loaded by **reading the file into one owned buffer and parsing each record with `from_le_bytes`, validating the `side`/`kind` discriminants before constructing each `BookEvent`** — never by reinterpreting mapped bytes as `&[BookEvent]`. Rationale: `Side`/`EventKind` are `repr(u8)` enums with only 2/3 valid bit patterns; casting an arbitrary (possibly corrupt) byte to one is instant UB, so a zero-copy `&[BookEvent]` from untrusted bytes is unsound and would *still* require a validation pass. The transmute path costs `unsafe` + an mmap dependency to avoid a one-time, off-the-clock load copy — a bad trade. The load is setup, not measured; the replay slice is resident in RAM and iterated by reference, so the hot path is already zero-copy and zero-alloc. (Rejected: `memmap2` + `unsafe` cast — violates the crate's `forbid(unsafe_code)` and zero-dep posture for no measured-path benefit. The `repr(C)`/40-byte layout is retained because matching disk and memory sizes keeps the format clean.)

---

## 4. `feed/src/corpus.rs` — format, reader, writer, replay

```rust
//! Binary corpus format and deterministic replay source. The corpus is the
//! boundary: above it, integer ticks only — no floats, no async, no I/O on the
//! hot path. Replay is a resident `&[BookEvent]` slice iterated by reference.

use book::{BookEvent, EventKind, Px, Qty, Side};
use std::io::{self, Read, Write};
use std::path::Path;

pub const MAGIC: [u8; 8] = *b"MDFEED\0\0";
pub const VERSION: u32 = 1;
pub const RECORD_SIZE: usize = 40;
pub const HEADER_SIZE: usize = 32;

#[derive(Debug)]
pub enum CorpusError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    BadRecordSize(u32),
    Truncated { expected: u64, found: u64 },
    BadDiscriminant { record: u64, field: &'static str, byte: u8 },
}
impl From<io::Error> for CorpusError { fn from(e: io::Error) -> Self { CorpusError::Io(e) } }
// impl std::fmt::Display + std::error::Error for CorpusError (required).

/// A loaded corpus: events resident in one owned buffer, replayed by reference.
#[derive(Debug, Default)]
pub struct Corpus {
    events: Vec<BookEvent>,
}

impl Corpus {
    /// Build from events already in memory (synthetic generator path).
    #[must_use]
    pub fn from_events(events: Vec<BookEvent>) -> Self { Self { events } }

    /// Load and FULLY VALIDATE a corpus file. One allocation (the event Vec).
    pub fn load(path: &Path) -> Result<Self, CorpusError> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut buf)?;
        Self::from_bytes(&buf)
    }

    /// Parse + validate header, then each record. Reject corrupt input loudly.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, CorpusError> {
        // validate len >= HEADER_SIZE; magic; version == VERSION;
        // record_size == 40; count; (buf.len() - HEADER_SIZE) == count * 40 (else Truncated).
        // for each record: read seq/ts/px/qty (from_le_bytes), validate side in {0,1}
        // and kind in {0,1,2} (else BadDiscriminant), then construct BookEvent via the
        // public constructors. Collect into Vec<BookEvent>.
        todo!()
    }

    /// Write events to a corpus file (header + records, all little-endian).
    pub fn save(path: &Path, events: &[BookEvent]) -> Result<(), CorpusError> {
        let f = std::fs::File::create(path)?;
        let mut w = io::BufWriter::new(f);
        Self::write_to(&mut w, events)?;
        w.flush()?;
        Ok(())
    }

    pub fn write_to<W: Write>(w: &mut W, events: &[BookEvent]) -> Result<(), CorpusError> {
        // header: MAGIC, VERSION.to_le_bytes, (RECORD_SIZE as u32).to_le_bytes,
        //         (events.len() as u64).to_le_bytes, 0u64.to_le_bytes
        // each record: seq,ts (u64 LE); px,qty (i64 LE); side as u8; kind as u8; [0u8;6]
        todo!()
    }

    /// The replay source: resident, ordered, zero-alloc/zero-copy by reference.
    #[must_use] pub fn events(&self) -> &[BookEvent] { &self.events }
    #[must_use] pub fn len(&self) -> usize { self.events.len() }
    #[must_use] pub fn is_empty(&self) -> bool { self.events.is_empty() }
    pub fn iter(&self) -> std::slice::Iter<'_, BookEvent> { self.events.iter() }
}
```

**`feed` does not pace, sleep, or time anything.** It hands the harness the ordered events and their recorded `ts`; open-loop arrival pacing and coordinated-omission correctness are **Phase 4's** job. Keeping `feed` free of timing is what makes replay deterministic.

**Helper to map a discriminant safely** (no transmute):
```rust
fn side_from_u8(b: u8, rec: u64) -> Result<Side, CorpusError> {
    match b { 0 => Ok(Side::Bid), 1 => Ok(Side::Ask),
              _ => Err(CorpusError::BadDiscriminant { record: rec, field: "side", byte: b }) }
}
fn kind_from_u8(b: u8, rec: u64) -> Result<EventKind, CorpusError> {
    match b { 0 => Ok(EventKind::Level), 1 => Ok(EventKind::Trade), 2 => Ok(EventKind::Clear),
              _ => Err(CorpusError::BadDiscriminant { record: rec, field: "kind", byte: b }) }
}
```
Construct each `BookEvent` from the validated `kind` via the public constructors (`level`/`trade`/`clear`) so the in-memory enums are always valid.

**Required unit tests (`corpus.rs`):**
1. Round-trip: `events → write_to → from_bytes` yields an identical event vector (compare via the same `Obs`-style observable or field equality).
2. Empty corpus round-trips (count 0).
3. Large corpus round-trips (≥ 1,000,000 records) — exercises the buffered writer and the count arithmetic.
4. Corrupt input is rejected: bad magic, wrong version, wrong record_size, truncated tail (not a multiple of 40), and an out-of-range `side`/`kind` byte each produce the specific `CorpusError`.
5. Determinism: the same events written twice produce byte-identical files.

---

## 5. `feed/src/synthetic.rs` + `feed/src/rng.rs` — deterministic load profiles

### 5.1 `rng.rs`
```rust
//! Deterministic, seedable PRNG for reproducible synthetic corpora. Public so the
//! `gen` binary and Phase 4 can regenerate identical streams from a seed.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);
impl SplitMix64 {
    #[must_use] pub fn new(seed: u64) -> Self { Self(seed) }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 { self.next_u64() % n }
}
```
(Independent copy from `book`'s test PRNG — `book` is frozen and its test module is not importable. ~10 lines of a standard algorithm; here it is a real, public library feature, not duplicated test scaffolding.)

### 5.2 `synthetic.rs` — API
```rust
use book::{BookEvent, Px, Qty, Side};
use crate::rng::SplitMix64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile { Steady, Burst, FlashCrash }

#[derive(Clone, Debug)]
pub struct GenConfig {
    pub profile: Profile,
    pub seed: u64,
    pub events: usize,
    pub mid: Px,         // starting mid price (ticks)
    pub band: i64,       // half-width of the active price band (ticks)
    pub max_qty: Qty,
    pub start_ts: u64,   // nanoseconds
}

/// Deterministic: identical `GenConfig` -> identical `Vec<BookEvent>`.
#[must_use]
pub fn generate(cfg: &GenConfig) -> Vec<BookEvent> { /* see §5.3 */ todo!() }
```

### 5.3 Profile semantics (these define the stress the Phase 4 sweep applies)
The generator keeps a light internal model — the current best bid/ask and the set of occupied levels per side — so emitted events resemble a real feed (updates and removals target existing or adjacent levels). `ts` advances monotonically; `seq` is a per-stream counter.

- **`Steady`** — a balanced book that hovers around `mid`. ~94% `Level` updates concentrated in the top ~8 levels (geometric decay of depth from the touch point), ~5% `Trade`, rare new-level/removal churn at the edges. `ts` advances at a roughly constant inter-event interval. This is the baseline that should favor `RevVecBook` (H1).
- **`Burst`** — long calm stretches (sparse `ts`) punctuated by short, dense bursts (hundreds of events sharing a tight `ts` window). Same spatial locality as `Steady`. Stresses apply throughput and, later, the SPMC ring's backpressure (Phases 7–8). The `ts` clustering is the distinguishing feature.
- **`FlashCrash`** — periodic calm, then a directional cascade: the aggressor sweeps level after level (`Trade`s printing down through the bid ladder), best bid collapses by many ticks within a tight `ts` window, levels are wiped (`qty=0`) across a wide span, the book thins on the struck side, then a partial recovery rebuilds levels. This is the **systems** repurposing of the mentor transcript's "Trump candle" — all product framing stripped; what remains is the worst case for a top-of-book linear scan (touched levels move far from index 0 as the book thins) and a heavy `memmove` workload. It is the profile most likely to expose where `RevVecBook` loses (H2/H4), which is exactly why it must exist.

**Required unit tests (`synthetic.rs`):**
1. Determinism: identical `GenConfig` → byte-identical corpus (generate twice, `write_to` both, compare).
2. Count: `generate` returns exactly `cfg.events` events.
3. Profile sanity: `FlashCrash` produces a span where best bid falls by ≥ K ticks within a bounded `ts` window and one side's depth contracts; `Burst` produces ≥ one window with ≥ B events sharing a `ts` band; `Steady` keeps best bid/ask within the band.
4. Replay sanity: feed each profile's output through `BTreeBook`, `SortedVecBook`, and `RevVecBook`; none panics and all three agree on final `best_bid`/`best_ask`/depths (a cheap reuse of the oracle invariant — not a substitute for Phase 2's oracle).

### 5.4 `feed/src/bin/gen.rs` — write the committed synthetic corpora
A pure-library (default-feature) binary. CLI: `gen --profile <steady|burst|flashcrash> --seed <u64> --events <n> --out <path>`; plus a `--all` mode that writes the canonical committed set:
```
feed/corpus/steady-s1-100k.mdf
feed/corpus/burst-s1-100k.mdf
feed/corpus/flashcrash-s1-100k.mdf
```
all at `seed=1`, `events=100_000`, a fixed `mid`/`band`/`max_qty`, and a matching `*.meta.json` (§7). These are the primary reproducible Phase 4 fixtures; commit them.

---

## 6. `feed/src/bin/recorder.rs` — the quarantined Binance recorder

Feature-gated (`recorder`), async, run **once** by the owner to capture a real session. It is never on the measured path and never in CI. `#![forbid(unsafe_code)]` applies.

### 6.1 CLI
```
recorder --symbol BTCUSDT --duration-secs 60 --out feed/corpus/btcusdt-sample.mdf
         [--max-events N] [--with-trades]
```
Bounded by duration or event count; on completion, flush the corpus + write the `.meta.json` sidecar, then exit 0.

### 6.2 Flow (depth diff + local-book reconciliation, Binance documented procedure)
1. Resolve scale from `GET /api/v3/exchangeInfo?symbol=SYM`: `PRICE_FILTER.tickSize`, `LOT_SIZE.stepSize` (decimal strings).
2. Open the diff stream `wss://stream.binance.com:9443/ws/<sym lower>@depth@100ms`; begin buffering diff messages immediately.
3. `GET /api/v3/depth?symbol=SYM&limit=1000` → `lastUpdateId`, `bids[]`, `asks[]`.
4. Drop buffered diffs with `u <= lastUpdateId`. Emit `Clear`, then a `Level` per snapshot bid/ask.
5. Apply the first diff where `U <= lastUpdateId+1 <= u`; thereafter require contiguity (`U == prev_u + 1`). On a gap, log and re-snapshot (re-`Clear` + reseed). For each diff, emit one `Level` per `(side, price, qty)` entry (`qty=0` ⇒ removal, already the book's semantics).
6. Optionally subscribe `<sym>@trade`: emit `Trade` with `aggressor = if m { Side::Ask } else { Side::Bid }` (`m` = "buyer is maker"; taker is the aggressor).
7. `seq` = local monotonic emission counter; `ts` = exchange event time `E` (ms) × 1_000_000 (ns).
8. On `--duration-secs`/`--max-events`/SIGINT: flush via `Corpus::save`, write `.meta.json`, exit.

Verify the endpoint, stream name, and message schema against current Binance API docs and the preserved reference `docs/legacy-reference/binance-ws-parsing.rs.txt`.

### 6.3 The corpus boundary: exact string→tick conversion (NO `f64`)
Prices/quantities arrive as decimal strings. Convert with exact integer arithmetic; `f64` is forbidden even here.
```rust
/// `value / tick` as an integer, exactly, with no floating point.
/// Both args are decimal strings (e.g. "65000.50", "0.01"). Errors if `value`
/// is not an integer multiple of `tick`.
fn to_ticks(value: &str, tick: &str) -> Result<i64, ConvError> {
    let d = frac_digits(value).max(frac_digits(tick));     // common scale 10^d
    let v: i128 = scale_to_int(value, d)?;                 // value * 10^d
    let t: i128 = scale_to_int(tick, d)?;                  // tick  * 10^d
    if t == 0 || v % t != 0 {
        return Err(ConvError::OffTickGrid);
    }
    i64::try_from(v / t).map_err(|_| ConvError::Overflow)
}
// frac_digits: count chars after '.'; scale_to_int: parse "[-]int[.frac]" into an
// i128 scaled by 10^d (pad/validate the fractional part to exactly d digits).
```
`Px` = price ÷ tickSize; `Qty` = qty ÷ stepSize. Both are exact integer counts. The human-readable scales live only in the meta sidecar (§7); the book never needs them.

### 6.4 Recorded sample — size discipline
Capture a **short, bounded** session (target ≤ a few MB; e.g. 60 s of `BTCUSDT`, or a lower-volume symbol if needed). Commit `feed/corpus/btcusdt-sample.mdf` + `.meta.json`. Do not commit large captures; document the recorder invocation so a fresh capture is one command. The synthetic corpora — not the real sample — are the primary committed fixtures; the real sample exists for credibility ("validated against real market data").

---

## 7. Determinism & provenance

**The reproducibility contract:** a given `.mdf` file replays to an identical event sequence on every run and every machine (explicit little-endian, no `f64`, no wall-clock in replay). Synthetic corpora are additionally regenerable from `(profile, seed, config)` via `gen`.

**`*.meta.json` sidecar** (one per committed corpus) records provenance:
- For synthetic: `{ kind: "synthetic", profile, seed, events, mid, band, max_qty, start_ts, generator_version }`.
- For recorded: `{ kind: "binance-recorded", symbol, tick_size, step_size, captured_at_utc, duration_secs, record_count, with_trades, recorder_version, binance_endpoints }`.

This is the data-provenance discipline the Phase 10 BENCHMARKS.md will cite: every committed corpus traces to either a seed+config or a real capture with its scales and timestamps.

---

## 8. Engineering Standard — governs every file in this phase

1. **The corpus boundary is absolute.** Floats and `String`s exist only inside the recorder, at the parse edge; the corpus and everything downstream are integer ticks. Conversion is exact integer arithmetic, no `f64`.
2. **The async quarantine is enforced by Cargo, not by intent.** `tokio` et al. are optional, behind `recorder`. The default `feed` tree is `book`-only; `bench`/`engine` never link async.
3. **`#![forbid(unsafe_code)]`** across lib and both binaries. The corpus is loaded by safe, validated deserialization — never by transmuting bytes.
4. **Replay is deterministic and timing-free.** `feed` never sleeps, paces, or reads the clock; it yields ordered events and their recorded `ts`. Pacing is Phase 4's.
5. **Hot path is zero-alloc/zero-copy.** Load allocates once (the event `Vec`); replay iterates `&BookEvent` over the resident slice. No per-event allocation.
6. **Corrupt input fails loudly.** Every malformed corpus yields a specific `CorpusError`; no silent truncation, no UB.
7. **Determinism everywhere.** Synthetic generation is seeded and reproducible; the same config and the same file always yield the same bytes/events.
8. **`book` is frozen.** Events are built only through its public constructors; no `book` file is touched.
9. **Provenance is mandatory.** Every committed corpus has a `.meta.json` tracing it to a seed+config or a real capture with scales and timestamps.
10. **Green-gate discipline.** `cargo build --workspace --all-targets`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` green before every commit (and, for the recorder session, the same three plus `--features recorder`). One session → meaningful conventional commit(s) → explicit STOP. Never commit red.

---

## 9. Phase 3 Definition of Done

1. `feed::Corpus` reads/writes/validates the §3 format; all §4 unit tests green (round-trip, empty, ≥1M records, corrupt-rejection, determinism).
2. `feed::generate` + `Profile` + `GenConfig` + `feed::rng::SplitMix64` implemented; all §5 tests green (determinism, count, profile sanity, tri-impl replay agreement).
3. `gen --all` writes the three canonical synthetic corpora + meta to `feed/corpus/`; committed.
4. `recorder` (feature-gated) implemented per §6: exchangeInfo scale resolution, snapshot+diff reconciliation, exact `f64`-free string→tick conversion, optional trades, bounded capture, corpus + meta output. One short real sample committed (≤ a few MB) with its `.meta.json`.
5. **Quarantine proven:** `cargo tree -p feed` (default) shows only `book`; `--features recorder` shows the async stack; `bench` does not link `tokio`.
6. `#![forbid(unsafe_code)]` holds across `feed`; the grep gate (no `f64`/`tokio`/… ) holds for the **default** tree (the async stack appears only under the `recorder` feature, and no `f64` appears anywhere — conversion is integer-exact).
7. `book` byte-for-byte unchanged (`git diff book-v1-frozen -- book/` empty); events built only via public constructors.
8. `feed/README.md` documents the format, the regenerate command, and the record command. `CLAUDE.md` updated per Appendix A.
9. `cargo build`/`clippy -D warnings`/`test` clean at every commit; meaningful conventional commits on `main`.

After Phase 3, `feed` provides deterministic, integer-tick event sequences. Next is Phase 4 (`bench`: the open-loop, coordinated-omission-correct shootout that finally measures the four books).

---

# Appendix A — `CLAUDE.md` update for Phase 3

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, the four-impl shootout, DoD culture
- docs/specs/phase0-spec.md    — workspace, tick types, guardrail
- docs/specs/phase1-spec.md    — event model, OrderBook trait, BTreeBook
- docs/specs/phase2-spec.md    — Vec impls, differential oracle, FREEZE (tag book-v1-frozen)
- docs/specs/phase3-spec.md    — CURRENT: feed (corpus, replay, synthetic, recorder)

## Hard rules
1. book is FROZEN. feed builds events only via public BookEvent constructors.
2. CORPUS BOUNDARY is absolute: floats/Strings only inside the recorder at the
   parse edge; corpus + everything downstream are integer ticks. String->tick
   conversion is EXACT integer arithmetic — no f64 anywhere, including the recorder.
3. ASYNC QUARANTINE is Cargo-enforced: tokio/tokio-tungstenite/serde/serde_json
   are optional deps behind the `recorder` feature, used only by the recorder
   binary. Default feed tree = book only. bench/engine never link async.
4. #![forbid(unsafe_code)] across feed (lib + both bins). Corpus is loaded by
   safe, validated deserialization — never transmuting mapped bytes.
5. feed is timing-free and deterministic: no sleep, no clock, no pacing in replay.
   Same file/seed -> same bytes/events. Pacing + coordinated omission are Phase 4.
6. Every committed corpus has a .meta.json provenance sidecar.

## Scope discipline
Work ONLY on the given session. End green (build + clippy -D warnings + test; the
recorder session also builds --features recorder), commit, list changes, STOP.
```

---

# Appendix B — Claude Code execution plan (4 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | Corpus format + replay | `corpus.rs` (§3–§4) + lib wiring | round-trip / corrupt-reject / 1M-record / determinism tests green |
| 2 | Synthetic generator | `rng.rs`, `synthetic.rs`, `bin/gen.rs` (§5) + committed corpora | determinism/count/profile/tri-impl tests green; 3 corpora + meta committed |
| 3 | Quarantined recorder | `bin/recorder.rs` (§6) + feature-gated deps + 1 real sample | builds with/without feature; quarantine proven; sample + meta committed |
| 4 | Provenance + docs + DoD | `feed/README.md`, meta finalize, `CLAUDE.md`, verify DoD | DoD §9 verified item by item |

Session 3 is the heavy build — split at the snapshot/diff-reconciliation boundary (get exchangeInfo + snapshot + Clear/Level emission green and committed first, then add the live diff/trade loop) if the window tightens. Sessions 1–2 are pure-lib and independent; keep them separate for clean commits and a safety margin.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read the root `CLAUDE.md` and `docs/specs/phase3-spec.md` §1–§4, §8. Update `CLAUDE.md` per Appendix A. Execute **Session 1 only**: implement `feed/src/corpus.rs` exactly per §3–§4 — the 32-byte header, 40-byte little-endian records, `Corpus` (`from_events`/`load`/`from_bytes`/`save`/`write_to`/`events`/`iter`), `CorpusError` with `Display`+`Error`, and the safe discriminant mapping (no transmute). Wire the module + re-exports into `lib.rs`. Add all §4 unit tests. No async, no third-party deps — `cargo tree -p feed` must show only `book`. Run the three gates. Commit `feat(feed): binary corpus format + deterministic replay`. List changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase3-spec.md` §5, §7, §8. Execute **Session 2 only**: implement `feed/src/rng.rs` (public `SplitMix64`), `feed/src/synthetic.rs` (`Profile`, `GenConfig`, `generate` with the §5.3 profile semantics), and `feed/src/bin/gen.rs` (default-feature CLI + `--all`). Add the §5 unit tests. Run `gen --all` to write `feed/corpus/{steady,burst,flashcrash}-s1-100k.mdf` plus `.meta.json` sidecars (§7); commit them. Still zero third-party deps. Run the three gates. Commit `feat(feed): seeded synthetic generator (steady/burst/flash-crash) + corpora`. List changes, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase3-spec.md` §6, §8. Execute **Session 3 only**: add the feature-gated async deps per §2.1 (resolve + pin latest stable via `cargo add --optional`; verify the tokio-tungstenite TLS feature name). Implement `feed/src/bin/recorder.rs` per §6 — exchangeInfo scale resolution, snapshot+diff reconciliation, the exact `f64`-free `to_ticks` conversion, optional trades, bounded capture, `Corpus::save` + `.meta.json`. `#![forbid(unsafe_code)]` applies. Prove the quarantine: `cargo tree -p feed` (default) shows only `book`; `--features recorder` shows the async stack; `cargo build -p bench` links no tokio. Run a bounded capture, commit the short sample + meta. Gates: build/clippy/test (default) AND `cargo build -p feed --features recorder --bin recorder` + clippy on it. Split at the snapshot/live-loop boundary if context grows. Commit `feat(feed): quarantined Binance recorder (feature-gated) + real sample`. List changes, STOP.

**Session 4**
> Read `CLAUDE.md` and `phase3-spec.md` §7, §9, Appendix A. Execute **Session 4 only**: write `feed/README.md` (the §3 format spec, the `gen --all` regenerate command, the `recorder` record command, the corpus manifest with provenance), finalize any missing `.meta.json`, confirm `git diff book-v1-frozen -- book/` is empty, run the three gates, then verify Phase 3 DoD §9 item by item and report each. Commit `docs(feed): corpus format spec, provenance, and Phase 3 close-out`. STOP. The `feed` crate is complete.
```

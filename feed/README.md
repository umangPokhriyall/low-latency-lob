# `feed` — deterministic event source (corpus · replay · synthetic · recorder)

`feed` is the Phase 3 deliverable: a committed binary **corpus** format, a
deterministic allocation-free **replay** source, a seeded **synthetic** load
generator (steady / burst / flash-crash), and a feature-gated, quarantined
**recorder** that captures a real Binance session into a corpus.

It sits above the frozen `book` core (tag `book-v1-frozen`) and below the Phase 4
`bench` harness. Everything `feed` hands downstream is **integer ticks** — no
floats, no `String`s, no async, no clock. `#![forbid(unsafe_code)]` holds across
the whole crate, including the recorder binary.

> **The corpus boundary is absolute.** Float parsing happens *only* inside the
> recorder, at the string→integer-tick edge (exact integer arithmetic, no `f64`).
> Nothing downstream of the written corpus ever sees a float or a heap `String`.

---

## The corpus binary format (`.mdf`)

A corpus is a 32-byte header followed by a flat array of fixed-size 40-byte
records. Everything is **little-endian** and explicitly (de)serialized
field-by-field — portable across architectures and sound on corrupt input.

### Header — 32 bytes, little-endian

| Offset | Field         | Type      | Value                         |
|-------:|---------------|-----------|-------------------------------|
| 0      | `magic`       | `[u8; 8]` | `b"MDFEED\0\0"`               |
| 8      | `version`     | `u32`     | `1`                           |
| 12     | `record_size` | `u32`     | `40`                          |
| 16     | `count`       | `u64`     | number of records             |
| 24     | `meta`        | `u64`     | reserved, `0`                 |

### Record — 40 bytes, little-endian (mirrors `book::BookEvent`)

| Offset | Field  | Type      | Notes                                         |
|-------:|--------|-----------|-----------------------------------------------|
| 0      | `seq`  | `u64`     | monotonic emission counter                    |
| 8      | `ts`   | `u64`     | event time, nanoseconds                       |
| 16     | `px`   | `i64`     | price in integer ticks                        |
| 24     | `qty`  | `i64`     | quantity in integer lots (`0` ⇒ remove level) |
| 32     | `side` | `u8`      | `0 = Bid`, `1 = Ask` (aggressor for `Trade`)  |
| 33     | `kind` | `u8`      | `0 = Level`, `1 = Trade`, `2 = Clear`         |
| 34     | `pad`  | `[u8; 6]` | written as zero; ignored on read              |

`record_size = 40 = size_of::<BookEvent>()` (asserted frozen in Phase 1), so
`count = (file_len - 32) / 40` is exact.

### Loading is safe, validated deserialization — not mmap-and-transmute

`Corpus::load` / `from_bytes` reads the file into one owned buffer and parses each
record with `from_le_bytes`, **validating the `side`/`kind` discriminants** before
constructing each `BookEvent` via the public constructors. `Side`/`EventKind` are
`repr(u8)` enums with only 2/3 valid bit patterns, so casting an arbitrary
(possibly corrupt) byte to one would be instant UB — a zero-copy `&[BookEvent]`
over untrusted bytes is unsound and would *still* need a validation pass. The load
is setup, off the measured path; the resident slice is then iterated **by
reference** (zero-alloc, zero-copy) on the hot path.

Corrupt input fails loudly with a specific `CorpusError`: `BadMagic`,
`UnsupportedVersion`, `BadRecordSize`, `Truncated`, or `BadDiscriminant`.

### Replay is deterministic and timing-free

`feed` never sleeps, paces, or reads the clock. It hands the harness the ordered
events and their recorded `ts`; **open-loop arrival pacing and coordinated-omission
correctness are Phase 4's job.** A given `.mdf` replays to an identical event
sequence on every run and every machine.

---

## Regenerate the synthetic corpora

The `gen` binary is **default-feature** (pure library, no async, no third-party
deps). `--all` writes the canonical committed fixtures at `seed=1`,
`events=100_000`, `mid=65000`, `band=64`, `max_qty=1024`, `start_ts=0`:

```sh
cargo run -p feed --bin gen -- --all
# writes feed/corpus/{steady,burst,flashcrash}-s1-100k.mdf + .meta.json
```

Single corpus form:

```sh
cargo run -p feed --bin gen -- \
    --profile <steady|burst|flashcrash> --seed <u64> --events <n> --out <path>
```

Identical `(profile, seed, config)` ⇒ byte-identical corpus.

---

## Record a real Binance session (quarantined)

The recorder is async (`tokio`) and lives behind the Cargo-enforced `recorder`
feature. It is run **once, by hand**, never on the measured path and never in CI.
The two REST calls (`exchangeInfo`, depth snapshot) and the `wss` diff stream
reuse one rustls/ring + webpki-roots stack — no extra HTTP-client crate.

```sh
cargo run -p feed --features recorder --bin recorder -- \
    --symbol BTCUSDT --duration-secs 60 --out feed/corpus/btcusdt-sample.mdf \
    [--max-events N] [--with-trades]
```

Flow (Binance's documented depth-diff + local-book reconciliation):

1. `GET /api/v3/exchangeInfo` → `PRICE_FILTER.tickSize`, `LOT_SIZE.stepSize`.
2. Open `wss://stream.binance.com:9443/ws`, `SUBSCRIBE <sym>@depth@100ms`
   (+ `<sym>@trade` with `--with-trades`); buffer diffs immediately.
3. `GET /api/v3/depth?limit=1000` → `lastUpdateId` + snapshot levels.
4. Drop buffered diffs with `u <= lastUpdateId`; emit `Clear` then a `Level` per level.
5. Apply the first diff with `U <= lastUpdateId+1 <= u`; thereafter require
   `U == prev_u + 1`. On a gap: log and re-snapshot (re-`Clear` + reseed).
6. With `--with-trades`: emit `Trade` (`aggressor = if m { Ask } else { Bid }`).
7. `seq` = local monotonic counter; `ts` = exchange event time `E`(ms) × 1_000_000.
8. Bounded by `--duration-secs` / `--max-events` / SIGINT → `Corpus::save` + `.meta.json`.

The string→tick conversion is **exact integer arithmetic** (i128 common-scale,
no `f64` even here): `Px = price / tickSize`, `Qty = qty / stepSize`, both exact
integer counts; off-grid values are rejected. Keep committed captures short
(target ≤ a few MB); the synthetic corpora are the primary fixtures, the real
sample exists for credibility.

### The async quarantine (enforced by Cargo, not discipline)

`tokio`, `tokio-tungstenite`, `tokio-rustls`, `webpki-roots`, `futures-util`,
`serde`, and `serde_json` are **optional dependencies behind the `recorder`
feature**. The default `feed` tree — library, replay, synthetic generator, `gen`
binary, and every downstream crate (`bench`, `engine`) — links **none** of them.

```sh
cargo tree -p feed                     # shows ONLY book
cargo tree -p feed --features recorder # shows the async stack
```

---

## Committed corpus manifest

Every committed corpus has a `*.meta.json` provenance sidecar. Synthetic corpora
are regenerable from `(profile, seed, config)`; the recorded sample traces to a
real capture with its scales and timestamps.

| File | Bytes | Records | Kind | Provenance |
|------|------:|--------:|------|------------|
| `steady-s1-100k.mdf`     | 4,000,032 | 100,000 | synthetic · steady     | seed 1, mid 65000, band 64, max_qty 1024, generator v1 |
| `burst-s1-100k.mdf`      | 4,000,032 | 100,000 | synthetic · burst      | seed 1, mid 65000, band 64, max_qty 1024, generator v1 |
| `flashcrash-s1-100k.mdf` | 4,000,032 | 100,000 | synthetic · flashcrash | seed 1, mid 65000, band 64, max_qty 1024, generator v1 |
| `btcusdt-sample.mdf`     |   550,632 |  13,765 | binance-recorded       | BTCUSDT, tickSize 0.01, stepSize 0.00001, with trades, captured 2026-06-03T19:13:38Z |

---

## Module map

| Path | Role |
|------|------|
| `src/corpus.rs`      | `.mdf` format, `Corpus` (load/save/from_bytes/write_to/replay), `CorpusError` |
| `src/synthetic.rs`   | `Profile`, `GenConfig`, `generate()` — the steady/burst/flash-crash semantics |
| `src/rng.rs`         | public `SplitMix64` — reproducible seeded generation |
| `src/bin/gen.rs`     | default-feature CLI; writes the canonical synthetic corpora |
| `src/bin/recorder.rs`| feature-gated async Binance recorder (quarantined) |

After Phase 3, `feed` provides deterministic, integer-tick event sequences. Next
is Phase 4 (`bench`: the open-loop, coordinated-omission-correct shootout that
finally measures the four books).

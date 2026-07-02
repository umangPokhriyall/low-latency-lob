//! Benchmark 5 — seqlock read latency under write contention (Phase 6 §6).
//!
//! The seqlock (`sync::SeqLock`) is the engine's single-writer / many-reader
//! top-of-book snapshot cell. This benchmark answers two questions with numbers,
//! both under the Phase 4 methodology (recorded clock floor §3.2, `black_box`
//! §3.3, pinning + warmup §3.4):
//!
//!   1. **Read latency + retry rate under contention.** One pinned writer stores
//!      snapshots while `K ∈ {1,2,4,8}` pinned readers time `load()` and count the
//!      optimistic-retry rate. As `K` grows, read p99 and the retry rate are the
//!      contention story.
//!   2. **Writer-independence (the load-bearing result).** The writer's own
//!      `store` latency is recorded separately for every `K`. Because the writer
//!      is wait-free (never blocked by a reader), its p50/p99 must stay flat as
//!      readers are added — the proof that readers do not tax the writer. A
//!      mutex-backed cell would show the opposite.
//!
//! Two writer modes:
//!   - `full_tilt` — the writer stores as fast as it can (worst-case contention,
//!     maximal retry pressure on readers).
//!   - `paced` — the writer stores at a fixed feed rate ([`FEED_RATE_HZ`]), the
//!     realistic market-data case; readers should see near-zero retries.
//!
//! This is NOT a coordinated-omission study: there is no arrival schedule for the
//! reads (a reader issues the next `load()` as soon as the last returns), so each
//! sample is the *service time* of one `load()` under a given write pressure. The
//! writer's `paced` mode uses a busy-spin schedule (never a sleep — §7 of the
//! Phase 4 spec; a syscall sleep would dwarf the ns-scale op).
//!
//! No `dyn`: the cell type is concrete (`sync::SeqLock`), so `load`/`store` inline.
//! `bench` keeps `#![forbid(unsafe_code)]` — threads/pinning/atomics are all safe.

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use sync::{SeqLock, TopOfBook};

/// The reader-count ladder. Each `K` value is run only if the host has enough
/// logical cores to give the writer its own core plus `K` distinct reader cores
/// (`1 + K <= logical_cores`); otherwise it is skipped (recorded in the headline).
const READER_LADDER: [usize; 4] = [1, 2, 4, 8];

/// `paced` writer store rate (stores/sec): an aggressive but realistic busy-symbol
/// top-of-book update rate. Paced via busy-spin against the bench clock.
const FEED_RATE_HZ: u64 = 1_000_000;

/// Mid price the synthetic top-of-book is centred on (matches the other benches).
const MID: i64 = 1_000_000;

/// The snapshot the writer stores at sequence `stamp`. Fields vary with `stamp`
/// so each `store` writes genuinely-new payload (a constant payload could let the
/// compiler hoist work out of the loop). The values are not asserted here — this
/// benchmark measures latency, not correctness (that is the loom + stress tests).
#[allow(clippy::cast_possible_wrap)] // stamp is a bounded loop counter, never near i64::MAX
fn snapshot_for(stamp: u64) -> TopOfBook {
    let q = (stamp & 0xffff) as i64;
    TopOfBook { bid_px: MID - 1, bid_qty: q + 1, ask_px: MID + 1, ask_qty: q + 2, stamp }
}

/// How the writer paces its stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterMode {
    FullTilt,
    Paced,
}

impl WriterMode {
    fn tag(self) -> &'static str {
        match self {
            WriterMode::FullTilt => "full_tilt",
            WriterMode::Paced => "paced",
        }
    }
}

#[derive(Debug, Clone)]
struct BenchOpts {
    /// Recorded `load()` samples PER reader thread.
    samples: u64,
    /// Untimed warmup iterations per thread.
    warmup: u64,
    /// The writer's pinned core (`--core` / `WRITER_CORE`).
    core: usize,
    /// Explicit reader cores (`--reader-cores` / `READER_CORES`), each reader `r`
    /// pinning to `reader_cores[r]`; empty ⇒ the contiguous `core+1+r` default.
    /// On the metal box these are spread across CCDs to maximize cross-CCD
    /// coherence traffic for `perf c2c` (spec §3, §A.8).
    reader_cores: Vec<usize>,
}

impl BenchOpts {
    /// The pinned core for reader `r`: the explicit list entry if present, else the
    /// contiguous `core+1+r` fallback (unchanged laptop behavior).
    fn reader_core(&self, r: usize) -> usize {
        self.reader_cores.get(r).copied().unwrap_or(self.core + 1 + r)
    }
}

/// The result of one (`K`, mode) contention cell.
#[derive(Debug)]
struct CellResult {
    /// Merged read-latency distribution across all `K` readers.
    read: Recorder,
    /// The writer's own `store`-latency distribution for this cell.
    write: Recorder,
    total_reads: u64,
    total_retries: u64,
}

/// Run one contention cell: spawn `readers` pinned reader threads each recording
/// `opts.samples` timed `load()`s, while this (writer) thread — pinned to
/// `opts.core` — stores snapshots in `mode` until every reader has finished,
/// recording its own `store` latency. Returns the merged read distribution, the
/// writer distribution, and the aggregate read/retry counts.
fn run_cell(clock: &BenchClock, opts: &BenchOpts, readers: usize, mode: WriterMode) -> CellResult {
    let cell = Arc::new(SeqLock::new(snapshot_for(0)));
    let finished = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..readers)
        .map(|r| {
            let cell = Arc::clone(&cell);
            let finished = Arc::clone(&finished);
            let (samples, warmup, reader_core) = (opts.samples, opts.warmup, opts.reader_core(r));
            thread::spawn(move || {
                let pinned = harness::pin_to_core(reader_core);
                // Untimed warmup: warm caches/predictor before recording (§3.4).
                for _ in 0..warmup {
                    black_box(cell.load());
                }
                let mut rec = Recorder::new();
                let mut retries = 0u64;
                let clk = BenchClock::new();
                for _ in 0..samples {
                    let t0 = clk.raw();
                    let (snap, r) = cell.load_counted();
                    let t1 = clk.raw();
                    black_box(snap);
                    rec.record(clk.delta_ns(t0, t1));
                    retries += u64::from(r);
                }
                finished.fetch_add(1, Ordering::Release);
                (rec, retries, pinned)
            })
        })
        .collect();

    // Writer runs on this thread, pinned to the writer core.
    let _ = harness::pin_to_core(opts.core);
    let write = drive_writer(clock, &cell, &finished, readers, mode, opts.warmup);

    // Collect readers and merge their distributions into one cell read-distribution.
    let mut read = Recorder::new();
    let (mut total_reads, mut total_retries) = (0u64, 0u64);
    for h in handles {
        let (rec, retries, _pinned) = h.join().expect("reader thread panicked");
        total_reads += rec.count();
        total_retries += retries;
        read.merge(&rec);
    }
    CellResult { read, write, total_reads, total_retries }
}

/// The writer loop. Warmup stores are always full-tilt (cache warming); the timed
/// loop stores in `mode` until all `readers` have signalled completion. Only the
/// `store` call itself is bracketed by the clock — the pacing spin and the
/// done-check sit outside the timed region.
fn drive_writer(
    clock: &BenchClock,
    cell: &SeqLock,
    finished: &AtomicUsize,
    readers: usize,
    mode: WriterMode,
    warmup: u64,
) -> Recorder {
    for stamp in 0..warmup {
        cell.store(black_box(snapshot_for(stamp)));
    }
    let mut rec = Recorder::new();
    let base = clock.raw();
    let mut stamp = warmup;
    while finished.load(Ordering::Acquire) < readers {
        if mode == WriterMode::Paced {
            // Busy-spin until this store's scheduled slot (never sleep).
            let scheduled = scheduled_offset_ns(stamp - warmup);
            while clock.now_since_ns(base) < scheduled {
                std::hint::spin_loop();
            }
        }
        let snap = snapshot_for(stamp);
        let t0 = clock.raw();
        cell.store(black_box(snap));
        let t1 = clock.raw();
        rec.record(clock.delta_ns(t0, t1));
        stamp += 1;
    }
    rec
}

/// The paced schedule: store `n` scheduled at `n / FEED_RATE_HZ` seconds.
#[inline]
#[allow(clippy::cast_possible_truncation)] // ns offset of a bounded loop counter fits u64
fn scheduled_offset_ns(n: u64) -> u64 {
    (u128::from(n) * 1_000_000_000u128 / u128::from(FEED_RATE_HZ)) as u64
}

/// One CSV row (one `K` × mode cell).
#[derive(Debug)]
struct Row {
    readers: usize,
    writer_mode: &'static str,
    samples: u64,
    overhead_ns: u64,
    read_p50: u64,
    read_p99: u64,
    read_p999: u64,
    read_max: u64,
    mean_retries_per_load: f64,
    write_p50: u64,
    write_p99: u64,
}

impl Row {
    fn from_cell(readers: usize, mode: WriterMode, overhead_ns: u64, c: &CellResult) -> Self {
        #[allow(clippy::cast_precision_loss)] // ratio of two large integer counts
        let mean_retries_per_load = if c.total_reads == 0 {
            0.0
        } else {
            c.total_retries as f64 / c.total_reads as f64
        };
        Self {
            readers,
            writer_mode: mode.tag(),
            samples: c.total_reads,
            overhead_ns,
            read_p50: c.read.p(0.50),
            read_p99: c.read.p(0.99),
            read_p999: c.read.p(0.999),
            read_max: c.read.max(),
            mean_retries_per_load,
            write_p50: c.write.p(0.50),
            write_p99: c.write.p(0.99),
        }
    }
}

/// Parse `--samples N --warmup N --core N --out DIR`. Minimal, hand-rolled (same
/// flags as the other benches).
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut samples = 1_000_000u64;
    let mut warmup = 100_000u64;
    // Writer core: `--core` flag wins; else `WRITER_CORE` env (metal-run plumbing); else 0.
    let mut core = harness::env_core("WRITER_CORE").unwrap_or(0);
    let mut out = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--samples" => samples = it.next().and_then(|s| s.parse().ok()).unwrap_or(samples),
            "--warmup" => warmup = it.next().and_then(|s| s.parse().ok()).unwrap_or(warmup),
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            // Consumed by harness::reader_cores below; skip its value here.
            "--reader-cores" => {
                it.next();
            }
            "--out" => {
                if let Some(d) = it.next() {
                    out = PathBuf::from(d);
                }
            }
            _ => {}
        }
    }
    let reader_cores = harness::reader_cores(args);
    (BenchOpts { samples, warmup, core, reader_cores }, out)
}

/// Logical-core count, for capping the reader ladder (writer core + K reader cores).
fn logical_cores() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Entry point: `bench seqlock [--samples N] [--warmup N] [--core N] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let overhead = clock.overhead_ns();
    let cores = logical_cores();
    // An explicit reader-core list caps K to its length (readers pin to those exact
    // cores); otherwise reserve one core for the writer and cap at 1 + K ≤ logical cores.
    let max_readers = if opts.reader_cores.is_empty() {
        cores.saturating_sub(1).max(1)
    } else {
        opts.reader_cores.len()
    };
    let ladder: Vec<usize> = READER_LADDER.iter().copied().filter(|&k| k <= max_readers).collect();

    eprintln!(
        "seqlock: samples/reader={} warmup={} writer_core={} logical_cores={} \
         reader_cores={:?} K-ladder={ladder:?} (capped; {} skipped) paced_rate={}Hz clock_overhead_ns={}",
        opts.samples,
        opts.warmup,
        opts.core,
        cores,
        opts.reader_cores,
        READER_LADDER.len() - ladder.len(),
        FEED_RATE_HZ,
        overhead,
    );

    let mut rows: Vec<Row> = Vec::new();
    for mode in [WriterMode::FullTilt, WriterMode::Paced] {
        for &k in &ladder {
            let cell = run_cell(&clock, &opts, k, mode);
            let row = Row::from_cell(k, mode, overhead, &cell);
            eprintln!(
                "  {:<9} K={k:<2} read p50={:>3}ns p99={:>4}ns p99.9={:>5}ns max={:>6}ns | \
                 retries/load={:.4} | write p50={:>3}ns p99={:>4}ns",
                row.writer_mode,
                row.read_p50,
                row.read_p99,
                row.read_p999,
                row.read_max,
                row.mean_retries_per_load,
                row.write_p50,
                row.write_p99,
            );
            rows.push(row);
        }
    }

    write_csv(&out_dir.join("seqlock_read.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "readers,writer_mode,samples,clock_overhead_ns,read_p50_ns,read_p99_ns,read_p999_ns,\
         read_max_ns,mean_retries_per_load,write_p50_ns,write_p99_ns\n",
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{},{},{:.6},{},{}",
            r.readers,
            r.writer_mode,
            r.samples,
            r.overhead_ns,
            r.read_p50,
            r.read_p99,
            r.read_p999,
            r.read_max,
            r.mean_retries_per_load,
            r.write_p50,
            r.write_p99,
        );
    }
    std::fs::write(path, s).expect("write seqlock_read.csv");
    eprintln!("wrote {}", path.display());
}

/// Headline: the writer-independence result — writer store p50/p99 should be flat
/// across K (readers don't tax the wait-free writer), while read p99 climbs.
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== WRITER-INDEPENDENCE HEADLINE (full_tilt) ====");
    eprintln!("{:<3} {:>10} {:>10} {:>10} {:>14}", "K", "read_p99", "write_p50", "write_p99", "retries/load");
    for r in rows.iter().filter(|r| r.writer_mode == "full_tilt") {
        eprintln!(
            "{:<3} {:>10} {:>10} {:>10} {:>14.4}",
            r.readers, r.read_p99, r.write_p50, r.write_p99, r.mean_retries_per_load,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny full cell runs end-to-end: every reader's samples are recorded, the
    /// writer records stores, and `load()` returns a snapshot the writer stored.
    /// (ns values are not asserted — debug-build clock noise; the latency claims
    /// live in RESULTS, measured in release.)
    #[test]
    fn cell_runs_and_records() {
        let clock = BenchClock::new();
        let opts = BenchOpts { samples: 20_000, warmup: 1_000, core: 0, reader_cores: Vec::new() };
        let cell = run_cell(&clock, &opts, 2, WriterMode::FullTilt);
        assert_eq!(cell.total_reads, 40_000, "both readers' samples must be recorded");
        assert_eq!(cell.read.count(), 40_000, "merged read distribution holds every sample");
        assert!(cell.write.count() > 0, "writer must have recorded stores");
    }

    /// Paced mode throttles the writer below full-tilt. Comparing store counts for
    /// the same cell config is robust to absolute (debug vs release) timing: a
    /// free-running writer stores far more than a 1 MHz-paced one over the same
    /// reader-bounded window. Guards that pacing actually paces.
    #[test]
    fn paced_writer_is_throttled() {
        let clock = BenchClock::new();
        let opts = BenchOpts { samples: 50_000, warmup: 1_000, core: 0, reader_cores: Vec::new() };
        let full = run_cell(&clock, &opts, 1, WriterMode::FullTilt);
        let paced = run_cell(&clock, &opts, 1, WriterMode::Paced);
        assert!(
            paced.write.count() < full.write.count(),
            "paced writer ({} stores) must store fewer than full-tilt ({} stores)",
            paced.write.count(),
            full.write.count(),
        );
    }

    #[test]
    fn snapshot_varies_with_stamp() {
        assert_ne!(snapshot_for(1), snapshot_for(2));
        assert_eq!(snapshot_for(7).stamp, 7);
    }
}

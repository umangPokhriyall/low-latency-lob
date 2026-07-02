//! Benchmark 6 — SPMC broadcast-ring throughput, latency, and false-sharing
//! evidence (Phase 7 §6).
//!
//! The ring (`sync::SpmcRing`) is the engine's single-producer / many-independent-
//! consumer output bus: one writer streams `[u64; W]` records and never blocks; each
//! consumer reads the whole stream from its own cursor and detects overrun. This
//! benchmark answers four questions with numbers, all under the Phase 4 methodology
//! (recorded clock floor §3.2, `black_box` §3.3, pinning + warmup §3.4):
//!
//!   1. **Producer push throughput (Mev/s).** A pinned producer pushes a fixed
//!      budget full-tilt while `K ∈ {1,2,4,8}` pinned consumers drain.
//!   2. **False-sharing evidence (the load-bearing result).** That producer
//!      throughput must stay ~**flat** as `K` rises — consumers read distinct cache
//!      lines (one slot per line, `#[repr(align(64))]`) and the write position sits
//!      on its own line, so adding consumers must not tax the writer. A throughput
//!      collapse as `K` grows would betray false sharing. The curve is the test.
//!   3. **Latency.** `push` and `try_recv` latency distributions (`black_box`ed
//!      payloads), expected near the clock floor.
//!   4. **Overrun rate vs consumer speed.** Two producer modes — `full_tilt`
//!      (consumers cannot keep up → overruns) and `paced` (a fixed feed rate →
//!      consumers keep up → ~zero overrun).
//!
//! This is **service time, not a coordinated-omission study**: a consumer issues its
//! next `try_recv` as soon as the last returns (no arrival schedule for receives),
//! and the producer's per-push timestamp brackets only the `push` call. The clock
//! floor is reported and never subtracted. `paced` uses a busy-spin schedule (never
//! a sleep — a syscall would dwarf the ns-scale op).
//!
//! No `dyn`: the ring type is concrete (`sync::SpmcRing<W>`), so `push`/`try_recv`
//! inline. `bench` keeps `#![forbid(unsafe_code)]` — threads/pinning/atomics are safe.

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use sync::{Consumer, Recv, SpmcRing};

/// Payload width in `u64` words per record (the engine packs a `BookEvent` into
/// this; here the words are a position-derived pattern so each push is genuinely
/// new payload the compiler cannot hoist).
const WORDS: usize = 4;

/// Ring capacity (slots, a power of two). Moderate: large enough that a keeping-up
/// consumer rarely laps in `paced` mode, small enough that a full-tilt producer
/// laps a draining consumer often (so the overrun-rate story has both regimes).
const RING_CAP: usize = 1024;

/// The consumer-count ladder. A `K` runs only if the host has the producer's core
/// plus `K` distinct consumer cores (`1 + K ≤ logical_cores`); otherwise skipped.
const CONSUMER_LADDER: [usize; 4] = [1, 2, 4, 8];

/// `paced` producer push rate (records/sec), busy-spun against the bench clock — an
/// aggressive but realistic market-data output rate at which consumers keep up.
const FEED_RATE_HZ: u64 = 1_000_000;

/// Pushes in the dedicated push-latency pass. Throughput is measured over a SEPARATE
/// untimed pass so `producer_mev_s` reflects the pure push rate, not the per-push
/// clock-read + histogram-record harness overhead (which is compute-bound and, under
/// many active consumers, suffers shared-cache pressure that would otherwise show up
/// as a throughput-vs-K slope unrelated to the push op itself).
const LATENCY_PUSHES: u64 = 200_000;

/// The record pushed at `pos`: word `k = pos*WORDS + k`, so every push writes new
/// payload (a constant record could be hoisted out of the loop). Not asserted here —
/// this benchmark measures throughput/latency, not correctness (loom + stress do that).
fn payload_for(pos: u64) -> [u64; WORDS] {
    core::array::from_fn(|k| pos.wrapping_mul(WORDS as u64).wrapping_add(k as u64))
}

/// How the producer paces its pushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProducerMode {
    FullTilt,
    Paced,
}

impl ProducerMode {
    fn tag(self) -> &'static str {
        match self {
            ProducerMode::FullTilt => "full_tilt",
            ProducerMode::Paced => "paced",
        }
    }
}

#[derive(Debug, Clone)]
struct BenchOpts {
    /// Recorded `push` samples (the producer's timed budget, N).
    samples: u64,
    /// Untimed warmup iterations per thread.
    warmup: u64,
    /// The producer's pinned core (`--core` / `PRODUCER_CORE`).
    core: usize,
    /// Explicit consumer cores (`--reader-cores` / `READER_CORES`), each consumer
    /// `r` pinning to `reader_cores[r]`; empty ⇒ the contiguous `core+1+r` default.
    /// On the metal box these are spread across CCDs to maximize cross-CCD
    /// coherence traffic for `perf c2c` (spec §3, §A.8).
    reader_cores: Vec<usize>,
}

impl BenchOpts {
    /// The pinned core for consumer `r`: the explicit list entry if present, else the
    /// contiguous `core+1+r` fallback (unchanged laptop behavior).
    fn consumer_core(&self, r: usize) -> usize {
        self.reader_cores.get(r).copied().unwrap_or(self.core + 1 + r)
    }
}

/// The per-consumer tally returned from each consumer thread.
#[derive(Debug)]
struct ConsumerStats {
    recv: Recorder,
    deliveries: u64,
    overruns: u64,
    skipped: u64,
    /// Active (recorded) drain window in ns, for per-consumer drain throughput.
    elapsed_ns: u64,
    pinned: bool,
}

/// The result of one (`K`, mode) cell.
#[derive(Debug)]
struct CellResult {
    /// The producer's `push`-latency distribution for this cell.
    push: Recorder,
    /// Merged `try_recv`-on-`Item` latency across all `K` consumers.
    recv: Recorder,
    pushed: u64,
    producer_mev_s: f64,
    consumer_mev_s_mean: f64,
    total_deliveries: u64,
    total_overruns: u64,
    total_skipped: u64,
    all_pinned: bool,
}

/// Producer-derived throughput in Mev/s: `n` pushes over `elapsed_ns` nanoseconds.
#[allow(clippy::cast_precision_loss)] // ratio of two large integer counts -> Mev/s
fn mev_per_s(n: u64, elapsed_ns: u64) -> f64 {
    if elapsed_ns == 0 {
        0.0
    } else {
        n as f64 * 1000.0 / elapsed_ns as f64 // n / (elapsed_ns/1e9) / 1e6 = n*1e3/elapsed_ns
    }
}

/// Run one consumer: pin, warm up untimed, then drain recording `try_recv`-on-`Item`
/// latency and accounting deliveries / overruns / skipped until the producer is done
/// and we have caught up to `total` positions.
fn run_consumer(
    clock: &BenchClock,
    mut c: Consumer<WORDS>,
    core: usize,
    warmup: u64,
    total: u64,
    done: &AtomicBool,
) -> ConsumerStats {
    let pinned = harness::pin_to_core(core);
    // Untimed warmup: warm the try_recv code path / predictor before recording (§3.4).
    for _ in 0..warmup {
        black_box(c.try_recv());
    }

    let mut recv = Recorder::new();
    let (mut deliveries, mut overruns, mut skipped) = (0u64, 0u64, 0u64);
    let base = clock.raw();
    loop {
        let t0 = clock.raw();
        let r = c.try_recv();
        let t1 = clock.raw();
        match r {
            Recv::Item(rec) => {
                black_box(rec);
                recv.record(clock.delta_ns(t0, t1)); // recv latency = cost to receive one record
                deliveries += 1;
            }
            Recv::Overrun { skipped: s } => {
                overruns += 1;
                skipped += s;
            }
            Recv::Empty => {
                if done.load(Ordering::Acquire) && c.cursor() >= total {
                    break;
                }
                std::hint::spin_loop();
            }
        }
    }
    let elapsed_ns = clock.delta_ns(base, clock.raw());
    ConsumerStats { recv, deliveries, overruns, skipped, elapsed_ns, pinned }
}

/// Drive the producer on its pinned thread: warmup pushes untimed, then `n` timed
/// pushes recording `push` latency, paced per `mode`. Returns the push distribution
/// and the producer's own throughput. Sets `done` when the full budget is pushed.
fn drive_producer(
    clock: &BenchClock,
    producer: &mut sync::Producer<WORDS>,
    n: u64,
    warmup: u64,
    mode: ProducerMode,
    done: &AtomicBool,
) -> (Recorder, f64) {
    for pos in 0..warmup {
        producer.push(black_box(payload_for(pos)));
    }
    let mut pos = warmup;

    // Throughput pass: `n` UNTIMED pushes (paced if requested). No per-push clock
    // read or histogram record, so producer_mev_s is the pure push rate — the
    // false-sharing signal, undistorted by harness overhead.
    let base = clock.raw();
    for i in 0..n {
        if mode == ProducerMode::Paced {
            // Busy-spin until this push's scheduled slot (never sleep).
            while clock.now_since_ns(base) < scheduled_offset_ns(i) {
                std::hint::spin_loop();
            }
        }
        producer.push(black_box(payload_for(pos)));
        pos += 1;
    }
    let producer_mev_s = mev_per_s(n, clock.delta_ns(base, clock.raw()));

    // Latency pass: a separate steady-state burst of timed pushes (paced if
    // requested) for the push-latency distribution.
    let mut rec = Recorder::new();
    let lat_base = clock.raw();
    for j in 0..LATENCY_PUSHES {
        if mode == ProducerMode::Paced {
            while clock.now_since_ns(lat_base) < scheduled_offset_ns(j) {
                std::hint::spin_loop();
            }
        }
        let payload = payload_for(pos);
        let t0 = clock.raw();
        producer.push(black_box(payload));
        let t1 = clock.raw();
        rec.record(clock.delta_ns(t0, t1));
        pos += 1;
    }
    done.store(true, Ordering::Release);
    (rec, producer_mev_s)
}

/// The paced schedule: push `n` scheduled at `n / FEED_RATE_HZ` seconds.
#[inline]
#[allow(clippy::cast_possible_truncation)] // ns offset of a bounded loop counter fits u64
fn scheduled_offset_ns(n: u64) -> u64 {
    (u128::from(n) * 1_000_000_000u128 / u128::from(FEED_RATE_HZ)) as u64
}

/// Run one (`K`, mode) cell: spawn `consumers` pinned draining threads PLUS a pinned
/// producer thread, then merge the distributions. The producer runs in its OWN
/// spawned thread (not the main thread) so the main thread is never pinned — every
/// worker thread therefore inherits a full affinity mask and pins to its own core
/// (pinning the long-lived main thread would restrict the mask its children inherit,
/// silently un-pinning later cells and oversubscribing a few cores).
fn run_cell(
    clock: &Arc<BenchClock>,
    opts: &BenchOpts,
    consumers: usize,
    mode: ProducerMode,
) -> CellResult {
    let (mut producer, handle) = SpmcRing::<WORDS>::with_capacity(RING_CAP).split();
    let done = Arc::new(AtomicBool::new(false));
    // Positions pushed: warmup (untimed) + samples (throughput pass) + the latency pass.
    let total = opts.warmup + opts.samples + LATENCY_PUSHES;

    // Create every consumer at cursor 0 BEFORE the producer pushes anything.
    let handles: Vec<_> = (0..consumers)
        .map(|r| {
            let c = handle.consumer();
            let clock = Arc::clone(clock);
            let done = Arc::clone(&done);
            let (warmup, core) = (opts.warmup, opts.consumer_core(r));
            thread::spawn(move || run_consumer(&clock, c, core, warmup, total, &done))
        })
        .collect();

    // Producer in its own pinned thread (see fn-doc on why not the main thread).
    let producer_handle = {
        let clock = Arc::clone(clock);
        let done = Arc::clone(&done);
        let (n, warmup, core) = (opts.samples, opts.warmup, opts.core);
        thread::spawn(move || {
            let pinned = harness::pin_to_core(core);
            let (push, mev) = drive_producer(&clock, &mut producer, n, warmup, mode, &done);
            (push, mev, pinned)
        })
    };
    let (push, producer_mev_s, prod_pinned) =
        producer_handle.join().expect("producer thread panicked");

    let mut recv = Recorder::new();
    let (mut total_deliveries, mut total_overruns, mut total_skipped) = (0u64, 0u64, 0u64);
    let mut consumer_mev_sum = 0.0f64;
    let mut all_pinned = prod_pinned;
    for h in handles {
        let s = h.join().expect("consumer thread panicked");
        total_deliveries += s.deliveries;
        total_overruns += s.overruns;
        total_skipped += s.skipped;
        consumer_mev_sum += mev_per_s(s.deliveries, s.elapsed_ns);
        all_pinned &= s.pinned;
        recv.merge(&s.recv);
    }
    #[allow(clippy::cast_precision_loss)] // K is tiny
    let consumer_mev_s_mean = consumer_mev_sum / consumers as f64;

    CellResult {
        push,
        recv,
        pushed: opts.samples,
        producer_mev_s,
        consumer_mev_s_mean,
        total_deliveries,
        total_overruns,
        total_skipped,
        all_pinned,
    }
}

/// One CSV row (one `K` × mode cell).
#[derive(Debug)]
struct Row {
    mode: &'static str,
    consumers: usize,
    capacity: usize,
    words: usize,
    samples: u64,
    overhead_ns: u64,
    push_p50: u64,
    push_p99: u64,
    recv_p50: u64,
    recv_p99: u64,
    producer_mev_s: f64,
    overrun_rate: f64,
}

impl Row {
    fn from_cell(consumers: usize, mode: ProducerMode, overhead_ns: u64, c: &CellResult) -> Self {
        // overrun_rate = fraction of consumer-observed positions lost to overrun.
        #[allow(clippy::cast_precision_loss)] // ratio of two large integer counts
        let overrun_rate = {
            let observed = c.total_deliveries + c.total_skipped;
            if observed == 0 { 0.0 } else { c.total_skipped as f64 / observed as f64 }
        };
        Self {
            mode: mode.tag(),
            consumers,
            capacity: RING_CAP,
            words: WORDS,
            samples: c.pushed,
            overhead_ns,
            push_p50: c.push.p(0.50),
            push_p99: c.push.p(0.99),
            recv_p50: c.recv.p(0.50),
            recv_p99: c.recv.p(0.99),
            producer_mev_s: c.producer_mev_s,
            overrun_rate,
        }
    }
}

/// Parse `--samples N --warmup N --core N --out DIR`. Minimal, hand-rolled (same
/// flags as the other benches).
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut samples = 1_000_000u64;
    let mut warmup = 100_000u64;
    // Producer core: `--core` flag wins; else `PRODUCER_CORE` env (metal-run plumbing); else 0.
    let mut core = harness::env_core("PRODUCER_CORE").unwrap_or(0);
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

/// Logical-core count, for capping the consumer ladder (producer core + K consumers).
fn logical_cores() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Entry point: `bench ring [--samples N] [--warmup N] [--core N] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = Arc::new(BenchClock::new());
    let overhead = clock.overhead_ns();
    let cores = logical_cores();
    // An explicit consumer-core list caps K to its length (consumers pin to those exact
    // cores); otherwise reserve one core for the producer and cap at 1 + K ≤ logical cores.
    let max_consumers = if opts.reader_cores.is_empty() {
        cores.saturating_sub(1).max(1)
    } else {
        opts.reader_cores.len()
    };
    let ladder: Vec<usize> =
        CONSUMER_LADDER.iter().copied().filter(|&k| k <= max_consumers).collect();

    eprintln!(
        "ring: samples(push)={} warmup={} producer_core={} logical_cores={} \
         consumer_cores={:?} K-ladder={ladder:?} (capped; {} skipped) cap={RING_CAP} words={WORDS} \
         paced_rate={FEED_RATE_HZ}Hz clock_overhead_ns={overhead}",
        opts.samples,
        opts.warmup,
        opts.core,
        cores,
        opts.reader_cores,
        CONSUMER_LADDER.len() - ladder.len(),
    );

    let mut rows: Vec<Row> = Vec::new();
    for mode in [ProducerMode::FullTilt, ProducerMode::Paced] {
        for &k in &ladder {
            let cell = run_cell(&clock, &opts, k, mode);
            let row = Row::from_cell(k, mode, overhead, &cell);
            eprintln!(
                "  {:<9} K={k:<2} push p50={:>3}ns p99={:>4}ns | recv p50={:>3}ns p99={:>4}ns | \
                 producer={:>7.2}Mev/s consumer~{:>7.2}Mev/s | deliv={} overrun_rate={:.4}{}",
                row.mode,
                row.push_p50,
                row.push_p99,
                row.recv_p50,
                row.recv_p99,
                row.producer_mev_s,
                cell.consumer_mev_s_mean,
                cell.total_deliveries,
                row.overrun_rate,
                if cell.all_pinned { "" } else { " [PIN FAILED]" },
            );
            let _ = cell.total_overruns; // counted into skipped/overrun_rate; not a CSV column
            rows.push(row);
        }
    }

    write_csv(&out_dir.join("ring_bench.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "mode,consumers,capacity,words,samples,clock_overhead_ns,push_p50_ns,push_p99_ns,\
         recv_p50_ns,recv_p99_ns,producer_mev_s,overrun_rate\n",
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{},{},{},{},{:.4},{:.6}",
            r.mode,
            r.consumers,
            r.capacity,
            r.words,
            r.samples,
            r.overhead_ns,
            r.push_p50,
            r.push_p99,
            r.recv_p50,
            r.recv_p99,
            r.producer_mev_s,
            r.overrun_rate,
        );
    }
    std::fs::write(path, s).expect("write ring_bench.csv");
    eprintln!("wrote {}", path.display());
}

/// Headline: the false-sharing result — producer push throughput should stay FLAT
/// across K (consumers read distinct lines; the write position is isolated).
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== FALSE-SHARING HEADLINE (full_tilt: producer Mev/s vs K — expect FLAT) ====");
    eprintln!("{:<3} {:>14} {:>12} {:>12} {:>14}", "K", "producer_Mev/s", "push_p99", "recv_p99", "overrun_rate");
    for r in rows.iter().filter(|r| r.mode == "full_tilt") {
        eprintln!(
            "{:<3} {:>14.2} {:>12} {:>12} {:>14.4}",
            r.consumers, r.producer_mev_s, r.push_p99, r.recv_p99, r.overrun_rate,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny full cell runs end-to-end: every timed push is recorded, the producer
    /// pushes its budget, and consumers deliver records. (ns values are not asserted
    /// — debug-build clock noise; latency claims live in RESULTS, measured release.)
    #[test]
    fn cell_runs_and_records() {
        let clock = Arc::new(BenchClock::new());
        let opts = BenchOpts { samples: 20_000, warmup: 1_000, core: 0, reader_cores: Vec::new() };
        let cell = run_cell(&clock, &opts, 2, ProducerMode::FullTilt);
        assert_eq!(cell.push.count(), LATENCY_PUSHES, "the latency pass records every timed push");
        assert_eq!(cell.pushed, 20_000, "producer_mev_s is over the `samples` throughput budget");
        assert!(cell.total_deliveries > 0, "consumers must deliver some records");
        // A consumer observes at most `total` positions (it stops at cursor>=total);
        // summed over 2 consumers, observed = delivered+skipped <= 2*total. (Equality
        // does NOT hold: the untimed warmup drains some positions uncounted.)
        let total = (opts.warmup + opts.samples + LATENCY_PUSHES) * 2;
        assert!(
            cell.total_deliveries + cell.total_skipped <= total,
            "observed ({}) cannot exceed produced ({total})",
            cell.total_deliveries + cell.total_skipped,
        );
    }

    /// Paced mode throttles the producer to ~`FEED_RATE_HZ`, far below free-running.
    /// Comparing producer throughput is robust to debug/release timing and core
    /// contention (unlike overrun counts, which depend on scheduler luck): a 1 MHz-
    /// paced producer reports far fewer Mev/s than full-tilt. Guards that pacing paces.
    #[test]
    fn paced_producer_is_throttled() {
        let clock = Arc::new(BenchClock::new());
        let opts = BenchOpts { samples: 40_000, warmup: 1_000, core: 0, reader_cores: Vec::new() };
        let full = run_cell(&clock, &opts, 1, ProducerMode::FullTilt);
        let paced = run_cell(&clock, &opts, 1, ProducerMode::Paced);
        assert!(
            paced.producer_mev_s < full.producer_mev_s,
            "paced producer ({:.3} Mev/s) must be slower than full-tilt ({:.3} Mev/s)",
            paced.producer_mev_s,
            full.producer_mev_s,
        );
    }

    #[test]
    fn payload_varies_with_position() {
        assert_ne!(payload_for(1), payload_for(2));
        assert_eq!(payload_for(3)[0], 12); // 3 * WORDS
    }
}

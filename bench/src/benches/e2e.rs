//! Benchmark 7 — end-to-end production-to-consumption latency of the assembled
//! pipeline (Phase 8 §5).
//!
//! This is the first measurement of the WHOLE composed hot path, not a primitive in
//! isolation: a pinned producer replays a corpus through
//! `EngineProducer::<BTreeBook>::process` — `book.apply` → `seqlock.store(top)` →
//! `ring.push(pack(ev))` — while `K ∈ {1,2,4,8}` pinned, independent consumers each
//! drain the broadcast ring (`EngineConsumer::poll`), do light derived work, resync
//! from the seqlock on overrun, and record the end-to-end latency of every cleanly
//! delivered event. All under the Phase 4 methodology (recorded clock floor §3.2,
//! `black_box` §3.3, pinning + warmup §3.4); no `dyn` (the book is monomorphized);
//! `bench` keeps `#![forbid(unsafe_code)]`.
//!
//! # Coordinated-omission correctness (§3.1, the load-bearing detail)
//! The producer paces the replay to a schedule and **stamps each event's `ts` with
//! its scheduled arrival** (ns from a shared clock base) BEFORE processing it. Each
//! consumer then records `latency = now_since_ns(base) - ev.ts` — completion minus
//! *scheduled* arrival, never minus push time. So when the pipeline falls behind at
//! saturation, the backlog shows up as `now ≫ scheduled` and the tail captures it;
//! the naive "now - `push_time`" form would measure service time and hide the very
//! coordinated omission this study exists to expose. The producer and all consumers
//! share one clock base (published once the producer is past warmup), so the
//! one-way cross-thread latency is measured against a single timeline.
//!
//! # The true-sharing reality (§6 — measured, not engineered around)
//! Producer throughput is expected to FALL as `K` rises: every `Consumer::try_recv`
//! reads the producer's shared `write.v` cursor (TRUE sharing — the `align(64)`
//! slots are isolated, so this is *not* false sharing). At the saturated (free-
//! running) rates the `producer_mev_s` column is that full-tilt throughput under
//! `K`; the §5 producer-throughput-vs-K plot reads it directly. This phase reports
//! the honest curve; it does not modify the verified `sync` primitives.

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use book::{BTreeBook, BookEvent};
use engine::{Engine, EngineConsumer, EngineProducer, Observed};
use feed::corpus::Corpus;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use Ordering::{Acquire, Release};

/// The headline book: the Phase 5 real-data champion (`BTreeBook` leads on BTCUSDT).
const BOOK: &str = "btree";

/// Ring capacity (slots, a power of two). Large enough that consumers keep up at the
/// sustainable rates (few overruns), small enough that a free-running producer at
/// saturation laps a draining consumer — so the overrun story has both regimes.
const RING_CAP: usize = 4096;

/// The consumer-count ladder. A `K` runs only if the host has the producer's core
/// plus `K` distinct consumer cores (`1 + K ≤ logical_cores`); otherwise skipped.
const CONSUMER_LADDER: [usize; 4] = [1, 2, 4, 8];

/// The synthetic fixed-rate ladder (events/sec). Brackets pipeline saturation: the
/// low end is comfortably sustainable, the high end free-runs (the producer cannot
/// keep the schedule), exposing both the bounded-tail and the saturated-tail regimes
/// and giving the producer-throughput-vs-K curve a full-tilt operating point.
const RATES: [u64; 7] = [
    1_000_000,
    2_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    50_000_000,
    100_000_000,
];

/// Untimed `poll` warmup iterations per consumer (warms the `try_recv`/unpack path
/// and predictor on the still-empty ring before any sample is recorded, §3.4).
const WARMUP_POLLS: u64 = 50_000;

/// A rate is *saturated* when achieved throughput plateaus below target — the paced
/// loop could no longer reach scheduled times (§7 saturation rule). 0.90 leaves
/// headroom for spin granularity while still flagging a genuine fall-behind.
const SUSTAIN_FRACTION: f64 = 0.90;

/// The synthetic corpus swept at fixed rates (steady profile, 100k events).
const SYNTH: (&str, &str) =
    ("steady", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/steady-s1-100k.mdf"));

/// The real-arrival corpus replayed at `speed = 1` (the headline real-data run).
const SAMPLE: (&str, &str) =
    ("btcusdt-sample", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/btcusdt-sample.mdf"));

/// The arrival schedule for one run: a fixed synthetic rate (ignores event `ts`) or
/// real-arrival replay (offset = `(ts - first_ts) / speed`). Mirrors Benchmark 3.
#[derive(Debug, Clone, Copy)]
enum Schedule {
    FixedRate(u64),
    RealArrival { first_ts: u64, speed: u64 },
}

impl Schedule {
    /// The scheduled arrival of event `i` as a ns offset from the loop's base.
    #[inline]
    fn offset_ns(self, i: usize, ev: &BookEvent) -> u64 {
        match self {
            #[allow(clippy::cast_possible_truncation)] // ns offset of a bounded loop fits u64
            Schedule::FixedRate(rate) => (i as u128 * 1_000_000_000u128 / u128::from(rate)) as u64,
            Schedule::RealArrival { first_ts, speed } => ev.ts.saturating_sub(first_ts) / speed,
        }
    }
}

/// Producer-derived throughput in Mev/s: `n` events over `elapsed_ns` nanoseconds.
#[allow(clippy::cast_precision_loss)] // ratio of two large integer counts -> Mev/s
fn mev_per_s(n: u64, elapsed_ns: u64) -> f64 {
    if elapsed_ns == 0 { 0.0 } else { n as f64 * 1000.0 / elapsed_ns as f64 }
}

/// True when achieved throughput fell below `SUSTAIN_FRACTION` of target.
fn is_saturated(target_rate: u64, achieved: f64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let target = target_rate as f64;
    achieved < SUSTAIN_FRACTION * target
}

/// The first real arrival timestamp: the first event with a non-zero `ts` (the
/// recorder writes the REST depth-snapshot prelude with `ts = 0`). Same rule as
/// Benchmark 3 — basing off `ts = 0` would make the busy-spin wait decades.
fn first_arrival_ts(events: &[BookEvent]) -> u64 {
    events
        .iter()
        .map(|e| e.ts)
        .find(|&ts| ts > 0)
        .or_else(|| events.first().map(|e| e.ts))
        .unwrap_or(0)
}

/// The natural feed rate of a real corpus: events / live ts-span, events/sec.
fn natural_rate_eps(events: &[BookEvent], first_ts: u64) -> u64 {
    let Some(last) = events.last() else { return 0 };
    let span = last.ts.saturating_sub(first_ts);
    if span == 0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)]
    u64::try_from(u128::from(events.len() as u64) * 1_000_000_000 / u128::from(span)).unwrap_or(0)
}

/// The cross-thread handshake shared by the producer and every consumer of a cell:
/// a readiness barrier (consumers past warmup), the published shared clock base, and
/// a completion flag. One timeline for the whole cell so latency is measured against
/// a single clock base across cores.
#[derive(Debug)]
struct Handshake {
    /// Count of consumers that have finished warmup (the start barrier).
    ready: AtomicU64,
    /// Set once the producer has published `base_raw` and may start pushing.
    started: AtomicBool,
    /// The shared clock base (raw counter), valid once `started`.
    base_raw: AtomicU64,
    /// Set once the producer has pushed the whole corpus.
    done: AtomicBool,
}

impl Handshake {
    fn new() -> Self {
        Self {
            ready: AtomicU64::new(0),
            started: AtomicBool::new(false),
            base_raw: AtomicU64::new(0),
            done: AtomicBool::new(false),
        }
    }
}

/// The per-consumer tally returned from each consumer thread.
#[derive(Debug)]
struct ConsumerStats {
    /// End-to-end (production→consumption) latency distribution for this consumer.
    e2e: Recorder,
    deliveries: u64,
    overruns: u64,
    skipped: u64,
    pinned: bool,
}

/// The result of one (schedule, `K`, rate) cell.
#[derive(Debug)]
struct CellResult {
    /// Merged end-to-end latency across all `K` consumers (the headline distribution).
    e2e: Recorder,
    producer_mev_s: f64,
    achieved_rate_eps: f64,
    total_deliveries: u64,
    total_overruns: u64,
    total_skipped: u64,
    all_pinned: bool,
}

#[derive(Debug, Clone, Copy)]
struct BenchOpts {
    /// The producer's pinned core; consumers take `core+1 .. core+1+K`.
    core: usize,
    /// Real-arrival replay speed multiplier (1 = real time).
    speed: u64,
    /// Skip the multi-minute real-arrival headline run (quick synthetic-only sweeps).
    skip_real: bool,
}

/// One consumer: pin, warm up untimed, signal ready, wait for the shared clock base,
/// then drain — recording end-to-end latency on each cleanly delivered event and
/// counting overruns — until the producer is done and we have caught up to `n`.
fn run_consumer(
    clock: &BenchClock,
    mut c: EngineConsumer,
    core: usize,
    n: u64,
    hs: &Handshake,
) -> ConsumerStats {
    let pinned = harness::pin_to_core(core);
    // Untimed warmup on the still-empty ring (warms try_recv/unpack + predictor).
    for _ in 0..WARMUP_POLLS {
        let _ = black_box(c.poll());
    }
    // Barrier: announce readiness, then wait for the producer to publish the base.
    hs.ready.fetch_add(1, Release);
    while !hs.started.load(Acquire) {
        std::hint::spin_loop();
    }
    let base = hs.base_raw.load(Acquire);

    let mut e2e = Recorder::new();
    let (mut deliveries, mut overruns, mut skipped) = (0u64, 0u64, 0u64);
    loop {
        match c.poll() {
            Observed::Event(ev) => {
                // CO-CORRECT: completion minus the SCHEDULED arrival stamped in ts.
                let lat = clock.now_since_ns(base).saturating_sub(ev.ts);
                black_box(ev);
                e2e.record(lat);
                deliveries += 1;
            }
            Observed::Overrun { skipped: s, snapshot } => {
                black_box(snapshot);
                overruns += 1;
                skipped += s;
            }
            Observed::Idle => {
                if hs.done.load(Acquire) && c.cursor() >= n {
                    break;
                }
                std::hint::spin_loop();
            }
        }
    }
    ConsumerStats { e2e, deliveries, overruns, skipped, pinned }
}

/// Drive the producer: untimed warmup (throwaway engine), wait for all consumers to
/// be ready, publish the shared clock base, then run the paced loop stamping each
/// event's `ts` with its scheduled arrival and calling `process`. Returns the
/// producer's throughput (Mev/s) and achieved event rate (eps).
fn drive_producer(
    clock: &BenchClock,
    producer: &mut EngineProducer<BTreeBook>,
    events: &[BookEvent],
    sched: Schedule,
    consumers: usize,
    hs: &Handshake,
) -> (f64, f64) {
    // Untimed warmup of the apply/store/push path on a throwaway engine (a small
    // ring it laps itself — no consumers — purely to warm the code path, §3.4).
    {
        let (mut wp, _wh) = Engine::<BTreeBook>::new(1024);
        for ev in events {
            wp.process(black_box(ev));
        }
        black_box(wp.top_of_book());
    }

    // Barrier: do not start the timed run until every consumer is past warmup, so no
    // consumer is still warming when the first real event is pushed (which would
    // drop early events from its recorded distribution).
    while hs.ready.load(Acquire) < consumers as u64 {
        std::hint::spin_loop();
    }

    let n = events.len() as u64;
    let base = clock.raw();
    hs.base_raw.store(base, Release);
    hs.started.store(true, Release); // publish: consumers may now read `base` and record

    for (i, ev) in events.iter().enumerate() {
        let scheduled = sched.offset_ns(i, ev);
        let mut now = clock.now_since_ns(base);
        while now < scheduled {
            std::hint::spin_loop();
            now = clock.now_since_ns(base);
        }
        let mut e = *ev;
        e.ts = scheduled; // stamp the scheduled arrival — the CO-correct latency basis
        producer.process(black_box(&e));
    }
    let active = clock.now_since_ns(base);
    hs.done.store(true, Release);

    let producer_mev_s = mev_per_s(n, active);
    #[allow(clippy::cast_precision_loss)]
    let achieved_rate_eps =
        if active == 0 { f64::INFINITY } else { n as f64 * 1e9 / active as f64 };
    (producer_mev_s, achieved_rate_eps)
}

/// Run one (schedule, `K`, rate) cell: spawn `consumers` pinned draining threads PLUS
/// a pinned producer thread (scoped, so the corpus and clock are borrowed, not
/// cloned), then merge the per-consumer distributions. The producer runs in its own
/// spawned thread (never the main thread) so the main thread is never pinned and each
/// worker inherits a full affinity mask (see Benchmark 6 for why this matters).
fn run_cell(
    clock: &BenchClock,
    opts: &BenchOpts,
    events: &[BookEvent],
    sched: Schedule,
    consumers: usize,
) -> CellResult {
    let (producer, handle) = Engine::<BTreeBook>::new(RING_CAP);
    let n = events.len() as u64;
    // Shared handshake the `move` closures copy by reference (so the atomics are
    // borrowed, not moved into the first thread spawned).
    let hs = Handshake::new();
    let hs = &hs;

    thread::scope(|s| {
        // Mint every consumer at cursor 0 BEFORE the producer pushes anything.
        let cons: Vec<_> = (0..consumers)
            .map(|r| {
                let c = handle.consumer();
                let core = opts.core + 1 + r;
                s.spawn(move || run_consumer(clock, c, core, n, hs))
            })
            .collect();

        // Producer in its own pinned thread (see fn-doc on why not the main thread).
        let prod = {
            let mut producer = producer; // move the single producer into its thread
            let core = opts.core;
            let cn = consumers;
            s.spawn(move || {
                let pinned = harness::pin_to_core(core);
                let (mev, achieved) =
                    drive_producer(clock, &mut producer, events, sched, cn, hs);
                (mev, achieved, pinned)
            })
        };

        let (producer_mev_s, achieved_rate_eps, prod_pinned) =
            prod.join().expect("producer thread panicked");

        let mut e2e = Recorder::new();
        let (mut total_deliveries, mut total_overruns, mut total_skipped) = (0u64, 0u64, 0u64);
        let mut all_pinned = prod_pinned;
        for h in cons {
            let st = h.join().expect("consumer thread panicked");
            total_deliveries += st.deliveries;
            total_overruns += st.overruns;
            total_skipped += st.skipped;
            all_pinned &= st.pinned;
            e2e.merge(&st.e2e);
        }

        CellResult {
            e2e,
            producer_mev_s,
            achieved_rate_eps,
            total_deliveries,
            total_overruns,
            total_skipped,
            all_pinned,
        }
    })
}

/// One CSV row (one schedule × K × rate cell).
#[derive(Debug)]
struct Row {
    book: &'static str,
    schedule: &'static str,
    consumers: usize,
    target_rate_eps: u64,
    achieved_rate_eps: f64,
    samples: u64,
    overhead_ns: u64,
    e2e_p50: u64,
    e2e_p99: u64,
    e2e_p999: u64,
    e2e_max: u64,
    producer_mev_s: f64,
    overrun_rate: f64,
    saturated: bool,
}

impl Row {
    fn from_cell(
        schedule: &'static str,
        consumers: usize,
        target_rate_eps: u64,
        overhead_ns: u64,
        saturated: bool,
        c: &CellResult,
    ) -> Self {
        // overrun_rate = fraction of consumer-observed positions lost to overrun.
        #[allow(clippy::cast_precision_loss)]
        let overrun_rate = {
            let observed = c.total_deliveries + c.total_skipped;
            if observed == 0 { 0.0 } else { c.total_skipped as f64 / observed as f64 }
        };
        Self {
            book: BOOK,
            schedule,
            consumers,
            target_rate_eps,
            achieved_rate_eps: c.achieved_rate_eps,
            samples: c.e2e.count(),
            overhead_ns,
            e2e_p50: c.e2e.p(0.50),
            e2e_p99: c.e2e.p(0.99),
            e2e_p999: c.e2e.p(0.999),
            e2e_max: c.e2e.max(),
            producer_mev_s: c.producer_mev_s,
            overrun_rate,
            saturated,
        }
    }
}

/// Parse `--core N --speed N --no-real --out DIR`. Minimal, hand-rolled (same flag
/// vocabulary as the other benches).
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut core = 0usize;
    let mut speed = 1u64;
    let mut skip_real = false;
    let mut out = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            "--speed" => speed = it.next().and_then(|s| s.parse().ok()).unwrap_or(speed).max(1),
            "--no-real" => skip_real = true,
            "--out" => {
                if let Some(d) = it.next() {
                    out = PathBuf::from(d);
                }
            }
            _ => {}
        }
    }
    (BenchOpts { core, speed, skip_real }, out)
}

/// Logical-core count, for capping the consumer ladder (producer core + K consumers).
fn logical_cores() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Entry point: `bench e2e [--core N] [--speed N] [--no-real] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let overhead = clock.overhead_ns();
    let cores = logical_cores();
    let max_consumers = cores.saturating_sub(1).max(1);
    let ladder: Vec<usize> =
        CONSUMER_LADDER.iter().copied().filter(|&k| k <= max_consumers).collect();

    eprintln!(
        "e2e: book={BOOK} producer_core={} speed={} logical_cores={cores} K-ladder={ladder:?} \
         (capped; {} skipped) cap={RING_CAP} clock_overhead_ns={overhead}",
        opts.core,
        opts.speed,
        CONSUMER_LADDER.len() - ladder.len(),
    );

    let mut rows: Vec<Row> = Vec::new();

    // --- Real-arrival replay (speed=1): the headline real-data end-to-end latency.
    if !opts.skip_real {
        match Corpus::load(Path::new(SAMPLE.1)) {
            Ok(corpus) => {
                let events = corpus.events();
                let first_ts = first_arrival_ts(events);
                let nat = natural_rate_eps(events, first_ts);
                let sched = Schedule::RealArrival { first_ts, speed: opts.speed };
                let span_secs =
                    events.last().map_or(0, |l| l.ts.saturating_sub(first_ts) / 1_000_000_000);
                eprintln!(
                    "  real-arrival {} ({} events, ~{nat} eps natural) — ~{}s/K at speed={} ...",
                    SAMPLE.0,
                    events.len(),
                    span_secs / opts.speed.max(1),
                    opts.speed,
                );
                for &k in &ladder {
                    let cell = run_cell(&clock, &opts, events, sched, k);
                    let row = Row::from_cell("real", k, nat, overhead, false, &cell);
                    eprint_cell("real", k, nat, &row, &cell);
                    rows.push(row);
                }
            }
            Err(e) => eprintln!("warn: skip real-arrival {} ({}): {e}", SAMPLE.0, SAMPLE.1),
        }
    }

    // --- Synthetic fixed-rate sweep to saturation.
    match Corpus::load(Path::new(SYNTH.1)) {
        Ok(corpus) => {
            let events = corpus.events();
            eprintln!("  fixed-rate sweep {} ({} events):", SYNTH.0, events.len());
            for &k in &ladder {
                for &rate in &RATES {
                    let cell = run_cell(&clock, &opts, events, Schedule::FixedRate(rate), k);
                    let sat = is_saturated(rate, cell.achieved_rate_eps);
                    let row = Row::from_cell("fixed", k, rate, overhead, sat, &cell);
                    eprint_cell("fixed", k, rate, &row, &cell);
                    rows.push(row);
                }
            }
        }
        Err(e) => eprintln!("warn: skip synthetic {} ({}): {e}", SYNTH.0, SYNTH.1),
    }

    write_csv(&out_dir.join("e2e.csv"), &rows);
    print_headline(&rows);
}

/// One progress line per cell.
fn eprint_cell(schedule: &str, k: usize, rate: u64, row: &Row, cell: &CellResult) {
    eprintln!(
        "    {schedule:<5} K={k:<2} rate={rate:>11} achieved={:>11.0} | e2e p50={:>6}ns \
         p99={:>8}ns p999={:>9}ns | producer={:>6.2}Mev/s overruns={} rate={:.4}{}{}",
        cell.achieved_rate_eps,
        row.e2e_p50,
        row.e2e_p99,
        row.e2e_p999,
        cell.producer_mev_s,
        cell.total_overruns,
        row.overrun_rate,
        if row.saturated { " SAT" } else { "" },
        if cell.all_pinned { "" } else { " [PIN FAILED]" },
    );
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "book,schedule,consumers,target_rate_eps,achieved_rate_eps,samples,clock_overhead_ns,\
         e2e_p50_ns,e2e_p99_ns,e2e_p999_ns,e2e_max_ns,producer_mev_s,overrun_rate,saturated\n",
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{:.0},{},{},{},{},{},{},{:.4},{:.6},{}",
            r.book,
            r.schedule,
            r.consumers,
            r.target_rate_eps,
            r.achieved_rate_eps,
            r.samples,
            r.overhead_ns,
            r.e2e_p50,
            r.e2e_p99,
            r.e2e_p999,
            r.e2e_max,
            r.producer_mev_s,
            r.overrun_rate,
            r.saturated,
        );
    }
    std::fs::write(path, s).expect("write e2e.csv");
    eprintln!("wrote {}", path.display());
}

/// Headline: the true-sharing curve — producer throughput at the saturated
/// (free-running) synthetic rate should FALL as K rises, and the real-corpus
/// end-to-end latency per K.
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== TRUE-SHARING HEADLINE (synthetic, producer Mev/s at saturation vs K) ====");
    eprintln!("{:<3} {:>16} {:>16} {:>14}", "K", "producer_Mev/s", "achieved_eps", "overrun_rate");
    for &k in &CONSUMER_LADDER {
        // The full-tilt operating point: the highest target rate for this K (saturated).
        if let Some(r) = rows
            .iter()
            .filter(|r| r.schedule == "fixed" && r.consumers == k)
            .max_by_key(|r| r.target_rate_eps)
        {
            eprintln!(
                "{:<3} {:>16.2} {:>16.0} {:>14.4}",
                k, r.producer_mev_s, r.achieved_rate_eps, r.overrun_rate,
            );
        }
    }

    eprintln!("\n==== REAL-CORPUS END-TO-END LATENCY (btcusdt-sample, speed 1) ====");
    eprintln!("{:<3} {:>10} {:>10} {:>11} {:>11}", "K", "p50_ns", "p99_ns", "p999_ns", "max_ns");
    for r in rows.iter().filter(|r| r.schedule == "real") {
        eprintln!(
            "{:<3} {:>10} {:>10} {:>11} {:>11}",
            r.consumers, r.e2e_p50, r.e2e_p99, r.e2e_p999, r.e2e_max,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book::{Px, Qty, Side};

    /// A small deterministic corpus: clear + a symmetric ladder + a few updates.
    fn small_corpus() -> Corpus {
        let mut evs = vec![BookEvent::clear(0, 0)];
        for i in 1..=64u64 {
            let px = i64::try_from(i).unwrap();
            evs.push(BookEvent::level(i, i * 1000, Side::Bid, Px(1_000 - px), Qty(10 + px)));
            evs.push(BookEvent::level(i, i * 1000, Side::Ask, Px(1_000 + px), Qty(10 + px)));
        }
        Corpus::from_events(evs)
    }

    /// Real-arrival offsets follow event timestamps; fixed-rate offsets follow `i`.
    #[test]
    fn schedule_offsets_are_correct() {
        let ev = BookEvent::level(0, 5_000, Side::Bid, Px(1), Qty(1));
        // Fixed 1e9 eps => 1 ns spacing => offset i == i ns.
        assert_eq!(Schedule::FixedRate(1_000_000_000).offset_ns(7, &ev), 7);
        // Real arrival, speed 1: offset == ts - first_ts.
        let real = Schedule::RealArrival { first_ts: 1_000, speed: 1 };
        assert_eq!(real.offset_ns(0, &ev), 4_000);
        // Speed 2 compresses time by half.
        let fast = Schedule::RealArrival { first_ts: 1_000, speed: 2 };
        assert_eq!(fast.offset_ns(0, &ev), 2_000);
    }

    /// Saturation rule: achieved far below target is saturated; near target is not.
    #[test]
    fn saturation_rule() {
        assert!(is_saturated(1_000_000, 100_000.0), "10% of target must be saturated");
        assert!(!is_saturated(1_000_000, 999_000.0), "99.9% of target is sustained");
    }

    /// A full cell runs end-to-end: the pipeline delivers events and records their
    /// end-to-end latency under a modest rate. (ns values are not asserted — debug
    /// clock noise; latency claims live in e2e.md, measured release.)
    #[test]
    fn cell_runs_and_records() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let opts = BenchOpts { core: 0, speed: 1, skip_real: true };
        let cell = run_cell(&clock, &opts, corpus.events(), Schedule::FixedRate(1_000_000), 2);
        assert!(cell.total_deliveries > 0, "consumers must deliver some events");
        assert_eq!(cell.e2e.count(), cell.total_deliveries, "every delivery is recorded");
        // Two consumers each see at most the whole corpus, none skipped on a roomy ring.
        assert!(
            cell.total_deliveries <= 2 * corpus.events().len() as u64,
            "cannot deliver more than 2x the corpus",
        );
    }

    /// A high rate against a tiny ring forces overruns that resync from the seqlock —
    /// the composition behaviour, exercised through the benchmark path.
    #[test]
    fn high_rate_produces_overruns() {
        let clock = BenchClock::new();
        // Build a long corpus so a free-running producer laps the consumer.
        let mut evs = vec![BookEvent::clear(0, 0)];
        for i in 1..=20_000u64 {
            let px = i64::try_from(i % 128 + 1).unwrap();
            let q = i64::try_from(1 + i % 7).unwrap();
            evs.push(BookEvent::level(i, i, Side::Bid, Px(1_000 - px), Qty(q)));
        }
        let corpus = Corpus::from_events(evs);
        let opts = BenchOpts { core: 0, speed: 1, skip_real: true };
        // 1e9 eps target => producer free-runs; the single consumer is lapped.
        let cell = run_cell(&clock, &opts, corpus.events(), Schedule::FixedRate(1_000_000_000), 1);
        assert!(cell.total_overruns > 0, "a saturated tiny ring must overrun the consumer");
        assert!(cell.total_skipped > 0, "overrun must report skipped positions");
    }
}

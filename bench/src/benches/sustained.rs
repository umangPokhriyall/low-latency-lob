//! Benchmark 3 — sustained-feed, coordinated-omission-correct response time (§7).
//!
//! This is the open-loop study and the one place CO-correctness (§3.1) is
//! demonstrated. Events arrive on a **schedule**; the response latency of event
//! `i` is `completion_i - scheduled_i` — measured against when it *should* have
//! been served, never `completion_i - apply_start_i`. When the book falls behind,
//! `completion > scheduled` by the accumulated lag and the tail captures it. The
//! naive `completion - apply_start` form would measure service time and mislabel
//! it response time, hiding the very backlog this benchmark exists to expose (the
//! §7 CO-proof unit test pins this distinction).
//!
//! Two schedules, both required (§7):
//! - **Real-arrival replay** of the BTCUSDT sample at `speed = 1` (its real `ts`):
//!   arrivals are ms-scale, `apply` is ns-scale, so every impl keeps up
//!   comfortably — the "it tracks a real feed" baseline.
//! - **Synthetic fixed-rate sweep** over the three profile corpora at increasing
//!   `rate_eps` until saturation — finds each impl's max sustainable rate and the
//!   response-time tail blow-up past it.
//!
//! Pacing is a busy-spin (`spin_loop`), never a sleep: at these timescales a
//! syscall sleep's own latency would dwarf the schedule. This is a paced loop,
//! not service time — it measures response time under load, not the bare op.

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use book::{BTreeBook, BookEvent, OrderBook, RevVecBook, SortedVecBook};
use feed::corpus::Corpus;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// The synthetic profile corpora swept (relative to this crate's manifest dir).
const PROFILES: [(&str, &str); 3] = [
    ("steady", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/steady-s1-100k.mdf")),
    ("burst", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/burst-s1-100k.mdf")),
    ("flashcrash", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/flashcrash-s1-100k.mdf")),
];

/// The real-arrival corpus replayed at `speed = 1`.
const SAMPLE: (&str, &str) =
    ("btcusdt-sample", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/btcusdt-sample.mdf"));

/// The fixed-rate ladder (§7), events/sec. Climbs past every impl's service rate
/// (throughput.csv shows ~45–77 Mev/s) so saturation and the tail are captured.
const RATES: [u64; 9] = [
    100_000,
    1_000_000,
    2_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    50_000_000,
    100_000_000,
    200_000_000,
];

/// A rate is *saturated* when the loop can no longer reach scheduled times, i.e.
/// achieved throughput plateaus below target (§7). 0.90 leaves headroom for spin
/// granularity while still flagging a genuine fall-behind.
const SUSTAIN_FRACTION: f64 = 0.90;

/// The arrival schedule for one run: a fixed synthetic rate (ignores event `ts`)
/// or real-arrival replay (offset = `(ts - first_ts) / speed`).
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
            #[allow(clippy::cast_possible_truncation)]
            Schedule::FixedRate(rate) => {
                (i as u128 * 1_000_000_000u128 / u128::from(rate)) as u64
            }
            Schedule::RealArrival { first_ts, speed } => ev.ts.saturating_sub(first_ts) / speed,
        }
    }
}

/// The outcome of one CO-correct run.
#[derive(Debug)]
struct RunResult {
    rec: Recorder,
    achieved_rate_eps: f64,
    samples: u64,
}

/// The CO-correct busy-paced loop (§7), monomorphized over the concrete book `B`
/// so `apply` inlines — there is **no `dyn OrderBook`** and no per-event closure
/// indirection in the measured loop. Records `completion - scheduled` for every
/// event. An untimed full replay warms the caches first (§3.4); the timed loop
/// then runs on a fresh book.
fn co_run<B: OrderBook>(clock: &BenchClock, events: &[BookEvent], sched: Schedule) -> RunResult {
    // Untimed warmup: one unpaced full replay to warm I-cache/D-cache/predictor.
    {
        let mut wb = B::default();
        for ev in events {
            black_box(&mut wb).apply(black_box(ev));
        }
        black_box(wb.best_bid());
    }

    let mut book = B::default();
    let mut rec = Recorder::new();
    let base = clock.raw();
    let mut last_done = 0u64;
    for (i, ev) in events.iter().enumerate() {
        let scheduled = sched.offset_ns(i, ev);
        let mut now = clock.now_since_ns(base);
        while now < scheduled {
            std::hint::spin_loop();
            now = clock.now_since_ns(base);
        }
        black_box(&mut book).apply(black_box(ev));
        let done = clock.now_since_ns(base);
        rec.record(done.saturating_sub(scheduled)); // CO-CORRECT: vs schedule
        last_done = done;
    }
    black_box(book.best_bid());

    let samples = rec.count();
    #[allow(clippy::cast_precision_loss)]
    let achieved_rate_eps = if last_done == 0 {
        f64::INFINITY
    } else {
        events.len() as f64 * 1e9 / last_done as f64
    };
    RunResult { rec, achieved_rate_eps, samples }
}

/// Monomorphized impl dispatch (no `dyn`). Phase 5 adds `"flat"`.
fn run_impl(name: &str, clock: &BenchClock, events: &[BookEvent], sched: Schedule) -> RunResult {
    match harness::for_impl(name) {
        Some("btree") => co_run::<BTreeBook>(clock, events, sched),
        Some("sorted") => co_run::<SortedVecBook>(clock, events, sched),
        Some("rev") => co_run::<RevVecBook>(clock, events, sched),
        _ => panic!("unknown impl `{name}` (expected one of {:?})", harness::IMPLS),
    }
}

/// True when achieved throughput fell below `SUSTAIN_FRACTION` of target — the
/// loop could not keep the schedule (§7 saturation rule).
fn is_saturated(target_rate: u64, achieved: f64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let target = target_rate as f64;
    achieved < SUSTAIN_FRACTION * target
}

/// One CSV row.
#[derive(Debug)]
struct Row {
    impl_name: &'static str,
    corpus: &'static str,
    schedule: &'static str,
    target_rate_eps: u64,
    achieved_rate_eps: f64,
    samples: u64,
    overhead_ns: u64,
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    saturated: bool,
}

#[derive(Debug, Clone, Copy)]
struct BenchOpts {
    core: usize,
    speed: u64,
    skip_real: bool,
}

/// Parse `--core N --speed N --no-real --out DIR`. Minimal, hand-rolled.
/// `--no-real` skips the ~45 s/impl real-arrival replay (useful for quick sweeps);
/// the committed numbers are produced with it on (the default).
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut core = 0usize;
    let mut speed = 1u64;
    let mut skip_real = false;
    let mut out = default_results_dir();
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

fn default_results_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"))
}

/// Build a [`Row`] from a finished run.
fn row_of(
    impl_name: &'static str,
    corpus: &'static str,
    schedule: &'static str,
    target_rate_eps: u64,
    overhead_ns: u64,
    saturated: bool,
    r: &RunResult,
) -> Row {
    Row {
        impl_name,
        corpus,
        schedule,
        target_rate_eps,
        achieved_rate_eps: r.achieved_rate_eps,
        samples: r.samples,
        overhead_ns,
        p50: r.rec.p(0.50),
        p99: r.rec.p(0.99),
        p999: r.rec.p(0.999),
        max: r.rec.max(),
        saturated,
    }
}

/// The first real arrival timestamp: the first event with a non-zero `ts`.
///
/// The recorder writes the initial REST depth-snapshot ladder with `ts = 0` (it
/// carries no exchange event time), then live websocket events with real
/// epoch-ns `ts`. The real-arrival schedule MUST base off the first *live*
/// arrival: with `first_ts = 0`, the ts=0 → first-live jump (decades of epoch
/// ns) makes the busy-spin wait ~56 years and never return. Prelude events, whose
/// `ts` is below `first_ts`, then schedule at offset 0 (`saturating_sub`) and are
/// applied instantly at the start of the paced loop.
fn first_arrival_ts(events: &[BookEvent]) -> u64 {
    events
        .iter()
        .map(|e| e.ts)
        .find(|&ts| ts > 0)
        .or_else(|| events.first().map(|e| e.ts))
        .unwrap_or(0)
}

/// The natural feed rate of a real corpus: events / (live ts-span) in events/sec,
/// where the span runs from the first live arrival to the last event.
fn natural_rate_eps(events: &[BookEvent], first_ts: u64) -> u64 {
    let Some(last) = events.last() else {
        return 0;
    };
    let span = last.ts.saturating_sub(first_ts);
    if span == 0 {
        return 0;
    }
    u64::try_from(u128::from(events.len() as u64) * 1_000_000_000 / u128::from(span)).unwrap_or(0)
}

/// Entry point for `bench sustained [--core N] [--speed N] [--no-real] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(opts.core);
    let overhead = clock.overhead_ns();
    eprintln!(
        "sustained: core={} (pinned={}) speed={} clock_overhead_ns={}",
        opts.core, pinned, opts.speed, overhead
    );

    let mut rows: Vec<Row> = Vec::new();

    // --- Real-arrival replay (speed=1): the "keeps up with a real feed" baseline.
    if !opts.skip_real {
        match Corpus::load(Path::new(SAMPLE.1)) {
            Ok(corpus) => {
                let events = corpus.events();
                let first_ts = first_arrival_ts(events);
                let nat = natural_rate_eps(events, first_ts);
                let sched = Schedule::RealArrival { first_ts, speed: opts.speed };
                let span_secs = events
                    .last()
                    .map_or(0, |l| l.ts.saturating_sub(first_ts) / 1_000_000_000);
                eprintln!(
                    "  real-arrival {} ({} events, ~{} eps natural) — ~{}s/impl at speed=1 ...",
                    SAMPLE.0,
                    events.len(),
                    nat,
                    span_secs / opts.speed.max(1),
                );
                for &impl_name in &harness::IMPLS {
                    let r = run_impl(impl_name, &clock, events, sched);
                    eprintln!(
                        "    {:<6} real  achieved={:>10.0} eps  resp p50={:>5}ns p99={:>6}ns max={}ns",
                        impl_name,
                        r.achieved_rate_eps,
                        r.rec.p(0.50),
                        r.rec.p(0.99),
                        r.rec.max(),
                    );
                    rows.push(row_of(impl_name, SAMPLE.0, "real", nat, overhead, false, &r));
                }
            }
            Err(e) => eprintln!("warn: skip real-arrival {} ({}): {e}", SAMPLE.0, SAMPLE.1),
        }
    }

    // --- Synthetic fixed-rate sweep to saturation, per profile.
    for &(corpus_tag, path) in &PROFILES {
        let corpus = match Corpus::load(Path::new(path)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warn: skip profile {corpus_tag} ({path}): {e}");
                continue;
            }
        };
        let events = corpus.events();
        eprintln!("  fixed-rate sweep {corpus_tag} ({} events):", events.len());
        for &impl_name in &harness::IMPLS {
            for &rate in &RATES {
                let r = run_impl(impl_name, &clock, events, Schedule::FixedRate(rate));
                let sat = is_saturated(rate, r.achieved_rate_eps);
                eprintln!(
                    "    {:<6} rate={:>11} achieved={:>11.0}  p50={:>5}ns p99={:>9}ns {}",
                    impl_name,
                    rate,
                    r.achieved_rate_eps,
                    r.rec.p(0.50),
                    r.rec.p(0.99),
                    if sat { "SATURATED" } else { "ok" },
                );
                rows.push(row_of(impl_name, corpus_tag, "fixed", rate, overhead, sat, &r));
            }
        }
    }

    write_csv(&out_dir.join("sustained.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "impl,corpus,schedule,target_rate_eps,achieved_rate_eps,samples,clock_overhead_ns,\
         resp_p50_ns,resp_p99_ns,resp_p999_ns,resp_max_ns,saturated\n",
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{:.0},{},{},{},{},{},{},{}",
            r.impl_name,
            r.corpus,
            r.schedule,
            r.target_rate_eps,
            r.achieved_rate_eps,
            r.samples,
            r.overhead_ns,
            r.p50,
            r.p99,
            r.p999,
            r.max,
            r.saturated,
        );
    }
    std::fs::write(path, s).expect("write sustained.csv");
    eprintln!("wrote {}", path.display());
}

/// The max sustainable rate for `(impl, corpus)`: the highest fixed target rate
/// whose run was not saturated (achieved ≥ 90% of target with a bounded tail).
fn max_sustainable(rows: &[Row], impl_name: &str, corpus: &str) -> Option<u64> {
    rows.iter()
        .filter(|r| r.schedule == "fixed" && r.impl_name == impl_name && r.corpus == corpus && !r.saturated)
        .map(|r| r.target_rate_eps)
        .max()
}

#[allow(clippy::cast_precision_loss)]
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== MAX SUSTAINABLE RATE (Mev/s, bounded-p99 fixed-rate) ====");
    eprint!("{:<7}", "impl");
    for (tag, _) in PROFILES {
        eprint!(" {tag:>12}");
    }
    eprintln!();
    for &impl_name in &harness::IMPLS {
        eprint!("{impl_name:<7}");
        for (tag, _) in PROFILES {
            match max_sustainable(rows, impl_name, tag) {
                Some(rate) => eprint!(" {:>12.1}", rate as f64 / 1e6),
                None => eprint!(" {:>12}", "-"),
            }
        }
        eprintln!();
    }

    // The tail blow-up: p99 at the last sustained rate vs the first saturated one.
    eprintln!("\n==== TAIL BLOW-UP AT SATURATION (steady, resp_p99_ns) ====");
    for &impl_name in &harness::IMPLS {
        let sustained = rows
            .iter()
            .filter(|r| r.corpus == "steady" && r.impl_name == impl_name && r.schedule == "fixed" && !r.saturated)
            .max_by_key(|r| r.target_rate_eps);
        let saturated = rows
            .iter()
            .filter(|r| r.corpus == "steady" && r.impl_name == impl_name && r.schedule == "fixed" && r.saturated)
            .min_by_key(|r| r.target_rate_eps);
        match (sustained, saturated) {
            (Some(s), Some(b)) => eprintln!(
                "{:<7} sustained {:>11} eps: p99={:>6}ns  ->  saturated {:>11} eps: p99={}ns",
                impl_name, s.target_rate_eps, s.p99, b.target_rate_eps, b.p99,
            ),
            _ => eprintln!("{impl_name:<7} (no clean sustained/saturated boundary in the swept ladder)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book::{BookEvent, Px, Qty, Side};

    /// Which subtraction the CO loop uses — the thing under test.
    #[derive(Clone, Copy)]
    enum RecordMode {
        /// CO-correct: `completion - scheduled`.
        Scheduled,
        /// The naive trap: `completion - apply_start` (hides backlog).
        ApplyStart,
    }

    /// A standalone paced loop with a deliberately slow synthetic op (busy-spin
    /// for `op_ns`), returning the per-event recorded latencies under `mode`. It
    /// mirrors [`co_run`]'s schedule/record skeleton but lets the test pick the
    /// (wrong vs right) subtraction and inject lag deterministically.
    fn paced_run(
        clock: &BenchClock,
        n: usize,
        interval_ns: u64,
        op_ns: u64,
        mode: RecordMode,
    ) -> Vec<u64> {
        let base = clock.raw();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let scheduled = i as u64 * interval_ns;
            let mut now = clock.now_since_ns(base);
            while now < scheduled {
                std::hint::spin_loop();
                now = clock.now_since_ns(base);
            }
            let apply_start = clock.now_since_ns(base);
            // Slow synthetic op: busy-spin ~op_ns so the consumer cannot keep up.
            let deadline = apply_start + op_ns;
            let mut t = apply_start;
            while t < deadline {
                std::hint::spin_loop();
                t = clock.now_since_ns(base);
            }
            let done = clock.now_since_ns(base);
            let lat = match mode {
                RecordMode::Scheduled => done.saturating_sub(scheduled),
                RecordMode::ApplyStart => done.saturating_sub(apply_start),
            };
            out.push(lat);
        }
        out
    }

    /// The §7 CO-proof. With `op_ns > interval_ns` the consumer falls behind by
    /// `op_ns - interval_ns` each event. The CO-correct `completion - scheduled`
    /// recording must show that lag **accumulating** (late events far worse than
    /// early ones); the naive `completion - apply_start` recording must NOT — it
    /// stays ~`op_ns`, hiding the backlog. That is exactly the bug this loop
    /// guards against, so the wrong subtraction fails the accumulation assertion.
    #[test]
    fn co_correct_records_accumulating_lag() {
        let clock = BenchClock::new();
        let n = 100;
        let interval_ns = 2_000; // 2 µs schedule spacing (500k eps)
        let op_ns = 40_000; // 40 µs op — 20× the interval, so lag piles up

        let sched = paced_run(&clock, n, interval_ns, op_ns, RecordMode::Scheduled);
        let astart = paced_run(&clock, n, interval_ns, op_ns, RecordMode::ApplyStart);

        // CO-correct: late events carry the accumulated backlog — far worse than
        // early ones, and on the order of i*(op-interval) by the end.
        let early = sched[5];
        let late = sched[n - 5];
        assert!(
            late > early * 5,
            "CO-correct latency must accumulate lag: late={late}ns early={early}ns"
        );
        let expected_tail = (n as u64 - 5) * (op_ns - interval_ns);
        assert!(
            late > expected_tail / 2,
            "tail {late}ns should approach i*(op-interval) ≈ {expected_tail}ns"
        );

        // The naive apply-start form stays ~op_ns and never reveals the backlog.
        let a_late = astart[n - 5];
        assert!(
            a_late < op_ns * 3,
            "apply-start latency must stay ~op_time (it hides lag): {a_late}ns"
        );
        // Hence the wrong loop fails to capture what the CO-correct one does.
        assert!(
            late > a_late * 5,
            "completion-apply_start ({a_late}ns) must under-report the lag that \
             completion-scheduled ({late}ns) captures"
        );
    }

    fn small_corpus() -> Corpus {
        let mut evs = vec![BookEvent::clear(0, 0)];
        for i in 1..=32u64 {
            let px = i64::try_from(i).unwrap();
            evs.push(BookEvent::level(i, i * 1000, Side::Bid, Px(1_000 - px), Qty(10 + px)));
            evs.push(BookEvent::level(i, i * 1000, Side::Ask, Px(1_000 + px), Qty(10 + px)));
        }
        Corpus::from_events(evs)
    }

    /// A modest fixed rate is kept (not saturated); an absurd one (1 ns spacing,
    /// far below the per-event apply cost) saturates — the saturation rule works.
    #[test]
    fn low_rate_sustains_high_rate_saturates() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let evs = corpus.events();

        let low = co_run::<RevVecBook>(&clock, evs, Schedule::FixedRate(1_000_000));
        assert!(
            !is_saturated(1_000_000, low.achieved_rate_eps),
            "1e6 eps should be sustainable: achieved {:.0}",
            low.achieved_rate_eps
        );

        let high = co_run::<RevVecBook>(&clock, evs, Schedule::FixedRate(1_000_000_000));
        assert!(
            is_saturated(1_000_000_000, high.achieved_rate_eps),
            "1e9 eps must saturate: achieved {:.0}",
            high.achieved_rate_eps
        );
        assert_eq!(low.samples, evs.len() as u64);
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

    /// The real-arrival base must skip the `ts=0` REST-snapshot prelude and lock
    /// onto the first live timestamp; prelude events then schedule at offset 0.
    /// (Guards the bug where `first_ts=0` made the busy-spin wait ~decades.)
    #[test]
    fn first_arrival_skips_zero_ts_prelude() {
        let evs = vec![
            BookEvent::clear(0, 0),
            BookEvent::level(1, 0, Side::Bid, Px(1), Qty(1)), // snapshot, ts=0
            BookEvent::level(2, 1_700_000_000_000_000_000, Side::Bid, Px(2), Qty(1)), // first live
            BookEvent::level(3, 1_700_000_000_500_000_000, Side::Ask, Px(3), Qty(1)),
        ];
        let first = first_arrival_ts(&evs);
        assert_eq!(first, 1_700_000_000_000_000_000);
        let sched = Schedule::RealArrival { first_ts: first, speed: 1 };
        // Prelude (ts < first_ts) collapses to offset 0; live events are relative.
        assert_eq!(sched.offset_ns(0, &evs[0]), 0);
        assert_eq!(sched.offset_ns(1, &evs[1]), 0);
        assert_eq!(sched.offset_ns(2, &evs[2]), 0);
        assert_eq!(sched.offset_ns(3, &evs[3]), 500_000_000);
        // All-zero ts falls back to the first event's ts.
        let zero = vec![BookEvent::clear(0, 0)];
        assert_eq!(first_arrival_ts(&zero), 0);
    }

    #[test]
    #[should_panic(expected = "unknown impl")]
    fn run_impl_rejects_unknown() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let _ = run_impl("flat", &clock, corpus.events(), Schedule::FixedRate(1_000_000));
    }
}

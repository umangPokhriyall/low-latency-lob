//! Benchmark 4 — end-to-end replay throughput (§8).
//!
//! The simple headline: replay a whole corpus as fast as possible (no pacing),
//! per impl, per corpus. For each cell we warm up with one untimed full replay,
//! then time the full `for ev in corpus { apply(ev) }` loop over the resident
//! `&[BookEvent]` slice ≥31 times and report the **median** total plus the
//! derived rate (events/s) and per-event cost.
//!
//! This is **service time**: there is no arrival process and therefore no
//! coordinated omission (§3.1) — it measures how fast the book chews through a
//! resident slice, not response time under a schedule. Each timed run starts
//! from a fresh `B::default()` (built untimed, before `t0`) so every run does the
//! same work (cold replay: inserts then updates), rather than later runs hitting
//! an already-populated book. Every applied event and the post-replay state are
//! `black_box`ed to defeat dead-code elision (§3.3).

use crate::clock::BenchClock;
use crate::harness;
use book::{BTreeBook, BookEvent, FlatBook, OrderBook, RevVecBook, SortedVecBook};
use feed::corpus::Corpus;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// The committed corpora replayed, by `(tag, path)`. Paths are relative to this
/// crate's manifest dir so the bench is runnable from anywhere. The three
/// synthetic profiles (100k events each) plus the real BTCUSDT sample.
const CORPORA: [(&str, &str); 4] = [
    ("steady", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/steady-s1-100k.mdf")),
    ("burst", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/burst-s1-100k.mdf")),
    ("flashcrash", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/flashcrash-s1-100k.mdf")),
    ("btcusdt-sample", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/btcusdt-sample.mdf")),
];

/// Time one full replay of `events` into a fresh `B`, repeated `runs` times;
/// return the **median** total-ns. The fresh book is built before `t0` (untimed)
/// so the measured span is purely the apply loop over the resident slice.
fn replay_median<B: OrderBook>(clock: &BenchClock, events: &[BookEvent], runs: usize) -> u64 {
    // Untimed warmup: one full replay to warm I-cache/D-cache/branch predictor.
    {
        let mut b = B::default();
        for ev in events {
            black_box(&mut b).apply(black_box(ev));
        }
        black_box(b.best_bid());
        black_box(b.last_trade());
    }

    let mut totals = Vec::with_capacity(runs);
    for _ in 0..runs {
        let mut b = B::default();
        let t0 = clock.raw();
        for ev in events {
            black_box(&mut b).apply(black_box(ev));
        }
        let t1 = clock.raw();
        black_box(b.best_bid());
        black_box(b.last_trade());
        totals.push(clock.delta_ns(t0, t1));
    }
    totals.sort_unstable();
    totals[totals.len() / 2]
}

/// The monomorphized impl dispatch (no `dyn OrderBook`).
fn run_impl(name: &str, clock: &BenchClock, events: &[BookEvent], runs: usize) -> u64 {
    match harness::for_impl(name) {
        Some("btree") => replay_median::<BTreeBook>(clock, events, runs),
        Some("sorted") => replay_median::<SortedVecBook>(clock, events, runs),
        Some("rev") => replay_median::<RevVecBook>(clock, events, runs),
        Some("flat") => replay_median::<FlatBook>(clock, events, runs),
        _ => panic!("unknown impl `{name}` (expected one of {:?})", harness::IMPLS),
    }
}

/// One CSV row.
#[derive(Debug)]
struct Row {
    impl_name: &'static str,
    corpus: &'static str,
    events: usize,
    runs: usize,
    total_ns_median: u64,
    events_per_sec: f64,
    ns_per_event: f64,
}

#[derive(Debug, Clone, Copy)]
struct BenchOpts {
    runs: usize,
    core: usize,
}

/// Parse `--runs N --core N --out DIR`. Minimal, hand-rolled.
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut runs = 31usize;
    let mut core = 0usize;
    let mut out = default_results_dir();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => runs = it.next().and_then(|s| s.parse().ok()).unwrap_or(runs),
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            "--out" => {
                if let Some(d) = it.next() {
                    out = PathBuf::from(d);
                }
            }
            _ => {}
        }
    }
    (BenchOpts { runs: runs.max(1), core }, out)
}

fn default_results_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"))
}

/// Entry point for `bench throughput [--runs N] [--core N] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(opts.core);
    eprintln!(
        "throughput: runs={} core={} (pinned={}) clock_overhead_ns={}",
        opts.runs,
        opts.core,
        pinned,
        clock.overhead_ns()
    );

    let mut rows: Vec<Row> = Vec::new();
    for &(corpus_tag, path) in &CORPORA {
        let corpus = match Corpus::load(Path::new(path)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warn: skip corpus {corpus_tag} ({path}): {e}");
                continue;
            }
        };
        let events = corpus.events();
        for &impl_name in &harness::IMPLS {
            let total = run_impl(impl_name, &clock, events, opts.runs);
            let n = events.len();
            #[allow(clippy::cast_precision_loss)]
            let (eps, npe) = if total == 0 {
                (f64::INFINITY, 0.0)
            } else {
                (
                    n as f64 * 1e9 / total as f64,
                    total as f64 / n as f64,
                )
            };
            eprintln!(
                "  {:<14} {:<6} {} events: median {} ns -> {:.2} Mev/s ({:.2} ns/ev)",
                corpus_tag,
                impl_name,
                n,
                total,
                eps / 1e6,
                npe,
            );
            rows.push(Row {
                impl_name,
                corpus: corpus_tag,
                events: n,
                runs: opts.runs,
                total_ns_median: total,
                events_per_sec: eps,
                ns_per_event: npe,
            });
        }
    }

    write_csv(&out_dir.join("throughput.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("impl,corpus,events,runs,total_ns_median,events_per_sec,ns_per_event\n");
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{:.0},{:.2}",
            r.impl_name,
            r.corpus,
            r.events,
            r.runs,
            r.total_ns_median,
            r.events_per_sec,
            r.ns_per_event,
        );
    }
    std::fs::write(path, s).expect("write throughput.csv");
    eprintln!("wrote {}", path.display());
}

/// Headline: events/s per impl on the `steady` profile (the representative load).
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== THROUGHPUT HEADLINE (steady corpus, Mev/s) ====");
    for &impl_name in &harness::IMPLS {
        if let Some(r) = rows
            .iter()
            .find(|r| r.corpus == "steady" && r.impl_name == impl_name)
        {
            eprintln!(
                "{:<7} {:.2} Mev/s ({:.2} ns/ev, median over {} runs)",
                impl_name,
                r.events_per_sec / 1e6,
                r.ns_per_event,
                r.runs,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book::{BookEvent, Px, Qty, Side};

    /// A small in-memory corpus: a clear, then a ladder of inserts and updates.
    fn small_corpus() -> Corpus {
        let mut evs = vec![BookEvent::clear(0, 0)];
        for i in 1..=64u64 {
            let px = i64::try_from(i).unwrap();
            evs.push(BookEvent::level(i, 0, Side::Bid, Px(1_000 - px), Qty(10 + px)));
            evs.push(BookEvent::level(i, 0, Side::Ask, Px(1_000 + px), Qty(10 + px)));
        }
        // A few updates and a trade so apply exercises every arm.
        evs.push(BookEvent::level(200, 0, Side::Bid, Px(999), Qty(42)));
        evs.push(BookEvent::trade(201, 0, Side::Ask, Px(1_001), Qty(3)));
        Corpus::from_events(evs)
    }

    /// The median replay total is positive (real work, not elided) and the
    /// derived rate is self-consistent (events / `total_ns` ≈ `events_per_sec`).
    #[test]
    fn replay_median_is_positive_and_consistent() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let events = corpus.events();
        let total = replay_median::<RevVecBook>(&clock, events, 9);
        assert!(total > 0, "median replay total must be positive (elision?)");
        #[allow(clippy::cast_precision_loss)]
        let eps = events.len() as f64 * 1e9 / total as f64;
        // Sanity: a 130-event replay is far under a second.
        assert!(eps > 1e6, "implausibly slow replay: {eps} ev/s");
    }

    /// All three impls replay the same corpus and each yields a positive median.
    #[test]
    fn all_impls_replay() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let events = corpus.events();
        for &impl_name in &harness::IMPLS {
            let total = run_impl(impl_name, &clock, events, 5);
            assert!(total > 0, "{impl_name} produced a zero median replay total");
        }
    }

    #[test]
    #[should_panic(expected = "unknown impl")]
    fn run_impl_rejects_unknown() {
        let clock = BenchClock::new();
        let corpus = small_corpus();
        let _ = run_impl("nope", &clock, corpus.events(), 1);
    }
}

//! Benchmark 2 — read-path cost vs depth (§6).
//!
//! The read path is what Phase 8's seqlock snapshot will hammer. For each
//! `impl × depth` we time, individually, ≥1M calls of three reads and record
//! their interior latency distributions:
//!
//! - `best_bid` — single best-level access. O(1) on a Vec (index the end/front),
//!   O(log n) on a `BTreeMap` (descend to the max key). This is H3's "`BTree`
//!   pays a standing best-access tax".
//! - `top_n_8` — copy the best 8 levels into a caller-provided buffer.
//! - `top_n_full` — copy the whole `depth`-level ladder (the full snapshot cost).
//!
//! This is **service time**: the read does not mutate and there is no arrival
//! process, so there is no coordinated omission (§3.1) — it measures the
//! operation itself. Every measured op `black_box`es the book and the returned
//! value (and the destination buffer) to defeat dead-code elision (§3.3). The
//! `top_n` buffers are allocated once at setup, never inside the timed loop.

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use crate::workload::build_at_depth;
use book::{BTreeBook, FlatBook, OrderBook, Px, Qty, RevVecBook, Side, SortedVecBook};
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// Mid price the ladder is built around (same as Benchmark 1).
const MID: Px = Px(1_000_000);

/// The depth ladder (§6: "same ladder" as Benchmark 1). Log-spaced.
const DEPTHS: [usize; 12] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048];

/// Fixed top-of-book window for the `top_n_8` read.
const TOP_N_8: usize = 8;

/// Per-cell measurement context.
#[derive(Debug)]
struct Ctx<'a> {
    clock: &'a BenchClock,
    mid: Px,
    depth: usize,
    samples: u64,
    warmup: u64,
}

/// The three recorders produced for one `impl × depth` cell.
#[derive(Debug)]
struct CellResult {
    best_bid: Recorder,
    top_n_8: Recorder,
    top_n_full: Recorder,
}

/// `best_bid`: time a single best-level access. `black_box` the book and the
/// returned `Option<(Px, Qty)>` so the optimizer cannot hoist or delete the call.
fn measure_best_bid<B: OrderBook>(ctx: &Ctx<'_>, book: &B) -> Recorder {
    harness::warmup(ctx.warmup, || {
        black_box(black_box(book).best_bid());
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let t0 = ctx.clock.raw();
        let r = black_box(book).best_bid();
        let t1 = ctx.clock.raw();
        black_box(r);
        rec.record(ctx.clock.delta_ns(t0, t1));
    }
    rec
}

/// `top_n`: time copying the best `out.len()` bid levels into the caller's
/// buffer. The buffer is allocated by the caller (untimed); the loop only writes
/// into it. `black_box` the book, the returned count, and the destination.
fn measure_top_n<B: OrderBook>(ctx: &Ctx<'_>, book: &B, out: &mut [(Px, Qty)]) -> Recorder {
    harness::warmup(ctx.warmup, || {
        black_box(black_box(book).top_n(Side::Bid, &mut *out));
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let t0 = ctx.clock.raw();
        let n = black_box(book).top_n(Side::Bid, &mut *out);
        let t1 = ctx.clock.raw();
        black_box(n);
        black_box(&*out);
        rec.record(ctx.clock.delta_ns(t0, t1));
    }
    rec
}

/// Run all three reads for one cell, monomorphized over the concrete book `B`.
/// The book does not mutate, so a single build serves all three reads; the two
/// `top_n` buffers are allocated here (setup), never inside a timed loop.
fn read_cell<B: OrderBook>(ctx: &Ctx<'_>) -> CellResult {
    let book = build_at_depth::<B>(ctx.mid, ctx.depth);
    let best_bid = measure_best_bid(ctx, &book);
    let mut buf8 = vec![(Px(0), Qty(0)); TOP_N_8];
    let top_n_8 = measure_top_n(ctx, &book, &mut buf8);
    let mut buf_full = vec![(Px(0), Qty(0)); ctx.depth];
    let top_n_full = measure_top_n(ctx, &book, &mut buf_full);
    CellResult { best_bid, top_n_8, top_n_full }
}

/// The monomorphized impl dispatch (no `dyn OrderBook`).
fn run_impl(name: &str, ctx: &Ctx<'_>) -> CellResult {
    match harness::for_impl(name) {
        Some("btree") => read_cell::<BTreeBook>(ctx),
        Some("sorted") => read_cell::<SortedVecBook>(ctx),
        Some("rev") => read_cell::<RevVecBook>(ctx),
        Some("flat") => read_cell::<FlatBook>(ctx),
        _ => panic!("unknown impl `{name}` (expected one of {:?})", harness::IMPLS),
    }
}

/// One CSV row.
#[derive(Debug)]
struct Row {
    impl_name: &'static str,
    depth: usize,
    op: &'static str,
    samples: u64,
    overhead_ns: u64,
    rec_summary: Summary,
}

#[derive(Debug, Clone, Copy)]
struct Summary {
    mean: f64,
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
}

impl Summary {
    fn of(rec: &Recorder) -> Self {
        Self {
            mean: rec.mean(),
            p50: rec.p(0.50),
            p99: rec.p(0.99),
            p999: rec.p(0.999),
            max: rec.max(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BenchOpts {
    samples: u64,
    warmup: u64,
    core: usize,
}

/// Parse `--samples N --warmup N --core N --out DIR`. Minimal, hand-rolled.
fn parse(args: &[String]) -> (BenchOpts, PathBuf) {
    let mut samples = 1_000_000u64;
    let mut warmup = 100_000u64;
    let mut core = 0usize;
    let mut out = default_results_dir();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--samples" => samples = it.next().and_then(|s| s.parse().ok()).unwrap_or(samples),
            "--warmup" => warmup = it.next().and_then(|s| s.parse().ok()).unwrap_or(warmup),
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            "--out" => {
                if let Some(d) = it.next() {
                    out = PathBuf::from(d);
                }
            }
            _ => {}
        }
    }
    (BenchOpts { samples, warmup, core }, out)
}

fn default_results_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"))
}

/// Entry point for `bench read [--samples N] [--warmup N] [--core N] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(opts.core);
    let overhead = clock.overhead_ns();
    eprintln!(
        "read path: samples={} warmup={} core={} (pinned={}) clock_overhead_ns={}",
        opts.samples, opts.warmup, opts.core, pinned, overhead
    );

    let mut rows: Vec<Row> = Vec::new();
    for &impl_name in &harness::IMPLS {
        for &depth in &DEPTHS {
            let ctx = Ctx {
                clock: &clock,
                mid: MID,
                depth,
                samples: opts.samples,
                warmup: opts.warmup,
            };
            let cell = run_impl(impl_name, &ctx);
            for (op, rec) in [
                ("best_bid", &cell.best_bid),
                ("top_n_8", &cell.top_n_8),
                ("top_n_full", &cell.top_n_full),
            ] {
                rows.push(Row {
                    impl_name,
                    depth,
                    op,
                    samples: rec.count(),
                    overhead_ns: overhead,
                    rec_summary: Summary::of(rec),
                });
            }
            eprintln!(
                "  {:<6} d={:<5} best_bid p50={:>4}ns p99={:>5}ns | top_n_full p50={:>5}ns",
                impl_name,
                depth,
                cell.best_bid.p(0.50),
                cell.best_bid.p(0.99),
                cell.top_n_full.p(0.50),
            );
        }
    }

    write_csv(&out_dir.join("read_path.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("impl,depth,op,samples,clock_overhead_ns,mean_ns,p50_ns,p99_ns,p999_ns,max_ns\n");
    for r in rows {
        let m = r.rec_summary;
        let _ = writeln!(
            s,
            "{},{},{},{},{},{:.1},{},{},{},{}",
            r.impl_name, r.depth, r.op, r.samples, r.overhead_ns, m.mean, m.p50, m.p99, m.p999, m.max,
        );
    }
    std::fs::write(path, s).expect("write read_path.csv");
    eprintln!("wrote {}", path.display());
}

fn best_bid_p50(rows: &[Row], impl_name: &str, depth: usize) -> Option<u64> {
    rows.iter()
        .find(|r| r.op == "best_bid" && r.impl_name == impl_name && r.depth == depth)
        .map(|r| r.rec_summary.p50)
}

/// Headline: the `best_bid` read tax (§6) — its p50 vs depth for each impl. The
/// Vec impls should stay flat (O(1)); `BTree` should climb (O(log n) descent).
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== READ-TAX HEADLINE (best_bid p50, ns) ====");
    let probe = [1usize, 64, 2048];
    eprint!("{:<7}", "impl");
    for d in probe {
        eprint!(" d={d:<5}");
    }
    eprintln!();
    for &impl_name in &harness::IMPLS {
        eprint!("{impl_name:<7}");
        for d in probe {
            match best_bid_p50(rows, impl_name, d) {
                Some(v) => eprint!(" {v:<7}"),
                None => eprint!(" {:<7}", "-"),
            }
        }
        eprintln!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Elision guard (§3.3): `top_n_full` must cost more at a deep ladder than at
    /// a shallow one — the copy is linear in `depth`. A flat-across-depth result
    /// is the elision signature (the optimizer deleted the copy).
    #[test]
    fn top_n_full_scales_with_depth() {
        let clock = BenchClock::new();
        let mk = |depth: usize| {
            let ctx = Ctx {
                clock: &clock,
                mid: MID,
                depth,
                samples: 100_000,
                warmup: 10_000,
            };
            let book = build_at_depth::<RevVecBook>(MID, depth);
            let mut buf = vec![(Px(0), Qty(0)); depth];
            measure_top_n(&ctx, &book, &mut buf).mean()
        };
        let shallow = mk(8);
        let deep = mk(1024);
        assert!(
            deep > shallow,
            "top_n_full must cost more at depth 1024 ({deep:.1}ns) than depth 8 ({shallow:.1}ns)"
        );
    }

    /// `best_bid` is exercised for real (count == samples) and reads back the
    /// expected best level — a functional/elision guard that does not depend on
    /// ns-scale timing (the H3 O(log n) read-tax claim itself is quantified in
    /// release mode in RESULTS.md, not asserted against debug-build clock noise).
    #[test]
    fn best_bid_is_exercised_and_correct() {
        let clock = BenchClock::new();
        let ctx = Ctx {
            clock: &clock,
            mid: MID,
            depth: 64,
            samples: 50_000,
            warmup: 5_000,
        };
        let book = build_at_depth::<BTreeBook>(MID, 64);
        let rec = measure_best_bid(&ctx, &book);
        assert_eq!(rec.count(), 50_000, "every best_bid call must be recorded");
        // The measured book really holds the offset-0 best bid (mid - STEP).
        assert_eq!(book.best_bid().map(|(p, _)| p), Some(Px(MID.0 - 2)));
    }

    /// The dispatch matches the four impls and rejects unknown names.
    #[test]
    #[should_panic(expected = "unknown impl")]
    fn run_impl_rejects_unknown() {
        let clock = BenchClock::new();
        let ctx = Ctx {
            clock: &clock,
            mid: MID,
            depth: 1,
            samples: 1,
            warmup: 0,
        };
        let _ = run_impl("nope", &ctx);
    }
}

//! Benchmark 1 — service-time depth sweep (§5): THE crossover artifact.
//!
//! For each `impl × Locality × depth` we time, individually, ≥1M `apply`s of
//! four ops and record their interior latency distributions:
//!
//! - `update` — in-place qty change at an existing level. Isolates the *locate*
//!   cost (linear scan vs binary search vs tree descent) — the crossover variable.
//! - `insert` — a new in-band gap level (locate + memmove); the book is restored
//!   untimed after each timed op so depth is held constant across the loop.
//! - `remove` — delete a level (locate + memmove); the level is inserted untimed
//!   before each timed op so depth is held constant.
//! - `trade_baseline` — a `Trade` apply (last-trade cache only): the dispatch
//!   floor every other op is read against.
//!
//! This is **service time**: there is no arrival process and therefore no
//! coordinated omission — it measures the operation itself (§3.1). Every measured
//! op `black_box`es its input event and the resulting state (§3.3).

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use crate::workload::{Locality, build_at_depth, gap_price, touch_price};
use book::{BTreeBook, BookEvent, FlatBook, OrderBook, Px, Qty, RevVecBook, Side, SortedVecBook};
use feed::rng::SplitMix64;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// Mid price the ladder is built around. Large enough that `mid ± STEP*depth`
/// never approaches zero at the deepest rung.
const MID: Px = Px(1_000_000);

/// The depth ladder (§5). Log-spaced so the crossover is visible on a log-x plot.
const DEPTHS: [usize; 12] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048];

/// Base seed for the deterministic touch streams (mixed with cell coordinates).
const SEED: u64 = 0xB1A5_C0DE_F00D_1234;

/// Fallback `.hgrm` export depth when a locality shows no rev↔sorted crossover.
const HGRM_FALLBACK_DEPTH: usize = 256;

/// Per-cell measurement context (bundled to keep arg counts sane).
#[derive(Debug)]
struct Ctx<'a> {
    clock: &'a BenchClock,
    mid: Px,
    depth: usize,
    loc: Locality,
    samples: u64,
    warmup: u64,
}

/// The four recorders produced for one `impl × locality × depth` cell.
#[derive(Debug)]
struct CellResult {
    update: Recorder,
    insert: Recorder,
    remove: Recorder,
    trade: Recorder,
}

/// Next odd, positive, varying qty — never zero, so an `update` stays an
/// in-place replace and never degenerates into a remove.
#[inline]
fn next_qty(q: &mut i64) -> Qty {
    *q = (q.wrapping_add(1) & 0x7FFF) | 1;
    Qty(*q)
}

#[inline]
fn rng_side(rng: &mut SplitMix64) -> Side {
    if rng.next_u64() & 1 == 0 { Side::Bid } else { Side::Ask }
}

/// `update`: time an in-place qty replace at an existing level. Book structure
/// (prices, depth) is invariant across the loop, so 1M timed ops stay at `depth`.
fn measure_update<B: OrderBook>(ctx: &Ctx<'_>, book: &mut B, seed: u64) -> Recorder {
    let mut rng = SplitMix64::new(seed);
    let mut q = 1i64;
    harness::warmup(ctx.warmup, || {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc);
        book.apply(&BookEvent::level(0, 0, side, px, next_qty(&mut q)));
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc);
        let ev = BookEvent::level(0, 0, side, px, next_qty(&mut q));
        let t0 = ctx.clock.raw();
        black_box(&mut *book).apply(black_box(&ev));
        let t1 = ctx.clock.raw();
        rec.record(ctx.clock.delta_ns(t0, t1));
    }
    black_box(book.best_bid());
    rec
}

/// `insert`: time a new in-band gap level (locate + memmove); restore untimed.
fn measure_insert<B: OrderBook>(ctx: &Ctx<'_>, book: &mut B, seed: u64) -> Recorder {
    let mut rng = SplitMix64::new(seed);
    let mut q = 1i64;
    harness::warmup(ctx.warmup, || {
        let side = rng_side(&mut rng);
        let gap = gap_price(touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc), side);
        book.apply(&BookEvent::level(0, 0, side, gap, next_qty(&mut q)));
        book.apply(&BookEvent::level(0, 0, side, gap, Qty::ZERO));
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let side = rng_side(&mut rng);
        let gap = gap_price(touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc), side);
        let ev = BookEvent::level(0, 0, side, gap, next_qty(&mut q));
        let t0 = ctx.clock.raw();
        black_box(&mut *book).apply(black_box(&ev));
        let t1 = ctx.clock.raw();
        rec.record(ctx.clock.delta_ns(t0, t1));
        book.apply(&BookEvent::level(0, 0, side, gap, Qty::ZERO)); // untimed restore
    }
    black_box(book.best_bid());
    rec
}

/// `remove`: time deleting a level (locate + memmove); insert untimed first.
fn measure_remove<B: OrderBook>(ctx: &Ctx<'_>, book: &mut B, seed: u64) -> Recorder {
    let mut rng = SplitMix64::new(seed);
    let mut q = 1i64;
    harness::warmup(ctx.warmup, || {
        let side = rng_side(&mut rng);
        let gap = gap_price(touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc), side);
        book.apply(&BookEvent::level(0, 0, side, gap, next_qty(&mut q)));
        book.apply(&BookEvent::level(0, 0, side, gap, Qty::ZERO));
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let side = rng_side(&mut rng);
        let gap = gap_price(touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc), side);
        book.apply(&BookEvent::level(0, 0, side, gap, next_qty(&mut q))); // untimed insert
        let ev = BookEvent::level(0, 0, side, gap, Qty::ZERO);
        let t0 = ctx.clock.raw();
        black_box(&mut *book).apply(black_box(&ev));
        let t1 = ctx.clock.raw();
        rec.record(ctx.clock.delta_ns(t0, t1));
    }
    black_box(book.best_bid());
    rec
}

/// `trade_baseline`: time a `Trade` apply — the dispatch/last-trade-cache floor.
fn measure_trade<B: OrderBook>(ctx: &Ctx<'_>, book: &mut B, seed: u64) -> Recorder {
    let mut rng = SplitMix64::new(seed);
    harness::warmup(ctx.warmup, || {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc);
        book.apply(&BookEvent::trade(0, 0, side, px, Qty(1)));
    });
    let mut rec = Recorder::new();
    for _ in 0..ctx.samples {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, ctx.mid, ctx.depth, side, ctx.loc);
        let ev = BookEvent::trade(0, 0, side, px, Qty(1));
        let t0 = ctx.clock.raw();
        black_box(&mut *book).apply(black_box(&ev));
        let t1 = ctx.clock.raw();
        rec.record(ctx.clock.delta_ns(t0, t1));
    }
    black_box(book.last_trade());
    rec
}

/// Run all four ops for one cell, monomorphized over the concrete book `B`.
/// Each op gets a fresh book (untimed build) and an independent touch stream.
fn sweep_cell<B: OrderBook>(ctx: &Ctx<'_>, seed: u64) -> CellResult {
    let mut bu = build_at_depth::<B>(ctx.mid, ctx.depth);
    let update = measure_update(ctx, &mut bu, seed ^ 0x1111);
    let mut bi = build_at_depth::<B>(ctx.mid, ctx.depth);
    let insert = measure_insert(ctx, &mut bi, seed ^ 0x2222);
    let mut br = build_at_depth::<B>(ctx.mid, ctx.depth);
    let remove = measure_remove(ctx, &mut br, seed ^ 0x3333);
    let mut bt = build_at_depth::<B>(ctx.mid, ctx.depth);
    let trade = measure_trade(ctx, &mut bt, seed ^ 0x4444);
    CellResult { update, insert, remove, trade }
}

/// The monomorphized impl dispatch (no `dyn OrderBook`). The name is validated
/// against the registry first, then matched to a concrete type — each arm
/// instantiates a fresh `sweep_cell::<Concrete>` so `apply` inlines.
fn run_impl(name: &str, ctx: &Ctx<'_>, seed: u64) -> CellResult {
    match harness::for_impl(name) {
        Some("btree") => sweep_cell::<BTreeBook>(ctx, seed),
        Some("sorted") => sweep_cell::<SortedVecBook>(ctx, seed),
        Some("rev") => sweep_cell::<RevVecBook>(ctx, seed),
        Some("flat") => sweep_cell::<FlatBook>(ctx, seed),
        _ => panic!("unknown impl `{name}` (expected one of {:?})", harness::IMPLS),
    }
}

/// One CSV row.
#[derive(Debug)]
struct Row {
    impl_name: &'static str,
    loc: Locality,
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
    p90: u64,
    p99: u64,
    p999: u64,
    max: u64,
}

impl Summary {
    fn of(rec: &Recorder) -> Self {
        Self {
            mean: rec.mean(),
            p50: rec.p(0.50),
            p90: rec.p(0.90),
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

/// Mix cell coordinates into the base seed for an independent-but-reproducible stream.
fn cell_seed(impl_idx: usize, loc: Locality, depth: usize) -> u64 {
    let loc_bit = match loc {
        Locality::Concentrated => 0u64,
        Locality::Uniform => 1u64,
    };
    SEED ^ (impl_idx as u64).wrapping_mul(0x9E37_79B9)
        ^ (loc_bit << 40)
        ^ (depth as u64).wrapping_mul(0x1000_0001)
}

/// Entry point for `bench service [--samples N] [--warmup N] [--core N] [--out DIR]`.
pub fn run(args: &[String]) {
    let (opts, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(opts.core);
    let overhead = clock.overhead_ns();
    eprintln!(
        "service sweep: samples={} warmup={} core={} (pinned={}) clock_overhead_ns={}",
        opts.samples, opts.warmup, opts.core, pinned, overhead
    );

    let mut rows: Vec<Row> = Vec::new();
    // Retain the `update` recorders so we can export the .hgrm at the crossover depth.
    let mut update_recs: Vec<(&'static str, Locality, usize, Recorder)> = Vec::new();

    for &loc in &[Locality::Concentrated, Locality::Uniform] {
        for (impl_idx, &impl_name) in harness::IMPLS.iter().enumerate() {
            for &depth in &DEPTHS {
                let ctx = Ctx {
                    clock: &clock,
                    mid: MID,
                    depth,
                    loc,
                    samples: opts.samples,
                    warmup: opts.warmup,
                };
                let cell = run_impl(impl_name, &ctx, cell_seed(impl_idx, loc, depth));
                for (op, rec) in [
                    ("update", &cell.update),
                    ("insert", &cell.insert),
                    ("remove", &cell.remove),
                    ("trade_baseline", &cell.trade),
                ] {
                    rows.push(Row {
                        impl_name,
                        loc,
                        depth,
                        op,
                        samples: rec.count(), // actual recorded count
                        overhead_ns: overhead,
                        rec_summary: Summary::of(rec),
                    });
                }
                eprintln!(
                    "  {:<6} {:<12} d={:<5} update p50={:>5}ns p99={:>6}ns",
                    impl_name,
                    loc.tag(),
                    depth,
                    cell.update.p(0.50),
                    cell.update.p(0.99),
                );
                update_recs.push((impl_name, loc, depth, cell.update));
            }
        }
    }

    write_csv(&out_dir.join("service_sweep.csv"), &rows);
    let crossovers = export_and_report(&out_dir, &rows, &update_recs);
    print_headline(&rows, &crossovers);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("impl,locality,depth,op,samples,clock_overhead_ns,mean_ns,p50_ns,p90_ns,p99_ns,p999_ns,max_ns\n");
    for r in rows {
        let m = r.rec_summary;
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{:.1},{},{},{},{},{}",
            r.impl_name,
            r.loc.tag(),
            r.depth,
            r.op,
            r.samples,
            r.overhead_ns,
            m.mean,
            m.p50,
            m.p90,
            m.p99,
            m.p999,
            m.max,
        );
    }
    std::fs::write(path, s).expect("write service_sweep.csv");
    eprintln!("wrote {}", path.display());
}

/// Look up an `update` p50 for (impl, loc, depth) from the committed rows.
fn update_p50(rows: &[Row], impl_name: &str, loc: Locality, depth: usize) -> Option<u64> {
    rows.iter()
        .find(|r| r.op == "update" && r.impl_name == impl_name && r.loc == loc && r.depth == depth)
        .map(|r| r.rec_summary.p50)
}

fn update_p99(rows: &[Row], impl_name: &str, loc: Locality, depth: usize) -> Option<u64> {
    rows.iter()
        .find(|r| r.op == "update" && r.impl_name == impl_name && r.loc == loc && r.depth == depth)
        .map(|r| r.rec_summary.p99)
}

/// The rev↔sorted crossover depth `D*` for a locality: the smallest depth from
/// which `RevVec`'s linear-scan `update` p50 *persistently* exceeds `SortedVec`'s
/// binary search — i.e. rev > sorted at that depth and every deeper depth.
///
/// Persistence (rather than the first single-point overtake) is deliberate: at
/// shallow depth both impls sit within ~1 ns of each other at the timer floor,
/// and a one-off flip there is noise, not a structural crossover. The real
/// crossover is the depth beyond which the linear scan never recovers.
fn crossover_depth(rows: &[Row], loc: Locality) -> Option<usize> {
    let mut dstar = None;
    for &d in DEPTHS.iter().rev() {
        let rev_over_sorted = match (update_p50(rows, "rev", loc, d), update_p50(rows, "sorted", loc, d)) {
            (Some(rev), Some(sorted)) => rev > sorted,
            _ => false,
        };
        if rev_over_sorted {
            dstar = Some(d);
        } else {
            break;
        }
    }
    dstar
}

/// Export the per-impl `.hgrm` interior distributions at each locality's
/// crossover depth (fallback to [`HGRM_FALLBACK_DEPTH`] if none), and return the
/// per-locality `D*` for the headline.
fn export_and_report(
    out_dir: &Path,
    rows: &[Row],
    update_recs: &[(&'static str, Locality, usize, Recorder)],
) -> Vec<(Locality, Option<usize>)> {
    let mut crossovers = Vec::new();
    for &loc in &[Locality::Concentrated, Locality::Uniform] {
        let dstar = crossover_depth(rows, loc);
        let export_depth = dstar.unwrap_or(HGRM_FALLBACK_DEPTH);
        for (impl_name, rloc, depth, rec) in update_recs {
            if *rloc == loc && *depth == export_depth {
                let path = out_dir.join(format!(
                    "service_update_{}_{}_d{}.hgrm",
                    impl_name,
                    loc.tag(),
                    export_depth
                ));
                if let Err(e) = rec.export_hgrm(&path) {
                    eprintln!("warn: failed to export {}: {e}", path.display());
                } else {
                    eprintln!("wrote {}", path.display());
                }
            }
        }
        crossovers.push((loc, dstar));
    }
    crossovers
}

fn prev_depth(depth: usize) -> Option<usize> {
    let i = DEPTHS.iter().position(|&d| d == depth)?;
    if i == 0 { None } else { Some(DEPTHS[i - 1]) }
}

fn print_headline(rows: &[Row], crossovers: &[(Locality, Option<usize>)]) {
    eprintln!("\n==== CROSSOVER HEADLINE (update p99, ns) ====");
    for &(loc, dstar) in crossovers {
        match dstar {
            None => eprintln!(
                "{}: no rev>sorted crossover within depth<=2048 (rev stays <= sorted)",
                loc.tag()
            ),
            Some(d) => {
                eprint!("{}: D* = {d}", loc.tag());
                if let Some(below) = prev_depth(d) {
                    if let (Some(rev_b), Some(sorted_b)) =
                        (update_p99(rows, "rev", loc, below), update_p99(rows, "sorted", loc, below))
                    {
                        eprint!(" | below d={below}: rev={rev_b} sorted={sorted_b}");
                    }
                }
                if let (Some(rev_a), Some(sorted_a)) =
                    (update_p99(rows, "rev", loc, d), update_p99(rows, "sorted", loc, d))
                {
                    eprint!(" | at d={d}: rev={rev_a} sorted={sorted_a}");
                }
                eprintln!();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Elision guard (§3.3): a measured `update` at depth 1 must exceed the clock
    /// floor. If the optimizer had deleted the `apply` (unused result), the timed
    /// delta would collapse to the read-read overhead; a measurement above the
    /// floor proves real work happened.
    #[test]
    fn update_at_depth_1_exceeds_clock_floor() {
        let clock = BenchClock::new();
        let ctx = Ctx {
            clock: &clock,
            mid: MID,
            depth: 1,
            loc: Locality::Concentrated,
            samples: 200_000,
            warmup: 20_000,
        };
        let mut book = build_at_depth::<RevVecBook>(MID, 1);
        let rec = measure_update(&ctx, &mut book, 0xE115_0114);
        let floor = clock.overhead_ns();
        assert!(
            rec.mean() > f64::from(u32::try_from(floor).unwrap_or(u32::MAX)),
            "update mean {:.2}ns must exceed clock floor {floor}ns (elision?)",
            rec.mean()
        );
        assert_eq!(rec.count(), 200_000);
    }

    /// Release-robust elision guard: `update` cost must scale with depth for the
    /// linear-scan impl. A flat-across-depth result is the elision signature (§3.3).
    #[test]
    fn update_cost_scales_with_depth_for_revvec() {
        let clock = BenchClock::new();
        let mk = |depth: usize| {
            let ctx = Ctx {
                clock: &clock,
                mid: MID,
                depth,
                loc: Locality::Uniform,
                samples: 100_000,
                warmup: 10_000,
            };
            let mut book = build_at_depth::<RevVecBook>(MID, depth);
            measure_update(&ctx, &mut book, 0x5CA1E).mean()
        };
        let shallow = mk(1);
        let deep = mk(256);
        assert!(
            deep > shallow,
            "RevVec update must cost more at depth 256 ({deep:.1}ns) than depth 1 ({shallow:.1}ns)"
        );
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
            loc: Locality::Concentrated,
            samples: 1,
            warmup: 0,
        };
        let _ = run_impl("nope", &ctx, 0);
    }
}

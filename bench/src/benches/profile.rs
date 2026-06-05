//! `bench profile` (§2.2) — the isolated, **untimed** hot loop.
//!
//! Runs `--iters N` `apply` (or read `search`) calls against a book built at
//! `--depth D` and the chosen `--locality`, each wrapped in `black_box`, with
//! **no per-op timing**: an external profiler (`perf stat` / `--topdown`, §2.3)
//! measures cycles and attributes them to `apply` cleanly. The thread is pinned;
//! the build, warmup, and event-stream generation are untimed so the profiled
//! region is the `apply` work alone. This subcommand emits **no CSV** — the
//! counters come from `perf`; if `perf` is unavailable it is still the canonical,
//! reproducible hot loop the §3 behavioral experiments and `PROFILING.md` cite.

use crate::harness;
use crate::workload::{Locality, build_at_depth_fast, touch_price};
use book::{BTreeBook, BookEvent, FlatBook, OrderBook, Px, Qty, RevVecBook, Side, SortedVecBook};
use feed::rng::SplitMix64;
use std::hint::black_box;

/// Mid price the ladder is built around (matches the service sweep).
const MID: Px = Px(1_000_000);
/// Base seed for the deterministic touch stream.
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
/// Pre-generated event-buffer length: large enough that the apply stream is not
/// trivially cached/predicted, small enough to build untimed. Cycled in the loop
/// so RNG/touch cost stays OUT of the profiled region.
const EVENT_BUF: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Apply,
    Search,
}

#[derive(Debug, Clone, Copy)]
struct Opts {
    impl_name: &'static str,
    op: Op,
    depth: usize,
    loc: Locality,
    iters: u64,
    core: usize,
}

/// Next odd, positive qty — never zero, so an `apply` stays an in-place replace.
#[inline]
fn next_qty(q: &mut i64) -> Qty {
    *q = (q.wrapping_add(1) & 0x7FFF) | 1;
    Qty(*q)
}

#[inline]
fn rng_side(rng: &mut SplitMix64) -> Side {
    if rng.next_u64() & 1 == 0 { Side::Bid } else { Side::Ask }
}

/// The untimed apply hot loop: cycle a pre-generated event buffer through
/// `apply`, `black_box`-ing the book and event each iteration to defeat elision.
fn run_apply<B: OrderBook>(o: &Opts) {
    let mut book: B = build_at_depth_fast(MID, o.depth);
    // Pre-generate the event stream (untimed) so only `apply` is in the hot loop.
    let mut rng = SplitMix64::new(SEED ^ (o.depth as u64));
    let mut q = 1i64;
    let mut events: Vec<BookEvent> = Vec::with_capacity(EVENT_BUF);
    for _ in 0..EVENT_BUF {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, MID, o.depth, side, o.loc);
        events.push(BookEvent::level(0, 0, side, px, next_qty(&mut q)));
    }
    // Warmup (untimed): warm I-cache, D-cache, branch predictor, core frequency.
    let warm = EVENT_BUF.min(usize::try_from(o.iters).unwrap_or(usize::MAX));
    for ev in events.iter().take(warm) {
        book.apply(ev);
    }
    // The profiled region: N applies, no timing.
    let mut idx = 0usize;
    for _ in 0..o.iters {
        let ev = &events[idx];
        black_box(&mut book).apply(black_box(ev));
        idx += 1;
        if idx == EVENT_BUF {
            idx = 0;
        }
    }
    black_box(book.best_bid());
    black_box(book.best_ask());
}

/// The untimed read-locate hot loop: repeated top-of-book reads (`best_bid` /
/// `best_ask`) — the read-side locate the profiler attributes per impl.
fn run_search<B: OrderBook>(o: &Opts) {
    let book: B = build_at_depth_fast(MID, o.depth);
    let warm = EVENT_BUF.min(usize::try_from(o.iters).unwrap_or(usize::MAX));
    for _ in 0..warm {
        black_box(book.best_bid());
    }
    for _ in 0..o.iters {
        black_box(black_box(&book).best_bid());
        black_box(black_box(&book).best_ask());
    }
}

/// Monomorphized dispatch (no `dyn`): each arm instantiates the concrete book so
/// `apply` inlines and the profiler sees the real, specialized code.
fn dispatch(o: &Opts) {
    match (o.impl_name, o.op) {
        ("btree", Op::Apply) => run_apply::<BTreeBook>(o),
        ("sorted", Op::Apply) => run_apply::<SortedVecBook>(o),
        ("rev", Op::Apply) => run_apply::<RevVecBook>(o),
        ("flat", Op::Apply) => run_apply::<FlatBook>(o),
        ("btree", Op::Search) => run_search::<BTreeBook>(o),
        ("sorted", Op::Search) => run_search::<SortedVecBook>(o),
        ("rev", Op::Search) => run_search::<RevVecBook>(o),
        ("flat", Op::Search) => run_search::<FlatBook>(o),
        _ => panic!("unknown impl `{}` (expected one of {:?})", o.impl_name, harness::IMPLS),
    }
}

fn parse(args: &[String]) -> Opts {
    let mut impl_name = "sorted";
    let mut op = Op::Apply;
    let mut depth = 2048usize;
    let mut loc = Locality::Uniform;
    let mut iters = 200_000_000u64;
    let mut core = 0usize;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--impl" => {
                if let Some(v) = it.next() {
                    impl_name = harness::for_impl(v)
                        .unwrap_or_else(|| panic!("unknown impl `{v}` (expected {:?})", harness::IMPLS));
                }
            }
            "--op" => {
                op = match it.next().map(String::as_str) {
                    Some("apply") => Op::Apply,
                    Some("search") => Op::Search,
                    Some(other) => panic!("unknown op `{other}` (expected apply|search)"),
                    None => op,
                };
            }
            "--depth" => depth = it.next().and_then(|s| s.parse().ok()).unwrap_or(depth).max(1),
            "--locality" => {
                loc = match it.next().map(String::as_str) {
                    Some("concentrated") => Locality::Concentrated,
                    Some("uniform") => Locality::Uniform,
                    Some(other) => panic!("unknown locality `{other}` (expected concentrated|uniform)"),
                    None => loc,
                };
            }
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            _ => {}
        }
    }
    Opts { impl_name, op, depth, loc, iters, core }
}

/// Entry point: `bench profile --impl X --op apply|search --depth D
/// --locality concentrated|uniform --iters N [--core C]`.
pub fn run(args: &[String]) {
    let o = parse(args);
    let pinned = harness::pin_to_core(o.core);
    let op = match o.op {
        Op::Apply => "apply",
        Op::Search => "search",
    };
    eprintln!(
        "profile: impl={} op={op} depth={} locality={} iters={} core={} (pinned={})",
        o.impl_name,
        o.depth,
        o.loc.tag(),
        o.iters,
        o.core,
        pinned
    );
    let t0 = std::time::Instant::now();
    dispatch(&o);
    eprintln!("profile: done in {:.2}s (attach perf externally for counters)", t0.elapsed().as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hot loop runs for every impl × op without panicking at a small iter
    /// count — the smoke test that the monomorphized dispatch is wired correctly.
    #[test]
    fn dispatch_runs_for_every_impl_and_op() {
        for &impl_name in &harness::IMPLS {
            for op in [Op::Apply, Op::Search] {
                let o = Opts { impl_name, op, depth: 64, loc: Locality::Uniform, iters: 5_000, core: 0 };
                dispatch(&o);
            }
        }
    }

    #[test]
    #[should_panic(expected = "unknown impl")]
    fn dispatch_rejects_unknown_impl() {
        let o = Opts {
            impl_name: "nope",
            op: Op::Apply,
            depth: 1,
            loc: Locality::Uniform,
            iters: 1,
            core: 0,
        };
        dispatch(&o);
    }
}

//! `bench cache-exp` (§3.2) — the cache-hierarchy signature **without a PMU**.
//!
//! Sweeps book depth so the per-side level-array footprint crosses L1 → L2 → LLC
//! (host cache sizes read from `/sys`), measuring `update` apply p50/p99 per impl
//! at **uniform** locality (touches land flat across the whole array, defeating
//! the prefetcher as the footprint exceeds each cache). The expected signature:
//! contiguous Vecs (binary search / direct index) degrade gracefully and step at
//! the boundaries; `BTreeBook`'s scattered nodes are elevated even when resident;
//! `FlatBook` is flat until its span exceeds the cache (the real-book regime).
//! Writes `cache_experiment.csv`, annotated with each cell's footprint and the
//! cache level that footprint still fits within.
//!
//! `RevVecBook` is O(depth²) to build and O(depth) per update (its locate is a
//! linear scan), so it is swept only up to [`REV_MAX_DEPTH`]; deeper cells are
//! skipped (the linear-scan arm is core/retiring-bound, not a cache probe — that
//! distinction is exactly what `PROFILING.md` draws).

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use crate::workload::{Locality, build_at_depth_fast, touch_price};
use book::{BTreeBook, BookEvent, FlatBook, OrderBook, Px, Qty, RevVecBook, Side, SortedVecBook};
use feed::rng::SplitMix64;
use std::fmt::Write as _;
use std::hint::black_box;
use std::mem::size_of;
use std::path::{Path, PathBuf};

const MID: Px = Px(1_000_000);
const SEED: u64 = 0xCACE_5EED_1234_5678;

/// Depth ladder whose per-side footprint (`depth * 16 B`) climbs from 4 KiB
/// (256) to 16 MiB (1,048,576), crossing L1 (≈48 KiB), L2 (≈1.25 MiB) and LLC
/// (≈8 MiB) on this host.
const DEPTHS: [usize; 7] = [256, 1024, 4096, 16384, 65536, 262_144, 1_048_576];
/// `RevVecBook`'s O(depth²) build and O(depth) update cap its feasible depth.
const REV_MAX_DEPTH: usize = 16384;
/// Samples per cell for the O(1)/O(log) impls.
const SAMPLES: u64 = 200_000;
/// Work budget bounding the O(depth) `RevVecBook` cells (samples scale as
/// `budget / depth`, clamped) so a deep linear scan does not run unbounded.
const REV_BUDGET: u64 = 2_000_000_000;

/// Bytes a single contiguous level occupies: `(Px, Qty)` = two `i64`s = 16 B.
fn level_bytes() -> u64 {
    size_of::<(Px, Qty)>() as u64
}

#[derive(Debug, Clone, Copy)]
struct Caches {
    l1d: u64,
    l2: u64,
    llc: u64,
}

/// Parse a `/sys` cache `size` string (`"48K"`, `"1280K"`, `"8192K"`, `"8M"`) to
/// bytes. Unknown suffixes fall back to a raw byte count.
fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') => (&s[..s.len() - 1], 1024u64),
        Some('M') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().unwrap_or(0) * mult
}

fn read_field(idx: usize, field: &str) -> Option<String> {
    let path = format!("/sys/devices/system/cpu/cpu0/cache/index{idx}/{field}");
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Read host L1d / L2 / LLC sizes from `/sys`. Falls back to this host's measured
/// sizes (recorded in `PROFILING.md`) if a node is missing.
fn read_caches() -> Caches {
    let (mut l1d, mut l2, mut llc) = (0u64, 0u64, 0u64);
    for idx in 0..16 {
        let Some(level) = read_field(idx, "level") else { break };
        let kind = read_field(idx, "type").unwrap_or_default();
        let size = read_field(idx, "size").map_or(0, |s| parse_size(&s));
        match (level.as_str(), kind.as_str()) {
            ("1", "Data") => l1d = size,
            ("2", _) => l2 = size,
            ("3", _) => llc = size,
            _ => {}
        }
    }
    Caches {
        l1d: if l1d == 0 { 48 * 1024 } else { l1d },
        l2: if l2 == 0 { 1280 * 1024 } else { l2 },
        llc: if llc == 0 { 8192 * 1024 } else { llc },
    }
}

/// The smallest cache level whose capacity still holds `footprint` (the resident
/// level — the latency step happens when the footprint outgrows it).
fn resident_level(footprint: u64, c: &Caches) -> &'static str {
    if footprint <= c.l1d {
        "L1"
    } else if footprint <= c.l2 {
        "L2"
    } else if footprint <= c.llc {
        "LLC"
    } else {
        "DRAM"
    }
}

#[inline]
fn next_qty(q: &mut i64) -> Qty {
    *q = (q.wrapping_add(1) & 0x7FFF) | 1;
    Qty(*q)
}

#[inline]
fn rng_side(rng: &mut SplitMix64) -> Side {
    if rng.next_u64() & 1 == 0 { Side::Bid } else { Side::Ask }
}

/// Time `samples` in-place `update` applies at uniform locality over a book of
/// `depth`. The book structure is invariant across the loop (qty replace only),
/// so every timed op stays at `depth`. Input event and book are `black_box`ed.
fn measure_update<B: OrderBook>(clock: &BenchClock, book: &mut B, depth: usize, samples: u64, warmup: u64, seed: u64) -> Recorder {
    let mut rng = SplitMix64::new(seed);
    let mut q = 1i64;
    for _ in 0..warmup {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, MID, depth, side, Locality::Uniform);
        book.apply(&BookEvent::level(0, 0, side, px, next_qty(&mut q)));
    }
    let mut rec = Recorder::new();
    for _ in 0..samples {
        let side = rng_side(&mut rng);
        let px = touch_price(&mut rng, MID, depth, side, Locality::Uniform);
        let ev = BookEvent::level(0, 0, side, px, next_qty(&mut q));
        let t0 = clock.raw();
        black_box(&mut *book).apply(black_box(&ev));
        let t1 = clock.raw();
        rec.record(clock.delta_ns(t0, t1));
    }
    black_box(book.best_bid());
    rec
}

/// Build + measure one `(impl, depth)` cell, returning the recorder and the
/// resident footprint in bytes (exact span for `FlatBook`; the contiguous level
/// total for the Vecs; an entry+node estimate for `BTreeBook`).
fn cell(impl_name: &str, depth: usize, clock: &BenchClock, samples: u64, warmup: u64, seed: u64) -> (Recorder, u64) {
    match impl_name {
        "sorted" => {
            let mut b: SortedVecBook = build_at_depth_fast(MID, depth);
            (measure_update(clock, &mut b, depth, samples, warmup, seed), depth as u64 * level_bytes())
        }
        "rev" => {
            let mut b: RevVecBook = build_at_depth_fast(MID, depth);
            (measure_update(clock, &mut b, depth, samples, warmup, seed), depth as u64 * level_bytes())
        }
        "btree" => {
            let mut b: BTreeBook = build_at_depth_fast(MID, depth);
            // Node estimate: entry (16 B) + ~50% B-tree node/pointer overhead.
            (measure_update(clock, &mut b, depth, samples, warmup, seed), depth as u64 * level_bytes() * 3 / 2)
        }
        "flat" => {
            let mut b: FlatBook = build_at_depth_fast(MID, depth);
            // Exact resident span: two parallel `Qty` arrays of the allocated span.
            let footprint = b.allocated_span_ticks() as u64 * 2 * size_of::<Qty>() as u64;
            (measure_update(clock, &mut b, depth, samples, warmup, seed), footprint)
        }
        _ => panic!("unknown impl `{impl_name}` (expected one of {:?})", harness::IMPLS),
    }
}

#[derive(Debug)]
struct Row {
    impl_name: &'static str,
    depth: usize,
    footprint_bytes: u64,
    cache_level: &'static str,
    samples: u64,
    overhead_ns: u64,
    p50_ns: u64,
    p99_ns: u64,
}

pub fn run(args: &[String]) {
    let (core, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");
    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(core);
    let caches = read_caches();
    let overhead = clock.overhead_ns();
    eprintln!(
        "cache-exp: L1d={} KiB L2={} KiB LLC={} KiB core={core} (pinned={pinned}) clock_overhead_ns={overhead}",
        caches.l1d / 1024,
        caches.l2 / 1024,
        caches.llc / 1024,
    );

    let mut rows: Vec<Row> = Vec::new();
    for &impl_name in &harness::IMPLS {
        for &depth in &DEPTHS {
            if impl_name == "rev" && depth > REV_MAX_DEPTH {
                continue; // O(depth) linear scan — skip the deep cells (documented)
            }
            let samples = if impl_name == "rev" {
                (REV_BUDGET / depth as u64).clamp(20_000, SAMPLES)
            } else {
                SAMPLES
            };
            let warmup = (samples / 10).max(1_000);
            let seed = SEED ^ (depth as u64).wrapping_mul(0x1000_0001);
            let (rec, footprint) = cell(impl_name, depth, &clock, samples, warmup, seed);
            let level = resident_level(footprint, &caches);
            rows.push(Row {
                impl_name,
                depth,
                footprint_bytes: footprint,
                cache_level: level,
                samples: rec.count(),
                overhead_ns: overhead,
                p50_ns: rec.p(0.50),
                p99_ns: rec.p(0.99),
            });
            eprintln!(
                "  {:<6} d={:<8} footprint={:>9} B [{:<4}] p50={:>6}ns p99={:>7}ns",
                impl_name,
                depth,
                footprint,
                level,
                rec.p(0.50),
                rec.p(0.99),
            );
        }
    }

    write_csv(&out_dir.join("cache_experiment.csv"), &rows);
    print_headline(&rows, &caches);
}

fn write_csv(path: &Path, rows: &[Row]) {
    let mut s = String::new();
    s.push_str("impl,depth,footprint_bytes,cache_level_crossed,samples,clock_overhead_ns,p50_ns,p99_ns\n");
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{},{}",
            r.impl_name, r.depth, r.footprint_bytes, r.cache_level, r.samples, r.overhead_ns, r.p50_ns, r.p99_ns,
        );
    }
    std::fs::write(path, s).expect("write cache_experiment.csv");
    eprintln!("wrote {}", path.display());
}

fn print_headline(rows: &[Row], c: &Caches) {
    eprintln!("\n==== CACHE-FOOTPRINT HEADLINE (update p50, ns) ====");
    eprintln!(
        "  boundaries: L1d={} KiB, L2={} KiB, LLC={} KiB (source: /sys)",
        c.l1d / 1024,
        c.l2 / 1024,
        c.llc / 1024
    );
    for &impl_name in &harness::IMPLS {
        let shallow = rows.iter().find(|r| r.impl_name == impl_name).map(|r| r.p50_ns);
        let deep = rows.iter().rev().find(|r| r.impl_name == impl_name).map(|r| (r.depth, r.p50_ns, r.cache_level));
        if let (Some(s), Some((d, p, lvl))) = (shallow, deep) {
            eprintln!("  {impl_name:<6}: p50 {s} ns (shallow) -> {p} ns @ d={d} [{lvl}]");
        }
    }
    eprintln!("  (source: cache_experiment.csv)");
}

fn parse(args: &[String]) -> (usize, PathBuf) {
    let mut core = 0usize;
    let mut out = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--core" => core = it.next().and_then(|s| s.parse().ok()).unwrap_or(core),
            "--out" => {
                if let Some(d) = it.next() {
                    out = PathBuf::from(d);
                }
            }
            _ => {}
        }
    }
    (core, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_handles_suffixes() {
        assert_eq!(parse_size("48K"), 48 * 1024);
        assert_eq!(parse_size("1280K"), 1280 * 1024);
        assert_eq!(parse_size("8M"), 8 * 1024 * 1024);
        assert_eq!(parse_size("4096"), 4096);
    }

    #[test]
    fn resident_level_classifies_boundaries() {
        let c = Caches { l1d: 48 * 1024, l2: 1280 * 1024, llc: 8192 * 1024 };
        assert_eq!(resident_level(4 * 1024, &c), "L1");
        assert_eq!(resident_level(256 * 1024, &c), "L2");
        assert_eq!(resident_level(4 * 1024 * 1024, &c), "LLC");
        assert_eq!(resident_level(32 * 1024 * 1024, &c), "DRAM");
    }

    /// A small cell builds and measures for every impl without panicking, and the
    /// recorder captures the requested samples (elision guard: real work timed).
    #[test]
    fn cell_runs_for_every_impl() {
        let clock = BenchClock::new();
        for &impl_name in &harness::IMPLS {
            let (rec, footprint) = cell(impl_name, 64, &clock, 5_000, 500, 1);
            assert_eq!(rec.count(), 5_000, "{impl_name}: sample count");
            assert!(footprint > 0, "{impl_name}: footprint");
        }
    }
}

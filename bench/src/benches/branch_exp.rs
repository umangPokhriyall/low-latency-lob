//! `bench branch-exp` (§3.1) + the §4 branchless lower-bound experiment.
//!
//! The branch-misprediction signature **without a PMU**: a branchy binary search
//! (`std::partition_point`) is slow *only* on unpredictable (random) keys, while
//! the branchless lower-bound (the comparison lowered to a `cmov`) is flat across
//! key predictability. We measure the 2×2 `{branchy, branchless} × {predictable,
//! random}` over a sorted level array, swept by depth, ≥10M lookups per cell,
//! block-timed against the recorded clock floor. The `branchy/random −
//! branchy/predictable` gap *is* the misprediction penalty, measured with no
//! hardware counters; the flatness of `branchless` proves the data-dependent
//! branch was the cause. Writes `branch_experiment.csv`.
//!
//! Framing (stated in full in `docs/PROFILING.md`): the frozen `SortedVecBook` is
//! **not** changed. The branchless variant is a `bench`-local function — the
//! quantified instruction-level alternative — and `FlatBook`'s direct index is
//! the structural branchless answer the real-data verdict already chose.

// Per-lookup latency is derived by dividing a block-total `u64` ns reading by the
// block length: the values are single- to triple-digit nanoseconds, far inside
// f64's exact-integer range, so these conversions lose nothing in practice.
#![allow(clippy::cast_precision_loss)]

use crate::clock::BenchClock;
use crate::harness;
use crate::recorder::Recorder;
use feed::rng::SplitMix64;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// Sorted-array depths swept (array length, one side). Spans small→large fan-out;
/// every cell fits comfortably in cache, so this isolates *branch* behavior from
/// the cache-footprint axis (that is `cache-exp`'s job).
const DEPTHS: [usize; 6] = [16, 64, 256, 1024, 4096, 16384];
/// Lookups per cell (≥10M per §3.1/§4).
const TARGET_LOOKUPS: u64 = 10_000_000;
/// Lookups timed under one clock bracket; the per-op latency is the block delta
/// divided by `BLOCK`, which amortizes the read-read floor over many searches.
const BLOCK: usize = 1024;
/// Base seed for the random key stream.
const SEED: u64 = 0xB1A5_C0DE_F00D_1234;

/// Branchless `lower_bound` over a sorted slice (§4). The comparison drives `base`
/// through [`std::hint::select_unpredictable`], which lowers to a conditional move
/// (`cmov`), so there is **no data-dependent control-flow branch** to mispredict
/// and latency is independent of key predictability. (Experiment only — the frozen
/// `SortedVecBook` is unchanged.)
///
/// The spec's illustrative form wrote the step as `base = if arr[mid] < key { mid }
/// else { base }` with the note "cmov, not a branch". On this toolchain (rustc
/// 1.95, `target-cpu=native`) LLVM lowers *both* that ternary **and** the
/// arithmetic-select rewrite (`base += (cmp as usize) * half`) back to a
/// conditional jump — the if-conversion is undone — so a hand-written safe-stable
/// branchless search is not actually branchless. `select_unpredictable` is the
/// intrinsic that pins the `cmov` (it is exactly what `std`'s own `partition_point`
/// uses, which is why the [`Variant::Std`] reference is also flat). The only branch
/// left here is the `arr[mid]` bounds check, whose outcome is invariant (always in
/// range) and so never mispredicts.
//
// `select_unpredictable` is stable since Rust 1.88; the frozen library crates keep
// the workspace MSRV (1.85), but this host-specific Phase 9 harness builds on the
// installed toolchain (1.95), so the one use is allowed rather than raising the
// declared MSRV (which would un-gate unrelated lints across the bench tree).
#[allow(clippy::incompatible_msrv)]
#[inline]
fn branchless_lower_bound(arr: &[i64], key: i64) -> usize {
    let mut base = 0usize;
    let mut len = arr.len();
    while len > 1 {
        let half = len / 2;
        let mid = base + half;
        base = std::hint::select_unpredictable(arr[mid] < key, mid, base); // cmov
        len -= half;
    }
    base + usize::from(arr.get(base).is_some_and(|&v| v < key))
}

/// Genuinely **branchy** `lower_bound`: a textbook control-flow binary search whose
/// `if arr[mid] < key { .. } else { .. }` is a data-dependent conditional jump.
/// On random keys ~half its comparisons mispredict (the penalty the §3.1 signature
/// isolates); on predictable keys the branch is learned and the penalty vanishes.
/// (`std::partition_point` is *not* branchy on this toolchain — it is already
/// branchless — so an explicit control-flow search is the honest branchy baseline;
/// the [`Variant::Std`] reference variant measures `partition_point` to show it.)
#[inline]
fn branchy_lower_bound(arr: &[i64], key: i64) -> usize {
    let mut lo = 0usize;
    let mut hi = arr.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if arr[mid] < key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Explicit control-flow binary search ([`branchy_lower_bound`]).
    Branchy,
    /// Arithmetic-select branchless search ([`branchless_lower_bound`], §4).
    Branchless,
    /// `std::partition_point` reference — branchless on this toolchain.
    Std,
}

impl Variant {
    fn tag(self) -> &'static str {
        match self {
            Variant::Branchy => "branchy",
            Variant::Branchless => "branchless",
            Variant::Std => "std",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyPattern {
    Predictable,
    Random,
}

impl KeyPattern {
    fn tag(self) -> &'static str {
        match self {
            KeyPattern::Predictable => "predictable",
            KeyPattern::Random => "random",
        }
    }
}

/// One CSV row: a measured 2×2 cell at a depth.
#[derive(Debug)]
struct Row {
    variant: Variant,
    pattern: KeyPattern,
    depth: usize,
    samples: u64,
    overhead_ns: u64,
    p50_ns: f64,
    p99_ns: f64,
    mean_ns: f64,
}

/// Build the sorted level array for a depth: ascending `i64` keys spaced by 2
/// (the workload tick step), so a lookup key domain of `[-1, 2*depth]` covers
/// hits, misses below, and misses above.
fn level_array(depth: usize) -> Vec<i64> {
    (0..depth).map(|i| i64::try_from(i).expect("depth index fits i64") * 2).collect()
}

/// Measure one 2×2 cell. Keys are generated into a reusable block buffer
/// *untimed*; only the lookups are bracketed by the clock. Inputs and outputs are
/// `black_box`ed so the optimizer cannot hoist or elide the search.
fn measure_cell(clock: &BenchClock, arr: &[i64], variant: Variant, pattern: KeyPattern, seed: u64) -> (Recorder, u64) {
    let blocks = TARGET_LOOKUPS.div_ceil(BLOCK as u64);
    let domain = u64::try_from(arr.len()).expect("len fits u64") * 2 + 2; // [-1 .. 2*depth]
    let mut rec = Recorder::new();
    let mut keybuf = vec![0i64; BLOCK];
    let mut rng = SplitMix64::new(seed);
    let mut seq = 0u64; // monotone walk for the predictable pattern
    let mut acc = 0usize;

    for _ in 0..blocks {
        // Fill the key block UNTIMED so RNG / index arithmetic stays out of the
        // bracket. Predictable = a sequential sweep over the key domain (the
        // comparison branch is learned); random = uniform over the domain.
        match pattern {
            KeyPattern::Predictable => {
                for k in &mut keybuf {
                    *k = i64::try_from(seq % domain).expect("key in domain") - 1;
                    seq = seq.wrapping_add(1);
                }
            }
            KeyPattern::Random => {
                for k in &mut keybuf {
                    *k = i64::try_from(rng.next_u64() % domain).expect("key in domain") - 1;
                }
            }
        }
        let t0 = clock.raw();
        for &key in &keybuf {
            let key = black_box(key);
            let idx = match variant {
                Variant::Branchy => branchy_lower_bound(arr, key),
                Variant::Branchless => branchless_lower_bound(arr, key),
                Variant::Std => arr.partition_point(|&v| v < key),
            };
            acc = acc.wrapping_add(black_box(idx));
        }
        let t1 = clock.raw();
        rec.record(clock.delta_ns(t0, t1));
    }
    black_box(acc);
    (rec, blocks * BLOCK as u64)
}

/// Per-op nanoseconds at quantile `q`: the block-total percentile divided by the
/// block length (block-averaged per-lookup latency).
fn per_op(rec: &Recorder, q: f64) -> f64 {
    rec.p(q) as f64 / BLOCK as f64
}

pub fn run(args: &[String]) {
    let (core, out_dir) = parse(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");
    let clock = BenchClock::new();
    let pinned = harness::pin_to_core(core);
    let overhead = clock.overhead_ns();
    eprintln!(
        "branch-exp: target_lookups/cell={TARGET_LOOKUPS} block={BLOCK} core={core} (pinned={pinned}) clock_overhead_ns={overhead}"
    );

    let mut rows: Vec<Row> = Vec::new();
    for &depth in &DEPTHS {
        let arr = level_array(depth);
        for variant in [Variant::Branchy, Variant::Branchless, Variant::Std] {
            for pattern in [KeyPattern::Predictable, KeyPattern::Random] {
                let seed = SEED ^ (depth as u64).wrapping_mul(0x9E37_79B9) ^ ((variant as u64) << 16) ^ ((pattern as u64) << 8);
                let (rec, samples) = measure_cell(&clock, &arr, variant, pattern, seed);
                rows.push(Row {
                    variant,
                    pattern,
                    depth,
                    samples,
                    overhead_ns: overhead,
                    p50_ns: per_op(&rec, 0.50),
                    p99_ns: per_op(&rec, 0.99),
                    mean_ns: rec.mean() / BLOCK as f64,
                });
            }
        }
        // Live read of the misprediction signature at this depth.
        let g = |v: Variant, p: KeyPattern| {
            rows.iter()
                .find(|r| r.depth == depth && r.variant == v && r.pattern == p)
                .map_or(0.0, |r| r.p50_ns)
        };
        eprintln!(
            "  d={:<6} branchy[pred={:>6.2} rand={:>6.2} Δ={:>6.2}]  branchless[pred={:>6.2} rand={:>6.2}] (p50 ns/lookup)",
            depth,
            g(Variant::Branchy, KeyPattern::Predictable),
            g(Variant::Branchy, KeyPattern::Random),
            g(Variant::Branchy, KeyPattern::Random) - g(Variant::Branchy, KeyPattern::Predictable),
            g(Variant::Branchless, KeyPattern::Predictable),
            g(Variant::Branchless, KeyPattern::Random),
        );
    }

    write_csv(&out_dir.join("branch_experiment.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    let mut s = String::new();
    s.push_str("variant,key_pattern,depth,samples,clock_overhead_ns,p50_ns,p99_ns,mean_ns\n");
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{:.3},{:.3},{:.3}",
            r.variant.tag(),
            r.pattern.tag(),
            r.depth,
            r.samples,
            r.overhead_ns,
            r.p50_ns,
            r.p99_ns,
            r.mean_ns,
        );
    }
    std::fs::write(path, s).expect("write branch_experiment.csv");
    eprintln!("wrote {}", path.display());
}

/// The 2×2 headline at the deepest swept depth: the misprediction penalty and the
/// branchless flatness, sourced to `branch_experiment.csv`.
fn print_headline(rows: &[Row]) {
    let depth = *DEPTHS.last().unwrap();
    let g = |v: Variant, p: KeyPattern| {
        rows.iter().find(|r| r.depth == depth && r.variant == v && r.pattern == p).map_or(0.0, |r| r.p50_ns)
    };
    let bp = g(Variant::Branchy, KeyPattern::Predictable);
    let br = g(Variant::Branchy, KeyPattern::Random);
    let lp = g(Variant::Branchless, KeyPattern::Predictable);
    let lr = g(Variant::Branchless, KeyPattern::Random);
    eprintln!("\n==== MISPREDICTION 2x2 HEADLINE (p50 ns/lookup, depth={depth}) ====");
    eprintln!("  branchy    : predictable={bp:.2}  random={br:.2}  -> penalty {:.2} ns", br - bp);
    eprintln!("  branchless : predictable={lp:.2}  random={lr:.2}  -> spread  {:.2} ns", (lr - lp).abs());
    eprintln!("  (source: branch_experiment.csv)");
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

    /// §4 correctness: the branchless lower-bound matches `std`'s `partition_point`
    /// on randomized sorted inputs (varied lengths, duplicates, keys spanning
    /// below/within/above the array).
    #[test]
    fn branchless_lower_bound_matches_partition_point() {
        let mut rng = SplitMix64::new(0xBADC_0FFE_E0DD_F00D);
        for _ in 0..3000 {
            let len = usize::try_from(rng.below(96) + 1).expect("len fits usize"); // 1..=96
            let mut v: Vec<i64> =
                (0..len).map(|_| i64::try_from(rng.next_u64() % 200).expect("fits i64") - 100).collect();
            v.sort_unstable();
            for _ in 0..24 {
                let key = i64::try_from(rng.next_u64() % 260).expect("fits i64") - 130; // below/within/above
                let expect = v.partition_point(|&x| x < key);
                let got = branchless_lower_bound(&v, key);
                assert_eq!(got, expect, "mismatch: arr={v:?} key={key}");
            }
        }
        // Degenerate cases.
        assert_eq!(branchless_lower_bound(&[], 5), 0);
        assert_eq!(branchless_lower_bound(&[7], 7), 0);
        assert_eq!(branchless_lower_bound(&[7], 8), 1);
    }

    /// All three variants (branchy control-flow, branchless arithmetic-select, and
    /// `std::partition_point`) compute the identical lower-bound for every key on a
    /// fixed level array — they differ only in microarchitectural behavior.
    #[test]
    fn all_variants_agree_on_level_array() {
        let arr = level_array(256);
        for key in -2..=520 {
            let std_idx = arr.partition_point(|&v| v < key);
            assert_eq!(branchy_lower_bound(&arr, key), std_idx, "branchy key={key}");
            assert_eq!(branchless_lower_bound(&arr, key), std_idx, "branchless key={key}");
        }
    }
}

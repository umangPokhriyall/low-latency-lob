//! Construct a book at a target depth and generate touches at a controlled
//! locality. The depth ladder and touch locality are the two axes of the §5
//! crossover sweep; everything here is deterministic given a seed.

use book::{BookEvent, OrderBook, Px, Qty, Side};
use feed::rng::SplitMix64;

/// Tick spacing between adjacent resident levels. Strictly `>1` so an INSERT can
/// target an in-band **gap** price (an odd tick offset, `existing ± 1`) that
/// collides with no resident (even-offset) level — letting Benchmark 1 isolate
/// insert/remove memmove cost from locate cost.
pub const STEP: i64 = 2;

/// Touch locality: where in the ladder updates land.
/// `Concentrated` is top-of-book-biased (the realistic case); `Uniform` spreads
/// flat across the whole depth (the adversarial, deep-search case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locality {
    Concentrated,
    Uniform,
}

impl Locality {
    /// Lower-case tag for CSV rows / filenames.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Locality::Concentrated => "concentrated",
            Locality::Uniform => "uniform",
        }
    }
}

/// Build a book with exactly `depth` levels per side around `mid` (untimed).
/// Bids occupy `mid-STEP .. mid-STEP*depth` (descending away from mid); asks
/// occupy `mid+STEP .. mid+STEP*depth`. Offset 0 (the best level) is nearest mid.
#[must_use]
pub fn build_at_depth<B: OrderBook>(mid: Px, depth: usize) -> B {
    let mut b = B::default();
    let d = i64::try_from(depth).expect("depth fits i64");
    for i in 1..=d {
        b.apply(&BookEvent::level(0, 0, Side::Bid, Px(mid.0 - STEP * i), Qty(1_000 + i)));
        b.apply(&BookEvent::level(0, 0, Side::Ask, Px(mid.0 + STEP * i), Qty(1_000 + i)));
    }
    b
}

/// Build a book with `depth` levels per side around `mid`, applying levels in
/// globally **ascending** price order (lowest bid up to highest ask) so the
/// contiguous binary-search Vec builds in O(N log N) — every new price appends
/// at the end of the sorted run — rather than the O(N²) front-insert order
/// [`build_at_depth`] produces. The final book is byte-for-byte the state
/// [`build_at_depth`] yields (a level set is order-independent); only the build
/// cost differs. This lets the Phase 9 cache-footprint sweep reach the deep,
/// LLC-crossing regime for the order-sensitive impls.
///
/// `RevVecBook` is O(N²) to build under *any* order — its locate is a linear
/// scan from the best end, which has no cheap bulk-load — so the cache sweep
/// caps its depth rather than calling this for arbitrarily large `depth`.
#[must_use]
pub fn build_at_depth_fast<B: OrderBook>(mid: Px, depth: usize) -> B {
    let mut b = B::default();
    let d = i64::try_from(depth).expect("depth fits i64");
    // Bids: lowest price (mid - STEP*depth) first, ascending up to mid - STEP.
    for i in (1..=d).rev() {
        b.apply(&BookEvent::level(0, 0, Side::Bid, Px(mid.0 - STEP * i), Qty(1_000 + i)));
    }
    // Asks: lowest price (mid + STEP) first, ascending up to mid + STEP*depth.
    for i in 1..=d {
        b.apply(&BookEvent::level(0, 0, Side::Ask, Px(mid.0 + STEP * i), Qty(1_000 + i)));
    }
    b
}

/// Offset-from-best (0 = best, nearest mid) of an EXISTING level, drawn per
/// `Locality`. `Uniform`: flat over `0..depth`. `Concentrated`: geometric via
/// fair coin flips — `P(offset=k) ≈ 2^-(k+1)`, so mostly offsets 0..3 — capped
/// at `depth-1`. Exposed for the locality-distribution unit test.
#[must_use]
pub fn touch_offset(rng: &mut SplitMix64, depth: usize, loc: Locality) -> usize {
    debug_assert!(depth >= 1);
    match loc {
        Locality::Uniform => {
            usize::try_from(rng.below(depth as u64)).expect("offset fits usize")
        }
        Locality::Concentrated => {
            let mut k = 0usize;
            while k + 1 < depth && (rng.next_u64() & 1) == 0 {
                k += 1;
            }
            k
        }
    }
}

/// The price of an EXISTING level at an offset drawn per `Locality`.
/// `side` selects which ladder; offset 0 is the side's best (nearest mid).
#[must_use]
pub fn touch_price(rng: &mut SplitMix64, mid: Px, depth: usize, side: Side, loc: Locality) -> Px {
    let off = i64::try_from(touch_offset(rng, depth, loc)).expect("offset fits i64");
    match side {
        Side::Bid => Px(mid.0 - STEP * (off + 1)),
        Side::Ask => Px(mid.0 + STEP * (off + 1)),
    }
}

/// An in-band GAP price adjacent to an existing level `existing` on `side`:
/// one tick toward mid, an odd offset that no resident level occupies. Inserting
/// here triggers a memmove proportional to the level's depth position; removing
/// it restores the book to its original depth.
#[must_use]
pub fn gap_price(existing: Px, side: Side) -> Px {
    match side {
        Side::Bid => Px(existing.0 + 1), // toward mid
        Side::Ask => Px(existing.0 - 1), // toward mid
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use book::{OrderBook, RevVecBook};

    #[test]
    fn build_at_depth_produces_exact_depth_both_sides() {
        for &depth in &[1usize, 2, 8, 64, 512] {
            let b: RevVecBook = build_at_depth(Px(1_000_000), depth);
            assert_eq!(b.depth(Side::Bid), depth);
            assert_eq!(b.depth(Side::Ask), depth);
            // Best bid is the level nearest mid (offset 0).
            assert_eq!(b.best_bid().map(|(p, _)| p), Some(Px(1_000_000 - STEP)));
            assert_eq!(b.best_ask().map(|(p, _)| p), Some(Px(1_000_000 + STEP)));
        }
    }

    /// The fast ascending builder yields a book observationally identical to the
    /// canonical [`build_at_depth`]: same depth, best, and full ladder on both
    /// sides. (Across all four impls the final level set is order-independent.)
    #[test]
    fn build_fast_matches_build_at_depth() {
        use book::{BTreeBook, FlatBook, OrderBook, SortedVecBook};

        fn ladder<B: OrderBook>(b: &B, side: Side) -> Vec<(Px, Qty)> {
            let mut out = vec![(Px::ZERO, Qty::ZERO); b.depth(side)];
            let n = b.top_n(side, &mut out);
            out.truncate(n);
            out
        }
        fn check<B: OrderBook>(mid: Px, depth: usize) {
            let slow: B = build_at_depth(mid, depth);
            let fast: B = build_at_depth_fast(mid, depth);
            for side in [Side::Bid, Side::Ask] {
                assert_eq!(slow.depth(side), depth);
                assert_eq!(fast.depth(side), depth);
                assert_eq!(ladder(&slow, side), ladder(&fast, side), "ladder mismatch d={depth}");
            }
            assert_eq!(slow.best_bid(), fast.best_bid());
            assert_eq!(slow.best_ask(), fast.best_ask());
        }

        let mid = Px(1_000_000);
        for &depth in &[1usize, 2, 7, 64, 300] {
            check::<SortedVecBook>(mid, depth);
            check::<RevVecBook>(mid, depth);
            check::<BTreeBook>(mid, depth);
            check::<FlatBook>(mid, depth);
        }
    }

    #[test]
    fn gap_price_is_an_unoccupied_in_band_price() {
        let mid = Px(1_000_000);
        let b: RevVecBook = build_at_depth(mid, 16);
        // The gap adjacent to the best bid is one tick toward mid, not resident.
        let best = touch_price(&mut SplitMix64::new(0), mid, 16, Side::Bid, Locality::Concentrated);
        let gap = gap_price(best, Side::Bid);
        assert_eq!(gap, Px(mid.0 - STEP + 1));
        // Inserting the gap grows depth by exactly one; removing restores it.
        let mut b = b;
        let before = b.depth(Side::Bid);
        b.apply(&BookEvent::level(0, 0, Side::Bid, gap, Qty(5)));
        assert_eq!(b.depth(Side::Bid), before + 1);
        b.apply(&BookEvent::level(0, 0, Side::Bid, gap, Qty(0)));
        assert_eq!(b.depth(Side::Bid), before);
    }

    /// The locality generator produces the intended offset distribution:
    /// `Uniform` is flat across `0..depth`; `Concentrated` is top-biased with a
    /// mode at 0 and the bulk within offsets 0..3.
    #[test]
    fn locality_generator_offset_distribution() {
        const DEPTH: usize = 64;
        const N: u64 = 400_000;

        // Uniform: every bucket gets ~N/DEPTH; none is empty or dominant.
        let mut rng = SplitMix64::new(0xD157_5EED);
        let mut uni = vec![0u64; DEPTH];
        for _ in 0..N {
            uni[touch_offset(&mut rng, DEPTH, Locality::Uniform)] += 1;
        }
        let expect = N / DEPTH as u64;
        for (k, &c) in uni.iter().enumerate() {
            assert!(c > expect / 2, "uniform bucket {k} underfilled: {c}");
            assert!(c < expect * 2, "uniform bucket {k} overfilled: {c}");
        }

        // Concentrated: offset 0 is the mode (~50%), and ≥90% land in 0..=3.
        let mut rng = SplitMix64::new(0x00C0_FFEE);
        let mut con = vec![0u64; DEPTH];
        for _ in 0..N {
            con[touch_offset(&mut rng, DEPTH, Locality::Concentrated)] += 1;
        }
        let frac0 = con[0] as f64 / N as f64;
        let head: u64 = con[0..4].iter().sum();
        let frac_head = head as f64 / N as f64;
        assert!((0.45..0.55).contains(&frac0), "P(offset=0) = {frac0}, expected ~0.5");
        assert!(frac_head > 0.90, "P(offset<=3) = {frac_head}, expected > 0.9");

        // And the concentrated mean offset is far below the uniform mean.
        let mean_con: f64 = con.iter().enumerate().map(|(k, &c)| k as f64 * c as f64).sum::<f64>() / N as f64;
        assert!(mean_con < 2.0, "concentrated mean offset = {mean_con}, expected < 2");
    }
}

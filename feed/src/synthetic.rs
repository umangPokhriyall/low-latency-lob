//! Deterministic synthetic load generator. Produces tick-space `BookEvent`
//! streams under three controllable profiles (steady / burst / flash-crash) so
//! Phase 4 can sweep the four book impls against repeatable, seed-derived input.
//!
//! No floats, no async, no I/O. Generation allocates the output `Vec` once at
//! setup; the replay hot path downstream stays zero-alloc. Identical `GenConfig`
//! → byte-identical corpus (the reproducibility contract, §7).

use crate::rng::SplitMix64;
use book::{BookEvent, Px, Qty, Side};
use std::collections::BTreeMap;

/// Bumped whenever the generator's output for a fixed config changes. Recorded
/// in each synthetic corpus's `.meta.json` provenance sidecar.
pub const GENERATOR_VERSION: u32 = 1;

// --- profile tuning (all integer ticks / nanoseconds) ------------------------
/// Levels around the touch that `Steady` concentrates updates in (geometric).
const STEADY_DEPTH: i64 = 8;
/// Baseline inter-event interval (ns) and its jitter for calm/steady flow.
const STEADY_DT: u64 = 1_000;
const STEADY_JITTER: u64 = 250;
/// Percent of steady events that are trades; the next slice is edge churn.
const TRADE_PCT: u64 = 5;
const CHURN_PCT: u64 = 1;
/// Calm-stretch inter-event interval (ns) between bursts / before a crash.
const CALM_DT: u64 = 50_000;

/// Which load shape `generate` emits. See §5.3 for the stress each applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Steady,
    Burst,
    FlashCrash,
}

/// Fully determines a synthetic stream: identical config → identical events.
#[derive(Clone, Debug)]
pub struct GenConfig {
    pub profile: Profile,
    pub seed: u64,
    pub events: usize,
    /// Starting mid price (ticks); the book is seeded symmetrically around it.
    pub mid: Px,
    /// Half-width of the active price band (ticks); the touch never leaves it.
    pub band: i64,
    pub max_qty: Qty,
    /// First event timestamp (nanoseconds); `ts` only advances from here.
    pub start_ts: u64,
}

/// Deterministic generator: identical `GenConfig` → identical `Vec<BookEvent>`.
///
/// Emits exactly `cfg.events` events (a leading `Clear`, a seeded ladder, then
/// profile-specific flow), stopping precisely at the requested count.
#[must_use]
pub fn generate(cfg: &GenConfig) -> Vec<BookEvent> {
    let mut sim = Sim::new(cfg);
    sim.seed();
    while !sim.full() {
        match cfg.profile {
            Profile::Steady => {
                sim.steady_event();
                sim.advance_steady();
            }
            Profile::Burst => sim.burst_cycle(),
            Profile::FlashCrash => sim.crash_cycle(),
        }
    }
    sim.out
}

/// The light internal book model the generator drives so emitted events resemble
/// a real feed (updates/removals target existing or adjacent levels).
struct Sim {
    rng: SplitMix64,
    seq: u64,
    ts: u64,
    mid: i64,
    band: i64,
    max_qty: i64,
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    out: Vec<BookEvent>,
    target: usize,
}

impl Sim {
    fn new(cfg: &GenConfig) -> Self {
        Self {
            rng: SplitMix64::new(cfg.seed),
            seq: 0,
            ts: cfg.start_ts,
            mid: cfg.mid.ticks(),
            band: cfg.band,
            max_qty: cfg.max_qty.lots(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            out: Vec::with_capacity(cfg.events),
            target: cfg.events,
        }
    }

    fn full(&self) -> bool {
        self.out.len() >= self.target
    }

    fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }

    // --- emitters: each pushes exactly one event unless the target is reached -

    fn level(&mut self, side: Side, px: i64, qty: i64) {
        if self.full() {
            return;
        }
        let book = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        if qty == 0 {
            book.remove(&px);
        } else {
            book.insert(px, qty);
        }
        self.out
            .push(BookEvent::level(self.seq, self.ts, side, Px(px), Qty(qty)));
        self.seq += 1;
    }

    fn trade(&mut self, aggressor: Side, px: i64, qty: i64) {
        if self.full() {
            return;
        }
        self.out
            .push(BookEvent::trade(self.seq, self.ts, aggressor, Px(px), Qty(qty)));
        self.seq += 1;
    }

    fn clear(&mut self) {
        if self.full() {
            return;
        }
        self.bids.clear();
        self.asks.clear();
        self.out.push(BookEvent::clear(self.seq, self.ts));
        self.seq += 1;
    }

    // --- rng helpers (kept in i64 tick-space, no `as` casts) -----------------

    /// Value in `0..n` as `i64`; `0` for non-positive `n`.
    fn roll_i64(&mut self, n: i64) -> i64 {
        let Ok(n) = u64::try_from(n) else { return 0 };
        if n == 0 {
            return 0;
        }
        i64::try_from(self.rng.below(n)).unwrap_or(0)
    }

    /// A positive quantity in `1..=max_qty`.
    fn rand_qty(&mut self) -> i64 {
        let span = self.max_qty.max(1);
        1 + self.roll_i64(span)
    }

    /// Offset from the touch, geometrically concentrated near 1, capped at `cap`.
    fn geo_offset(&mut self, cap: i64) -> i64 {
        let mut off = 1;
        while off < cap && self.rng.below(2) == 0 {
            off += 1;
        }
        off
    }

    /// Aggregate quantity for a seeded ladder level: geometric decay from touch.
    fn decay_qty(&self, offset: i64) -> i64 {
        let shift = u32::try_from(offset - 1).unwrap_or(0).min(6);
        (self.max_qty >> shift).max(1)
    }

    fn advance_steady(&mut self) {
        self.ts += STEADY_DT + self.rng.below(STEADY_JITTER);
    }

    // --- setup ---------------------------------------------------------------

    /// `Clear`, then a symmetric ladder of `band` levels per side around `mid`.
    fn seed(&mut self) {
        self.clear();
        let mut off = 1;
        while off <= self.band {
            if self.full() {
                return;
            }
            let q = self.decay_qty(off);
            self.level(Side::Bid, self.mid - off, q);
            self.level(Side::Ask, self.mid + off, q);
            off += 1;
        }
    }

    // --- steady flow ---------------------------------------------------------

    /// One steady event: mostly a top-of-book level update, ~5% trades, ~1%
    /// edge churn. Always emits at least one event (progress is guaranteed).
    fn steady_event(&mut self) {
        let r = self.rng.below(100);
        if r < TRADE_PCT && self.try_trade() {
            // a trade was printed
        } else if r < TRADE_PCT + CHURN_PCT {
            self.edge_churn();
        } else {
            self.level_update();
        }
    }

    /// Print a trade at the touch of a random side. Returns `false` (emitting
    /// nothing) only if that side is empty, so the caller can fall back.
    fn try_trade(&mut self) -> bool {
        if self.rng.below(2) == 0 {
            if let Some(bb) = self.best_bid() {
                let q = self.rand_qty();
                self.trade(Side::Ask, bb, q);
                return true;
            }
        } else if let Some(ba) = self.best_ask() {
            let q = self.rand_qty();
            self.trade(Side::Bid, ba, q);
            return true;
        }
        false
    }

    /// Update a level within the top `STEADY_DEPTH`; ~10% are removals.
    fn level_update(&mut self) {
        let side = self.rand_side();
        let off = self.geo_offset(STEADY_DEPTH.min(self.band.max(1)));
        let px = self.px_at(side, off);
        let qty = if self.rng.below(10) == 0 {
            0
        } else {
            self.rand_qty()
        };
        self.level(side, px, qty);
    }

    /// Add or remove a level out near the band edge (rare new-level churn).
    fn edge_churn(&mut self) {
        let side = self.rand_side();
        let off = STEADY_DEPTH + 1 + self.roll_i64(self.band - STEADY_DEPTH);
        let px = self.px_at(side, off);
        if self.rng.below(2) == 0 {
            let q = self.rand_qty();
            self.level(side, px, q);
        } else {
            self.level(side, px, 0);
        }
    }

    fn rand_side(&mut self) -> Side {
        if self.rng.below(2) == 0 {
            Side::Bid
        } else {
            Side::Ask
        }
    }

    fn px_at(&self, side: Side, off: i64) -> i64 {
        match side {
            Side::Bid => self.mid - off,
            Side::Ask => self.mid + off,
        }
    }

    // --- burst flow ----------------------------------------------------------

    /// A calm stretch (large `ts` gaps) followed by a dense burst (hundreds of
    /// events sharing a tight `ts` window). Same spatial locality as steady.
    fn burst_cycle(&mut self) {
        let calm = 20 + self.rng.below(40);
        for _ in 0..calm {
            if self.full() {
                return;
            }
            self.steady_event();
            self.ts += CALM_DT + self.rng.below(CALM_DT);
        }
        let burst = 200 + self.rng.below(120);
        for _ in 0..burst {
            if self.full() {
                return;
            }
            self.steady_event();
            self.ts += self.rng.below(2); // 0 or 1 ns: a tight cluster
        }
    }

    // --- flash-crash flow ----------------------------------------------------

    /// Periodic calm, then a directional cascade: the aggressor sweeps level
    /// after level down the bid ladder within a tight `ts` window (best bid
    /// collapses by many ticks, the struck side thins), then a partial recovery.
    fn crash_cycle(&mut self) {
        let calm = 200 + self.rng.below(200);
        for _ in 0..calm {
            if self.full() {
                return;
            }
            self.steady_event();
            self.advance_steady();
        }

        // Cascade: print a trade taking out the best bid, then wipe that level.
        let sweep = (self.band * 3 / 4).max(20);
        for _ in 0..sweep {
            if self.full() {
                return;
            }
            let Some(bb) = self.best_bid() else { break };
            let q = self.bids.get(&bb).copied().unwrap_or(1);
            self.trade(Side::Ask, bb, q);
            self.level(Side::Bid, bb, 0);
            self.ts += self.rng.below(3); // tight crash window
        }

        // Partial recovery: rebuild bid levels back up toward the mid.
        let recover = (sweep / 2).max(1);
        let mut k = 1;
        while k <= recover {
            if self.full() {
                return;
            }
            let q = self.rand_qty();
            self.level(Side::Bid, self.mid - k, q);
            self.advance_steady();
            k += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Corpus;
    use book::{BTreeBook, OrderBook, RevVecBook, SortedVecBook};

    fn cfg(profile: Profile, events: usize) -> GenConfig {
        GenConfig {
            profile,
            seed: 1,
            events,
            mid: Px(65_000),
            band: 64,
            max_qty: Qty(1_024),
            start_ts: 0,
        }
    }

    fn bytes_of(events: &[BookEvent]) -> Vec<u8> {
        let mut b = Vec::new();
        Corpus::write_to(&mut b, events).expect("write_to");
        b
    }

    #[test]
    fn determinism_byte_identical() {
        for p in [Profile::Steady, Profile::Burst, Profile::FlashCrash] {
            let a = generate(&cfg(p, 5_000));
            let b = generate(&cfg(p, 5_000));
            assert_eq!(bytes_of(&a), bytes_of(&b), "{p:?} not deterministic");
        }
    }

    #[test]
    fn exact_count() {
        for p in [Profile::Steady, Profile::Burst, Profile::FlashCrash] {
            for n in [0usize, 1, 3, 130, 5_000] {
                assert_eq!(generate(&cfg(p, n)).len(), n, "{p:?} n={n}");
            }
        }
    }

    #[test]
    fn ts_is_monotonic_and_seq_is_dense() {
        for p in [Profile::Steady, Profile::Burst, Profile::FlashCrash] {
            let evs = generate(&cfg(p, 5_000));
            for (i, w) in evs.windows(2).enumerate() {
                assert!(w[1].ts >= w[0].ts, "{p:?} ts went backwards at {i}");
            }
            for (i, e) in evs.iter().enumerate() {
                assert_eq!(e.seq, i as u64, "{p:?} seq not dense at {i}");
            }
        }
    }

    #[test]
    fn steady_stays_within_band() {
        let c = cfg(Profile::Steady, 20_000);
        let evs = generate(&c);
        let mut book = BTreeBook::default();
        for e in &evs {
            book.apply(e);
            if let Some((px, _)) = book.best_bid() {
                assert!((px.ticks() - c.mid.ticks()).abs() <= c.band);
            }
            if let Some((px, _)) = book.best_ask() {
                assert!((px.ticks() - c.mid.ticks()).abs() <= c.band);
            }
        }
    }

    #[test]
    fn burst_has_dense_ts_window() {
        const B: usize = 100;
        const W: u64 = 1_000;
        let evs = generate(&cfg(Profile::Burst, 5_000));
        let found = evs.windows(B).any(|w| w[B - 1].ts - w[0].ts <= W);
        assert!(found, "no window of {B} events within {W}ns");
    }

    #[test]
    fn flashcrash_collapses_bid() {
        const K: i64 = 20; // best bid falls at least this many ticks
        const D: usize = 10; // bid depth contracts by at least this many levels
        const W: u64 = 5_000; // within this ns window
        let evs = generate(&cfg(Profile::FlashCrash, 20_000));
        let mut book = BTreeBook::default();
        let mut bb = Vec::with_capacity(evs.len());
        let mut depth = Vec::with_capacity(evs.len());
        let mut ts = Vec::with_capacity(evs.len());
        for e in &evs {
            book.apply(e);
            bb.push(book.best_bid().map(|(p, _)| p.ticks()));
            depth.push(book.depth(Side::Bid));
            ts.push(e.ts);
        }

        let mut found = false;
        'scan: for i in 0..evs.len() {
            let Some(hi) = bb[i] else { continue };
            let (di, ti) = (depth[i], ts[i]);
            for j in (i + 1)..evs.len() {
                if ts[j] - ti > W {
                    break;
                }
                if let Some(lo) = bb[j] {
                    if hi - lo >= K && di.saturating_sub(depth[j]) >= D {
                        found = true;
                        break 'scan;
                    }
                }
            }
        }
        assert!(found, "no flash-crash collapse within the window");
    }

    #[test]
    fn tri_impl_replay_agrees() {
        for p in [Profile::Steady, Profile::Burst, Profile::FlashCrash] {
            let evs = generate(&cfg(p, 10_000));
            let mut bt = BTreeBook::default();
            let mut sv = SortedVecBook::default();
            let mut rv = RevVecBook::default();
            for e in &evs {
                bt.apply(e);
                sv.apply(e);
                rv.apply(e);
            }
            assert_eq!(bt.best_bid(), sv.best_bid(), "{p:?} best_bid bt/sv");
            assert_eq!(bt.best_bid(), rv.best_bid(), "{p:?} best_bid bt/rv");
            assert_eq!(bt.best_ask(), sv.best_ask(), "{p:?} best_ask bt/sv");
            assert_eq!(bt.best_ask(), rv.best_ask(), "{p:?} best_ask bt/rv");
            for side in [Side::Bid, Side::Ask] {
                assert_eq!(bt.depth(side), sv.depth(side), "{p:?} depth bt/sv");
                assert_eq!(bt.depth(side), rv.depth(side), "{p:?} depth bt/rv");
            }
        }
    }
}

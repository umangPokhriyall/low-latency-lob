//! Deterministic, seedable PRNG for reproducible synthetic corpora. Public so the
//! `gen` binary and Phase 4 can regenerate identical streams from a seed.
//!
//! An independent copy of the standard `SplitMix64` algorithm (the `book` test
//! PRNG is not importable — `book` is frozen and its test module is private).
//! Here it is a real, public library feature, not duplicated test scaffolding.

/// `SplitMix64`: a fast, fully deterministic 64-bit generator. Identical seeds
/// produce identical streams on every machine — the basis of corpus determinism.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Advance the state and return the next 64-bit value.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish value in `0..n`. `n` must be non-zero (all callers pass `n > 0`).
    #[must_use]
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_stream() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(1);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn distinct_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn below_is_in_range() {
        let mut r = SplitMix64::new(42);
        for _ in 0..1000 {
            assert!(r.below(10) < 10);
        }
    }
}

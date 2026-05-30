//! Fixed-point tick types: `Px` (price in ticks) and `Qty` (quantity in lots).
//!
//! Representation is **locked in Phase 0** and unchanged by this relocation:
//! `#[repr(transparent)]` newtypes over `i64`, ABI-identical to `i64`, genuinely
//! zero-cost. No floating point ever enters this crate (see crate-root invariants).

use core::ops::{Add, AddAssign, Sub, SubAssign};

/// Price as an integer number of ticks (the symbol's minimum price increment).
/// `repr(transparent)` => ABI-identical to `i64`, a genuinely zero-cost newtype.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Px(pub i64);

/// Quantity as an integer number of lots (the symbol's minimum size increment).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Qty(pub i64);

impl Px {
    pub const ZERO: Px = Px(0);
    #[inline] #[must_use] pub const fn ticks(self) -> i64 { self.0 }
    /// Signed tick distance `self - other` (positive when `self` is the higher price).
    #[inline] #[must_use] pub const fn diff(self, other: Px) -> i64 { self.0 - other.0 }
}

impl Qty {
    pub const ZERO: Qty = Qty(0);
    #[inline] #[must_use] pub const fn lots(self) -> i64 { self.0 }
    #[inline] #[must_use] pub const fn is_zero(self) -> bool { self.0 == 0 }
}

impl Add<i64> for Px { type Output = Px; #[inline] fn add(self, t: i64) -> Px { Px(self.0 + t) } }
impl Sub<i64> for Px { type Output = Px; #[inline] fn sub(self, t: i64) -> Px { Px(self.0 - t) } }
impl Add for Qty { type Output = Qty; #[inline] fn add(self, r: Qty) -> Qty { Qty(self.0 + r.0) } }
impl Sub for Qty { type Output = Qty; #[inline] fn sub(self, r: Qty) -> Qty { Qty(self.0 - r.0) } }
impl AddAssign for Qty { #[inline] fn add_assign(&mut self, r: Qty) { self.0 += r.0; } }
impl SubAssign for Qty { #[inline] fn sub_assign(&mut self, r: Qty) { self.0 -= r.0; } }

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn px_is_zero_cost_over_i64() {
        assert_eq!(size_of::<Px>(), size_of::<i64>());
        assert_eq!(size_of::<Qty>(), size_of::<i64>());
        assert_eq!(size_of::<Option<Px>>(), size_of::<i64>() * 2); // sanity on layout
    }

    #[test]
    fn px_orders_like_an_integer() {
        assert!(Px(100) > Px(99));
        assert!(Px(-1) < Px(0));
        let mut v = [Px(3), Px(1), Px(2)];
        v.sort_unstable();
        assert_eq!(v, [Px(1), Px(2), Px(3)]);
    }

    #[test]
    fn px_diff_is_signed() {
        assert_eq!(Px(105).diff(Px(100)), 5);
        assert_eq!(Px(100).diff(Px(105)), -5);
        assert_eq!(Px(100).diff(Px(100)), 0);
    }

    #[test]
    fn px_tick_arithmetic() {
        assert_eq!(Px(100) + 5, Px(105));
        assert_eq!(Px(100) - 5, Px(95));
    }

    #[test]
    fn qty_arithmetic_and_zero() {
        assert!(Qty::ZERO.is_zero());
        let mut q = Qty(10);
        q += Qty(5);
        assert_eq!(q, Qty(15));
        q -= Qty(15);
        assert!(q.is_zero());
    }
}

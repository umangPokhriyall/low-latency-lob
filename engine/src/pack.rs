//! `BookEvent` <-> `[u64; W]` packing for the SPMC ring (engine-owned, Phase 8).
//!
//! The ring carries opaque atomic words; the engine is the layer that decides what
//! a record *means*. Packing mirrors the corpus boundary: the producer serialises a
//! fixed `[u64; W]`, and the consumer **validates the discriminants at unpack**
//! before trusting the bytes — exactly as the recorder validated the float-string
//! edge before the corpus. No `transmute`, no `unsafe`; this is explicit integer
//! bit-manipulation, so a corrupt word becomes a typed [`UnpackError`], never UB.

use book::{BookEvent, EventKind, Px, Qty, Side};

/// Words per ring record. `size_of::<BookEvent>() == 40 == 5 * 8`, so a `BookEvent`
/// serialises into exactly five `u64` words with no padding to carry.
pub const W: usize = 5;

const _: () = assert!(size_of::<BookEvent>() == W * 8);

/// Serialise a `BookEvent` into five words. Hot path: pure arithmetic, no alloc.
///
/// Word layout: `[seq, ts, px-bits, qty-bits, side|(kind<<8)]`. `px`/`qty` are
/// stored as their `i64` bit patterns (reversed losslessly at unpack); the
/// discriminant byte packs `side` in bits 0..8 and `kind` in bits 8..16.
#[inline]
#[must_use]
#[allow(clippy::cast_sign_loss)] // i64 -> u64 bit pattern, reversed exactly at unpack
pub fn pack(ev: &BookEvent) -> [u64; W] {
    [
        ev.seq,
        ev.ts,
        ev.px.ticks() as u64, // i64 bit pattern
        ev.qty.lots() as u64,
        (ev.side as u64) | ((ev.kind as u64) << 8),
    ]
}

/// Why a record failed to unpack: an out-of-range `side` or `kind` discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpackError {
    /// The low discriminant byte was not a valid [`Side`].
    BadSide(u64),
    /// The high discriminant byte was not a valid [`EventKind`].
    BadKind(u64),
}

/// Deserialise five words back into a `BookEvent`, validating the discriminants.
///
/// # Errors
/// Returns [`UnpackError::BadSide`] / [`UnpackError::BadKind`] if the discriminant
/// byte holds a value no `repr(u8)` variant maps to. The engine producer only ever
/// emits valid records, so on the hot path this is the typed proof that the ring
/// round-trip stayed intact, not an expected branch.
#[allow(clippy::cast_possible_wrap)] // u64 -> i64 reverses the lossless pack cast
pub fn unpack(rec: &[u64; W]) -> Result<BookEvent, UnpackError> {
    let side = match rec[4] & 0xFF {
        0 => Side::Bid,
        1 => Side::Ask,
        b => return Err(UnpackError::BadSide(b)),
    };
    let kind = match (rec[4] >> 8) & 0xFF {
        0 => EventKind::Level,
        1 => EventKind::Trade,
        2 => EventKind::Clear,
        b => return Err(UnpackError::BadKind(b)),
    };
    Ok(BookEvent {
        seq: rec[0],
        ts: rec[1],
        px: Px(rec[2] as i64),
        qty: Qty(rec[3] as i64),
        side,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Vec<BookEvent> {
        vec![
            BookEvent::level(0, 100, Side::Bid, Px(65_000), Qty(12)),
            BookEvent::level(1, 200, Side::Ask, Px(-7), Qty(0)),
            BookEvent::trade(2, 300, Side::Ask, Px(64_999), Qty(5)),
            BookEvent::trade(3, 400, Side::Bid, Px(65_001), Qty(1)),
            BookEvent::clear(4, 500),
            BookEvent::level(u64::MAX, u64::MAX, Side::Ask, Px(i64::MIN), Qty(i64::MAX)),
            BookEvent::level(6, 600, Side::Bid, Px(i64::MAX), Qty(i64::MIN)),
        ]
    }

    #[test]
    fn round_trip_is_identity() {
        for ev in events() {
            let back = unpack(&pack(&ev)).expect("valid record unpacks");
            // BookEvent is not PartialEq; compare every field explicitly.
            assert_eq!(back.seq, ev.seq);
            assert_eq!(back.ts, ev.ts);
            assert_eq!(back.px, ev.px);
            assert_eq!(back.qty, ev.qty);
            assert_eq!(back.side, ev.side);
            assert_eq!(back.kind, ev.kind);
        }
    }

    #[test]
    fn bad_side_is_rejected() {
        let mut rec = pack(&BookEvent::clear(0, 0));
        rec[4] = (rec[4] & !0xFF) | 7; // valid kind (0), bogus side
        assert!(matches!(unpack(&rec), Err(UnpackError::BadSide(7))));
    }

    #[test]
    fn bad_kind_is_rejected() {
        let mut rec = pack(&BookEvent::clear(0, 0));
        rec[4] = (rec[4] & 0xFF) | (9 << 8); // valid side, bogus kind
        assert!(matches!(unpack(&rec), Err(UnpackError::BadKind(9))));
    }
}

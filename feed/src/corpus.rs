//! Binary corpus format and deterministic replay source. The corpus is the
//! boundary: above it, integer ticks only — no floats, no async, no I/O on the
//! hot path. Replay is a resident `&[BookEvent]` slice iterated by reference.

use book::{BookEvent, EventKind, Px, Qty, Side};
use std::io::{self, Read, Write};
use std::path::Path;

pub const MAGIC: [u8; 8] = *b"MDFEED\0\0";
pub const VERSION: u32 = 1;
pub const RECORD_SIZE: usize = 40;
pub const HEADER_SIZE: usize = 32;

// The on-disk `record_size` header field is a `u32`; this is the same constant
// in that type. 40 trivially fits, so the narrowing is a non-event.
#[allow(clippy::cast_possible_truncation)]
const RECORD_SIZE_U32: u32 = RECORD_SIZE as u32;

/// Errors from loading/validating a corpus. Every malformed corpus maps to a
/// specific variant — corrupt input fails loudly, never silently or via UB.
#[derive(Debug)]
pub enum CorpusError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    BadRecordSize(u32),
    Truncated { expected: u64, found: u64 },
    BadDiscriminant { record: u64, field: &'static str, byte: u8 },
}

impl From<io::Error> for CorpusError {
    fn from(e: io::Error) -> Self {
        CorpusError::Io(e)
    }
}

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorpusError::Io(e) => write!(f, "corpus I/O error: {e}"),
            CorpusError::BadMagic => write!(f, "bad magic: not an MDFEED corpus"),
            CorpusError::UnsupportedVersion(v) => {
                write!(f, "unsupported corpus version {v} (expected {VERSION})")
            }
            CorpusError::BadRecordSize(s) => {
                write!(f, "bad record_size {s} (expected {RECORD_SIZE})")
            }
            CorpusError::Truncated { expected, found } => write!(
                f,
                "truncated corpus: header declares {expected} payload bytes, found {found}"
            ),
            CorpusError::BadDiscriminant {
                record,
                field,
                byte,
            } => write!(
                f,
                "bad {field} discriminant {byte} in record {record}"
            ),
        }
    }
}

impl std::error::Error for CorpusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CorpusError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// A loaded corpus: events resident in one owned buffer, replayed by reference.
#[derive(Debug, Default)]
pub struct Corpus {
    events: Vec<BookEvent>,
}

impl Corpus {
    /// Build from events already in memory (synthetic generator path).
    #[must_use]
    pub fn from_events(events: Vec<BookEvent>) -> Self {
        Self { events }
    }

    /// Load and FULLY VALIDATE a corpus file. One allocation (the event Vec).
    ///
    /// # Errors
    /// Returns [`CorpusError::Io`] if the file cannot be opened or read, or any
    /// validation error from [`Corpus::from_bytes`].
    pub fn load(path: &Path) -> Result<Self, CorpusError> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut buf)?;
        Self::from_bytes(&buf)
    }

    /// Parse + validate header, then each record. Reject corrupt input loudly.
    ///
    /// # Errors
    /// Returns the specific [`CorpusError`] for any malformed input: a buffer
    /// shorter than the header, bad magic, unsupported version, wrong record
    /// size, a payload length that is not `count * 40` bytes, or an
    /// out-of-range `side`/`kind` discriminant.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, CorpusError> {
        if buf.len() < HEADER_SIZE {
            return Err(CorpusError::Truncated {
                expected: HEADER_SIZE as u64,
                found: buf.len() as u64,
            });
        }

        // Header (32 bytes, little-endian). All slices below are length-checked
        // by construction, so `le_bytes` never panics.
        if buf[0..8] != MAGIC {
            return Err(CorpusError::BadMagic);
        }
        let version = u32::from_le_bytes(le_bytes::<4>(&buf[8..12]));
        if version != VERSION {
            return Err(CorpusError::UnsupportedVersion(version));
        }
        let record_size = u32::from_le_bytes(le_bytes::<4>(&buf[12..16]));
        if record_size as usize != RECORD_SIZE {
            return Err(CorpusError::BadRecordSize(record_size));
        }
        let count = u64::from_le_bytes(le_bytes::<8>(&buf[16..24]));
        // buf[24..32] is `meta`, reserved; ignored on read.

        let payload = &buf[HEADER_SIZE..];
        let expected = count
            .checked_mul(RECORD_SIZE as u64)
            .ok_or(CorpusError::Truncated {
                expected: u64::MAX,
                found: payload.len() as u64,
            })?;
        if payload.len() as u64 != expected {
            return Err(CorpusError::Truncated {
                expected,
                found: payload.len() as u64,
            });
        }

        let mut events = Vec::with_capacity(payload.len() / RECORD_SIZE);
        for (i, rec) in payload.chunks_exact(RECORD_SIZE).enumerate() {
            let rec_idx = i as u64;
            let seq = u64::from_le_bytes(le_bytes::<8>(&rec[0..8]));
            let ts = u64::from_le_bytes(le_bytes::<8>(&rec[8..16]));
            let px = Px(i64::from_le_bytes(le_bytes::<8>(&rec[16..24])));
            let qty = Qty(i64::from_le_bytes(le_bytes::<8>(&rec[24..32])));
            let side = side_from_u8(rec[32], rec_idx)?;
            let kind = kind_from_u8(rec[33], rec_idx)?;
            // rec[34..40] is `pad`; written zero, ignored here.
            events.push(event_from_parts(seq, ts, side, px, qty, kind));
        }

        Ok(Self { events })
    }

    /// Write events to a corpus file (header + records, all little-endian).
    ///
    /// # Errors
    /// Returns [`CorpusError::Io`] if the file cannot be created or any write fails.
    pub fn save(path: &Path, events: &[BookEvent]) -> Result<(), CorpusError> {
        let f = std::fs::File::create(path)?;
        let mut w = io::BufWriter::new(f);
        Self::write_to(&mut w, events)?;
        w.flush()?;
        Ok(())
    }

    /// Serialize the header + records (all little-endian) to `w`.
    ///
    /// # Errors
    /// Returns [`CorpusError::Io`] if any underlying write fails.
    pub fn write_to<W: Write>(w: &mut W, events: &[BookEvent]) -> Result<(), CorpusError> {
        // Header (32 bytes, little-endian).
        w.write_all(&MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&RECORD_SIZE_U32.to_le_bytes())?;
        w.write_all(&(events.len() as u64).to_le_bytes())?;
        w.write_all(&0u64.to_le_bytes())?; // meta, reserved

        // Records (40 bytes each, little-endian).
        for e in events {
            w.write_all(&e.seq.to_le_bytes())?;
            w.write_all(&e.ts.to_le_bytes())?;
            w.write_all(&e.px.ticks().to_le_bytes())?;
            w.write_all(&e.qty.lots().to_le_bytes())?;
            w.write_all(&[e.side as u8, e.kind as u8])?;
            w.write_all(&[0u8; 6])?; // pad
        }
        Ok(())
    }

    /// The replay source: resident, ordered, zero-alloc/zero-copy by reference.
    #[must_use]
    pub fn events(&self) -> &[BookEvent] {
        &self.events
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, BookEvent> {
        self.events.iter()
    }
}

impl<'a> IntoIterator for &'a Corpus {
    type Item = &'a BookEvent;
    type IntoIter = std::slice::Iter<'a, BookEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

/// Copy an exactly-`N`-byte slice into a fixed array for `from_le_bytes`.
/// Callers always pass a slice of length `N`, so the `copy_from_slice` is infallible.
#[inline]
fn le_bytes<const N: usize>(src: &[u8]) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(src);
    a
}

fn side_from_u8(b: u8, rec: u64) -> Result<Side, CorpusError> {
    match b {
        0 => Ok(Side::Bid),
        1 => Ok(Side::Ask),
        _ => Err(CorpusError::BadDiscriminant {
            record: rec,
            field: "side",
            byte: b,
        }),
    }
}

fn kind_from_u8(b: u8, rec: u64) -> Result<EventKind, CorpusError> {
    match b {
        0 => Ok(EventKind::Level),
        1 => Ok(EventKind::Trade),
        2 => Ok(EventKind::Clear),
        _ => Err(CorpusError::BadDiscriminant {
            record: rec,
            field: "kind",
            byte: b,
        }),
    }
}

/// Reconstruct a `BookEvent` from validated parts via the public constructors,
/// so the in-memory enums are always valid (no transmute).
fn event_from_parts(
    seq: u64,
    ts: u64,
    side: Side,
    px: Px,
    qty: Qty,
    kind: EventKind,
) -> BookEvent {
    match kind {
        EventKind::Level => BookEvent::level(seq, ts, side, px, qty),
        EventKind::Trade => BookEvent::trade(seq, ts, side, px, qty),
        EventKind::Clear => BookEvent::clear(seq, ts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field-by-field observable equality (`BookEvent` is not `PartialEq`).
    /// `Clear` ignores `px`/`qty`/`side`, mirroring the constructor semantics.
    fn ev_eq(a: &BookEvent, b: &BookEvent) -> bool {
        if a.seq != b.seq || a.ts != b.ts || a.kind != b.kind {
            return false;
        }
        match a.kind {
            EventKind::Clear => true,
            _ => a.px == b.px && a.qty == b.qty && a.side == b.side,
        }
    }

    fn slices_eq(a: &[BookEvent], b: &[BookEvent]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| ev_eq(x, y))
    }

    fn sample_events() -> Vec<BookEvent> {
        vec![
            BookEvent::clear(0, 1_000),
            BookEvent::level(1, 1_100, Side::Bid, Px(65_000), Qty(5)),
            BookEvent::level(2, 1_200, Side::Ask, Px(65_010), Qty(3)),
            BookEvent::trade(3, 1_300, Side::Ask, Px(65_010), Qty(1)),
            BookEvent::level(4, 1_400, Side::Bid, Px(65_000), Qty(0)), // removal
            BookEvent::level(5, 1_500, Side::Ask, Px(-7), Qty(9)),     // negative px allowed
        ]
    }

    fn write_bytes(events: &[BookEvent]) -> Vec<u8> {
        let mut buf = Vec::new();
        Corpus::write_to(&mut buf, events).expect("write_to");
        buf
    }

    #[test]
    fn round_trip_field_equality() {
        let events = sample_events();
        let buf = write_bytes(&events);
        let loaded = Corpus::from_bytes(&buf).expect("from_bytes");
        assert!(slices_eq(loaded.events(), &events));
    }

    #[test]
    fn empty_corpus_round_trips() {
        let buf = write_bytes(&[]);
        assert_eq!(buf.len(), HEADER_SIZE);
        let loaded = Corpus::from_bytes(&buf).expect("from_bytes");
        assert!(loaded.is_empty());
        assert_eq!(loaded.len(), 0);
    }

    #[test]
    fn large_corpus_round_trips() {
        let n: usize = 1_000_000;
        let mut events = Vec::with_capacity(n);
        for i in 0..n {
            let v = i64::try_from(i).expect("fits i64");
            events.push(BookEvent::level(
                i as u64,
                (i as u64) * 10,
                if i % 2 == 0 { Side::Bid } else { Side::Ask },
                Px(65_000 + (v % 100)),
                Qty((v % 50) + 1),
            ));
        }
        let buf = write_bytes(&events);
        assert_eq!(buf.len(), HEADER_SIZE + n * RECORD_SIZE);
        let loaded = Corpus::from_bytes(&buf).expect("from_bytes");
        assert_eq!(loaded.len(), n);
        assert!(slices_eq(loaded.events(), &events));
    }

    #[test]
    fn save_load_file_round_trip() {
        let events = sample_events();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("feed_corpus_test_{}.mdf", std::process::id()));
        Corpus::save(&path, &events).expect("save");
        let loaded = Corpus::load(&path).expect("load");
        let _ = std::fs::remove_file(&path);
        assert!(slices_eq(loaded.events(), &events));
    }

    #[test]
    fn determinism_byte_identical() {
        let events = sample_events();
        assert_eq!(write_bytes(&events), write_bytes(&events));
    }

    #[test]
    fn reject_bad_magic() {
        let mut buf = write_bytes(&sample_events());
        buf[0] = b'X';
        assert!(matches!(
            Corpus::from_bytes(&buf),
            Err(CorpusError::BadMagic)
        ));
    }

    #[test]
    fn reject_bad_version() {
        let mut buf = write_bytes(&sample_events());
        buf[8..12].copy_from_slice(&999u32.to_le_bytes());
        assert!(matches!(
            Corpus::from_bytes(&buf),
            Err(CorpusError::UnsupportedVersion(999))
        ));
    }

    #[test]
    fn reject_bad_record_size() {
        let mut buf = write_bytes(&sample_events());
        buf[12..16].copy_from_slice(&41u32.to_le_bytes());
        assert!(matches!(
            Corpus::from_bytes(&buf),
            Err(CorpusError::BadRecordSize(41))
        ));
    }

    #[test]
    fn reject_truncated_tail() {
        let mut buf = write_bytes(&sample_events());
        buf.truncate(buf.len() - 7); // tail no longer a multiple of 40
        assert!(matches!(
            Corpus::from_bytes(&buf),
            Err(CorpusError::Truncated { .. })
        ));
    }

    #[test]
    fn reject_too_short_for_header() {
        let buf = [0u8; HEADER_SIZE - 1];
        assert!(matches!(
            Corpus::from_bytes(&buf),
            Err(CorpusError::Truncated { .. })
        ));
    }

    #[test]
    fn reject_bad_side_discriminant() {
        let mut buf = write_bytes(&sample_events());
        // record 1 starts at HEADER_SIZE + RECORD_SIZE; side is at offset 32.
        let side_off = HEADER_SIZE + RECORD_SIZE + 32;
        buf[side_off] = 7;
        match Corpus::from_bytes(&buf) {
            Err(CorpusError::BadDiscriminant { record, field, byte }) => {
                assert_eq!((record, field, byte), (1, "side", 7));
            }
            other => panic!("expected BadDiscriminant, got {other:?}"),
        }
    }

    #[test]
    fn reject_bad_kind_discriminant() {
        let mut buf = write_bytes(&sample_events());
        // record 1: kind is at offset 33.
        let kind_off = HEADER_SIZE + RECORD_SIZE + 33;
        buf[kind_off] = 9;
        match Corpus::from_bytes(&buf) {
            Err(CorpusError::BadDiscriminant { record, field, byte }) => {
                assert_eq!((record, field, byte), (1, "kind", 9));
            }
            other => panic!("expected BadDiscriminant, got {other:?}"),
        }
    }

    #[test]
    fn pad_bytes_ignored_on_read() {
        let mut buf = write_bytes(&sample_events());
        // Dirty the pad of record 0 (offsets 34..40); must not affect decoding.
        for off in 34..40 {
            buf[HEADER_SIZE + off] = 0xFF;
        }
        let loaded = Corpus::from_bytes(&buf).expect("from_bytes");
        assert!(slices_eq(loaded.events(), &sample_events()));
    }

    #[test]
    fn error_display_is_specific() {
        let e = CorpusError::BadDiscriminant {
            record: 1,
            field: "side",
            byte: 7,
        };
        assert!(format!("{e}").contains("side"));
        // Exercise the Error trait object path.
        let _: &dyn std::error::Error = &e;
    }
}

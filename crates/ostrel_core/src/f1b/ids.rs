//! Replica, op, row and log identities and the hybrid logical clock (ARCHITECTURE 5.3).
//!
//! On the wire every id is a lowercase hex string of fixed width, big endian, with its fields
//! in comparison order, so string order of two encodings equals the order of the values.
//! Widths: [`ReplicaId`] 16, [`ServerSeq`] 16, [`OpId`] 24, [`RowId`] 32, [`Hlc`] 32.
//! Any other width, an uppercase digit, a sign or any other character is rejected.

use std::fmt;

/// Hex width of a [`ReplicaId`].
pub const REPLICA_ID_HEX: usize = 16;
/// Hex width of a [`ServerSeq`].
pub const SERVER_SEQ_HEX: usize = 16;
/// Hex width of an [`OpId`] (replica 16, then seq 8).
pub const OP_ID_HEX: usize = 24;
/// Hex width of a [`RowId`] (wall time 12, counter 4, replica 16).
pub const ROW_ID_HEX: usize = 32;
/// Hex width of an [`Hlc`] (wall time 12, counter 4, replica 16).
pub const HLC_HEX: usize = 32;

/// Largest wall time an [`Hlc`] can carry: 48 bits of milliseconds since the Unix epoch.
pub const MAX_WALL_MS: u64 = (1 << 48) - 1;

/// A hex id that is not in its strict wire form, or clock parts out of range.
///
/// Every case maps to the protocol reason `Invalid`; the variants exist for diagnostics only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidId {
    /// The string does not have the fixed width of the expected id.
    Width {
        /// Expected number of hex digits.
        expected: usize,
        /// Number of bytes found.
        found: usize,
    },
    /// A byte outside `0-9a-f` (uppercase, sign, whitespace, non ASCII) at this byte offset.
    Char {
        /// Byte offset of the first offending byte.
        offset: usize,
    },
    /// A wall time above [`MAX_WALL_MS`].
    WallTime,
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Width { expected, found } => {
                write!(f, "expected {expected} hex digits, found {found} bytes")
            }
            Self::Char { offset } => {
                write!(f, "byte {offset} is not a lowercase hex digit")
            }
            Self::WallTime => write!(f, "wall time does not fit in 48 bits"),
        }
    }
}

impl std::error::Error for InvalidId {}

/// One copy of shared state, issued by the server per (user, device key) (D32).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplicaId(pub u64);

/// Position in the server op log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerSeq(pub u64);

/// Identity of an op: its replica and a per replica sequence number, contiguous from 1.
///
/// Ordered by replica, then seq, which is also the order of the hex form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId {
    /// Replica that created the op.
    pub replica: ReplicaId,
    /// Sequence number within that replica.
    pub seq: u32,
}

/// Hybrid logical clock stamp: wall time (48 bits), counter, replica.
///
/// The total order (wall time, then counter, then replica) breaks every last writer wins tie.
/// The fields are private so that a wall time above 48 bits cannot exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    // Field order is the comparison order; the derived `Ord` relies on it.
    wall_ms: u64,
    counter: u16,
    replica: ReplicaId,
}

/// Row identity: wall time (48 bits), counter (16 bits) and replica (64 bits) of the creating
/// op's clock in one `u128`. Time sortable and bound to the creating replica (D20).
///
/// There is no constructor from a raw `u128`: a row id is made from a clock with
/// [`RowId::new`] or read from the wire with [`RowId::from_hex`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u128);

impl ReplicaId {
    /// Lowercase hex, 16 digits.
    pub fn to_hex(self) -> String {
        format!("{:016x}", self.0)
    }

    /// Parses the strict 16 digit form.
    pub fn from_hex(text: &str) -> Result<Self, InvalidId> {
        parse_hex(text, REPLICA_ID_HEX).map(|v| Self(low_u64(v)))
    }
}

impl ServerSeq {
    /// Lowercase hex, 16 digits.
    pub fn to_hex(self) -> String {
        format!("{:016x}", self.0)
    }

    /// Parses the strict 16 digit form.
    pub fn from_hex(text: &str) -> Result<Self, InvalidId> {
        parse_hex(text, SERVER_SEQ_HEX).map(|v| Self(low_u64(v)))
    }
}

impl OpId {
    /// Lowercase hex, 24 digits: replica, then seq.
    pub fn to_hex(self) -> String {
        format!("{:016x}{:08x}", self.replica.0, self.seq)
    }

    /// Parses the strict 24 digit form.
    pub fn from_hex(text: &str) -> Result<Self, InvalidId> {
        let v = parse_hex(text, OP_ID_HEX)?;
        Ok(Self {
            replica: ReplicaId(low_u64(v >> 32)),
            seq: (v & 0xffff_ffff) as u32,
        })
    }
}

impl Hlc {
    /// Creates a stamp; fails if `wall_ms` does not fit in 48 bits.
    pub fn new(wall_ms: u64, counter: u16, replica: ReplicaId) -> Result<Self, InvalidId> {
        if wall_ms > MAX_WALL_MS {
            return Err(InvalidId::WallTime);
        }
        Ok(Self {
            wall_ms,
            counter,
            replica,
        })
    }

    /// Milliseconds since the Unix epoch, at most [`MAX_WALL_MS`].
    pub fn wall_ms(self) -> u64 {
        self.wall_ms
    }

    /// Counter for stamps within the same millisecond.
    pub fn counter(self) -> u16 {
        self.counter
    }

    /// Replica that issued the stamp.
    pub fn replica(self) -> ReplicaId {
        self.replica
    }

    /// Lowercase hex, 32 digits: wall time 12, counter 4, replica 16.
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.packed())
    }

    /// Parses the strict 32 digit form.
    pub fn from_hex(text: &str) -> Result<Self, InvalidId> {
        parse_hex(text, HLC_HEX).map(unpack_clock)
    }

    fn packed(self) -> u128 {
        (u128::from(self.wall_ms) << 80)
            | (u128::from(self.counter) << 64)
            | u128::from(self.replica.0)
    }
}

impl RowId {
    /// The only constructor from parts: the id of a row created by an op stamped `hlc`.
    pub fn new(hlc: Hlc) -> Self {
        Self(hlc.packed())
    }

    /// The clock parts the id was made from.
    pub fn hlc(self) -> Hlc {
        unpack_clock(self.0)
    }

    /// Replica that created the row.
    pub fn replica(self) -> ReplicaId {
        self.hlc().replica
    }

    /// The raw 128 bit value, for storage in adapters.
    pub fn as_u128(self) -> u128 {
        self.0
    }

    /// Lowercase hex, 32 digits.
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    /// Parses the strict 32 digit form.
    pub fn from_hex(text: &str) -> Result<Self, InvalidId> {
        parse_hex(text, ROW_ID_HEX).map(Self)
    }
}

macro_rules! display_as_hex {
    ($($ty:ty),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }
    )*};
}

display_as_hex!(ReplicaId, ServerSeq, OpId, Hlc, RowId);

/// Unpacks the shared layout of [`Hlc`] and [`RowId`]; 48 bits of wall time always fit.
fn unpack_clock(v: u128) -> Hlc {
    Hlc {
        wall_ms: low_u64(v >> 80),
        counter: ((v >> 64) & 0xffff) as u16,
        replica: ReplicaId(low_u64(v)),
    }
}

fn low_u64(v: u128) -> u64 {
    (v & u128::from(u64::MAX)) as u64
}

/// Strict fixed width lowercase hex. Validates every byte itself, because
/// `u128::from_str_radix` would accept a leading `+` and uppercase digits.
fn parse_hex(text: &str, width: usize) -> Result<u128, InvalidId> {
    let bytes = text.as_bytes();
    if bytes.len() != width {
        return Err(InvalidId::Width {
            expected: width,
            found: bytes.len(),
        });
    }
    let mut v: u128 = 0;
    for (offset, &b) in bytes.iter().enumerate() {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => return Err(InvalidId::Char { offset }),
        };
        // width is at most 32, so this never overflows.
        v = (v << 4) | u128::from(digit);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_widths() {
        let r = ReplicaId(0xa);
        assert_eq!(r.to_hex(), "000000000000000a");
        assert_eq!(ReplicaId::from_hex(&r.to_hex()), Ok(r));
        let op = OpId {
            replica: ReplicaId(u64::MAX),
            seq: u32::MAX,
        };
        assert_eq!(op.to_hex(), "ffffffffffffffffffffffff");
        assert_eq!(OpId::from_hex(&op.to_hex()), Ok(op));
        let hlc = Hlc::new(MAX_WALL_MS, u16::MAX, ReplicaId(1)).unwrap();
        assert_eq!(hlc.to_hex().len(), HLC_HEX);
        assert_eq!(Hlc::from_hex(&hlc.to_hex()), Ok(hlc));
        let row = RowId::new(hlc);
        assert_eq!(row.to_hex(), hlc.to_hex());
        assert_eq!(row.hlc(), hlc);
        assert_eq!(row.replica(), ReplicaId(1));
        assert_eq!(RowId::from_hex(&row.to_hex()), Ok(row));
        let seq = ServerSeq(1);
        assert_eq!(seq.to_string(), "0000000000000001");
    }

    #[test]
    fn rejects_everything_but_strict_lowercase_hex() {
        let bad = [
            "",
            "000000000000000",
            "00000000000000000",
            "000000000000000A",
            "+00000000000000a",
            "-00000000000000a",
            " 00000000000000a",
            "00000000000000a ",
            "0x0000000000000a",
            "00000000000000g0",
            // 15 ASCII digits plus one two byte character: 17 bytes.
            "00000000000000\u{e9}",
            // 14 digits plus one two byte character: 16 bytes, not hex.
            "0000000000000\u{e9}0",
        ];
        for text in bad {
            assert!(ReplicaId::from_hex(text).is_err(), "accepted {text:?}");
            assert!(ServerSeq::from_hex(text).is_err(), "accepted {text:?}");
        }
        assert_eq!(
            ReplicaId::from_hex("000000000000000A"),
            Err(InvalidId::Char { offset: 15 })
        );
        assert_eq!(
            OpId::from_hex("000000000000000a0000001"),
            Err(InvalidId::Width {
                expected: 24,
                found: 23
            })
        );
    }

    #[test]
    fn wall_time_is_limited_to_48_bits() {
        assert_eq!(
            Hlc::new(MAX_WALL_MS + 1, 0, ReplicaId(0)),
            Err(InvalidId::WallTime)
        );
        assert!(Hlc::new(MAX_WALL_MS, 0, ReplicaId(0)).is_ok());
    }

    #[test]
    fn hex_order_equals_value_order() {
        let r = |n| ReplicaId(n);
        let stamps = [
            Hlc::new(1, 0, r(u64::MAX)).unwrap(),
            Hlc::new(1, 1, r(0)).unwrap(),
            Hlc::new(1, 1, r(2)).unwrap(),
            Hlc::new(2, 0, r(0)).unwrap(),
            Hlc::new(MAX_WALL_MS, 0, r(0)).unwrap(),
        ];
        for pair in stamps.windows(2) {
            assert!(pair[0] < pair[1]);
            assert!(pair[0].to_hex() < pair[1].to_hex());
            assert!(RowId::new(pair[0]) < RowId::new(pair[1]));
        }
        let a = OpId {
            replica: r(1),
            seq: u32::MAX,
        };
        let b = OpId {
            replica: r(2),
            seq: 1,
        };
        assert!(a < b && a.to_hex() < b.to_hex());
    }
}

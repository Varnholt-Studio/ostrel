//! Byte forms of ids and values as this driver stores them.
//!
//! Two forms exist for a [`Value`]:
//!
//! * [`encode_value`] and [`decode_value`]: the stored form. It keeps the exact variant, so a
//!   value read back equals the value written (`Text` stays `Text`, `-0.0` stays `-0.0`).
//! * [`order_key`]: a byte string whose bytewise order is [`compare_key`] (D61) and whose
//!   equality is equality under [`compare_key`]. It is the key of `Set` elements and `Map` keys
//!   in the side tables, so `ORDER BY` and the primary key follow the contract without any
//!   sorting in Rust.
//!
//! Ids and stamps are big endian (docs/db-mapping.md 2.3), so byte order is id and stamp order.

use std::fmt;

use ostrel_core::value::base64url;
use ostrel_db::api::{Hlc, OpId, ReplicaId, RowId, Value, compare_key};

/// Deepest nesting of `Set`, `Map` and `List` values this driver stores. The wire limit is 32
/// (ARCHITECTURE 5.9); the driver limit only keeps decoding a damaged file on a bounded stack.
pub const MAX_DEPTH: usize = 256;

/// A stored byte string that is not a valid stored form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corrupt;

impl fmt::Display for Corrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stored value is damaged")
    }
}

/// 16 bytes, big endian.
pub fn row_id_bytes(id: RowId) -> [u8; 16] {
    id.as_u128().to_be_bytes()
}

pub fn row_id_from(bytes: &[u8]) -> Result<RowId, Corrupt> {
    let raw: [u8; 16] = bytes.try_into().map_err(|_| Corrupt)?;
    RowId::from_hex(&format!("{:032x}", u128::from_be_bytes(raw))).map_err(|_| Corrupt)
}

/// Wall time (6 bytes), counter (2 bytes), replica (8 bytes), big endian.
pub fn hlc_bytes(h: Hlc) -> [u8; 16] {
    let mut out = [0u8; 16];
    let wall = h.wall_ms().to_be_bytes();
    out[..6].copy_from_slice(&wall[2..]);
    out[6..8].copy_from_slice(&h.counter().to_be_bytes());
    out[8..].copy_from_slice(&h.replica().0.to_be_bytes());
    out
}

pub fn hlc_from(bytes: &[u8]) -> Result<Hlc, Corrupt> {
    let raw: [u8; 16] = bytes.try_into().map_err(|_| Corrupt)?;
    let mut wall = [0u8; 8];
    wall[2..].copy_from_slice(&raw[..6]);
    let counter = u16::from_be_bytes([raw[6], raw[7]]);
    let replica = replica_from(&raw[8..])?;
    Hlc::new(u64::from_be_bytes(wall), counter, replica).map_err(|_| Corrupt)
}

pub fn replica_bytes(r: ReplicaId) -> [u8; 8] {
    r.0.to_be_bytes()
}

pub fn replica_from(bytes: &[u8]) -> Result<ReplicaId, Corrupt> {
    let raw: [u8; 8] = bytes.try_into().map_err(|_| Corrupt)?;
    Ok(ReplicaId(u64::from_be_bytes(raw)))
}

/// Op id from its stored replica blob and sequence number.
pub fn op_id_from(replica: &[u8], seq: i64) -> Result<OpId, Corrupt> {
    Ok(OpId {
        replica: replica_from(replica)?,
        seq: u32::try_from(seq).map_err(|_| Corrupt)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Stored form
// ---------------------------------------------------------------------------------------------

const T_NULL: u8 = 0;
const T_BOOL: u8 = 1;
const T_INT: u8 = 2;
const T_FLOAT: u8 = 3;
const T_TEXT: u8 = 4;
const T_TIME: u8 = 5;
const T_BYTES: u8 = 6;
const T_ENUM: u8 = 7;
const T_REF: u8 = 8;
const T_SET: u8 = 9;
const T_MAP: u8 = 10;
const T_LIST: u8 = 11;
const T_RANK: u8 = 12;

/// The stored form: a tag byte, then the payload. Lengths and counts are `u32` big endian.
/// `None` when the value is nested deeper than [`MAX_DEPTH`] or a length does not fit `u32`.
pub fn encode_value(v: &Value) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    encode_into(v, 0, &mut out)?;
    Some(out)
}

fn put_len(n: usize, out: &mut Vec<u8>) -> Option<()> {
    out.extend_from_slice(&u32::try_from(n).ok()?.to_be_bytes());
    Some(())
}

fn put_str(tag: u8, s: &[u8], out: &mut Vec<u8>) -> Option<()> {
    out.push(tag);
    put_len(s.len(), out)?;
    out.extend_from_slice(s);
    Some(())
}

fn encode_into(v: &Value, depth: usize, out: &mut Vec<u8>) -> Option<()> {
    match v {
        Value::Null => out.push(T_NULL),
        Value::Bool(b) => out.extend_from_slice(&[T_BOOL, u8::from(*b)]),
        Value::Int(i) => {
            out.push(T_INT);
            out.extend_from_slice(&i.get().to_be_bytes());
        }
        Value::Float(f) => {
            out.push(T_FLOAT);
            out.extend_from_slice(&f.get().to_bits().to_be_bytes());
        }
        Value::Time(t) => {
            out.push(T_TIME);
            out.extend_from_slice(&t.get().to_be_bytes());
        }
        Value::Text(s) => put_str(T_TEXT, s.as_bytes(), out)?,
        Value::Bytes(b) => put_str(T_BYTES, b, out)?,
        Value::Enum(s) => put_str(T_ENUM, s.as_bytes(), out)?,
        Value::Rank(s) => put_str(T_RANK, s.as_bytes(), out)?,
        Value::Ref(r) => {
            out.push(T_REF);
            out.extend_from_slice(&row_id_bytes(*r));
        }
        Value::Set(items) | Value::List(items) => {
            if depth >= MAX_DEPTH {
                return None;
            }
            out.push(if matches!(v, Value::Set(_)) {
                T_SET
            } else {
                T_LIST
            });
            put_len(items.len(), out)?;
            for item in items {
                encode_into(item, depth + 1, out)?;
            }
        }
        Value::Map(entries) => {
            if depth >= MAX_DEPTH {
                return None;
            }
            out.push(T_MAP);
            put_len(entries.len(), out)?;
            for (k, x) in entries {
                encode_into(k, depth + 1, out)?;
                encode_into(x, depth + 1, out)?;
            }
        }
    }
    Some(())
}

/// Reads a whole stored form; trailing bytes make it [`Corrupt`].
pub fn decode_value(bytes: &[u8]) -> Result<Value, Corrupt> {
    let mut r = Reader { bytes, pos: 0 };
    let v = r.value(0)?;
    if r.pos != bytes.len() {
        return Err(Corrupt);
    }
    Ok(v)
}

struct Reader<'b> {
    bytes: &'b [u8],
    pos: usize,
}

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Result<&'b [u8], Corrupt> {
        let end = self.pos.checked_add(n).ok_or(Corrupt)?;
        let s = self.bytes.get(self.pos..end).ok_or(Corrupt)?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Corrupt> {
        self.take(N)?.try_into().map_err(|_| Corrupt)
    }

    fn len(&mut self) -> Result<usize, Corrupt> {
        usize::try_from(u32::from_be_bytes(self.array()?)).map_err(|_| Corrupt)
    }

    fn string(&mut self) -> Result<String, Corrupt> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| Corrupt)
    }

    fn value(&mut self, depth: usize) -> Result<Value, Corrupt> {
        let [tag] = self.array()?;
        let int = |b: [u8; 8]| i64::from_be_bytes(b);
        Ok(match tag {
            T_NULL => Value::Null,
            T_BOOL => match self.array()? {
                [0] => Value::Bool(false),
                [1] => Value::Bool(true),
                _ => return Err(Corrupt),
            },
            T_INT => Value::int(int(self.array()?)).map_err(|_| Corrupt)?,
            T_TIME => Value::time(int(self.array()?)).map_err(|_| Corrupt)?,
            T_FLOAT => Value::float(f64::from_bits(u64::from_be_bytes(self.array()?)))
                .map_err(|_| Corrupt)?,
            T_TEXT => Value::Text(self.string()?),
            T_ENUM => Value::Enum(self.string()?),
            T_RANK => Value::Rank(self.string()?),
            T_BYTES => {
                let n = self.len()?;
                Value::Bytes(self.take(n)?.to_vec())
            }
            T_REF => Value::Ref(row_id_from(self.take(16)?)?),
            T_SET | T_LIST | T_MAP => {
                if depth >= MAX_DEPTH {
                    return Err(Corrupt);
                }
                let n = self.len()?;
                // Every item takes at least one byte, so a count beyond the rest is damage; this
                // also bounds the allocation below.
                if n > self.bytes.len() - self.pos {
                    return Err(Corrupt);
                }
                if tag == T_MAP {
                    let mut entries = Vec::with_capacity(n);
                    for _ in 0..n {
                        let k = self.value(depth + 1)?;
                        entries.push((k, self.value(depth + 1)?));
                    }
                    Value::Map(entries)
                } else {
                    let mut items = Vec::with_capacity(n);
                    for _ in 0..n {
                        items.push(self.value(depth + 1)?);
                    }
                    if tag == T_SET {
                        Value::Set(items)
                    } else {
                        Value::List(items)
                    }
                }
            }
            _ => return Err(Corrupt),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Order key
// ---------------------------------------------------------------------------------------------

/// Class bytes in the class order of [`compare_key`].
const K_NULL: u8 = 0x10;
const K_BOOL: u8 = 0x20;
const K_NUMBER: u8 = 0x30;
const K_STRING: u8 = 0x40;
const K_ARRAY: u8 = 0x50;

/// Byte string whose bytewise order and equality are those of [`compare_key`].
///
/// Each part is self delimiting, so the parts of an array concatenate without changing the
/// order: numbers have a fixed width, strings escape `0x00` as `0x00 0xFF` and end with
/// `0x00 0x00`, array items start with `0x01` and the array ends with `0x00`, so a shorter
/// prefix sorts first. `None` under the same conditions as [`encode_value`].
pub fn order_key(v: &Value) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    key_into(v, 0, &mut out)?;
    Some(out)
}

fn key_string(s: &[u8], out: &mut Vec<u8>) {
    out.push(K_STRING);
    for &b in s {
        out.push(b);
        if b == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

fn key_number(f: f64, out: &mut Vec<u8>) {
    // -0 equals 0 under compare_key. Every Int and Time is exact in f64 (53 bits).
    let f = if f == 0.0 { 0.0 } else { f };
    let bits = f.to_bits();
    let ordered = if bits >> 63 == 1 {
        !bits
    } else {
        bits | (1 << 63)
    };
    out.push(K_NUMBER);
    out.extend_from_slice(&ordered.to_be_bytes());
}

fn key_into(v: &Value, depth: usize, out: &mut Vec<u8>) -> Option<()> {
    match v {
        Value::Null => out.push(K_NULL),
        Value::Bool(b) => out.extend_from_slice(&[K_BOOL, u8::from(*b)]),
        Value::Int(i) | Value::Time(i) => key_number(i.get() as f64, out),
        Value::Float(f) => key_number(f.get(), out),
        Value::Text(s) | Value::Enum(s) | Value::Rank(s) => key_string(s.as_bytes(), out),
        Value::Ref(r) => key_string(r.to_hex().as_bytes(), out),
        Value::Bytes(b) => key_string(base64url(b).as_bytes(), out),
        Value::Set(_) | Value::List(_) | Value::Map(_) => {
            if depth >= MAX_DEPTH {
                return None;
            }
            out.push(K_ARRAY);
            let mut items: Vec<Value> = match v {
                Value::Set(items) | Value::List(items) => items.clone(),
                Value::Map(entries) => entries
                    .iter()
                    .map(|(k, x)| Value::List(vec![k.clone(), x.clone()]))
                    .collect(),
                _ => Vec::new(),
            };
            // Set elements and map entries in compare_key order (stable), as the wire writes
            // them; a list keeps program order.
            if !matches!(v, Value::List(_)) {
                items.sort_by(compare_key);
            }
            for item in &items {
                out.push(1);
                key_into(item, depth + 1, out)?;
            }
            out.push(0);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn int(v: i64) -> Value {
        Value::int(v).unwrap()
    }

    fn rid(n: u128) -> RowId {
        RowId::from_hex(&format!("{n:032x}")).unwrap()
    }

    /// Values from every class with the edge cases of each.
    fn samples() -> Vec<Value> {
        let int_max = (1i64 << 53) - 1;
        let mut v = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            int(-int_max),
            int(-1),
            int(0),
            int(1),
            int(int_max),
            Value::float(-0.0).unwrap(),
            Value::float(0.0).unwrap(),
            Value::float(0.5).unwrap(),
            Value::float(-1e300).unwrap(),
            Value::float(f64::MIN_POSITIVE).unwrap(),
            Value::time(1).unwrap(),
            Value::Text(String::new()),
            Value::Text("a".into()),
            Value::Text("a\u{0}".into()),
            Value::Text("a\u{0}\u{0}".into()),
            Value::Text("a\u{1}".into()),
            Value::Text("ab".into()),
            Value::Text("\u{ff01}".into()),
            Value::Text("\u{1f600}".into()),
            Value::Enum("a".into()),
            Value::Rank("a0".into()),
            Value::Bytes(vec![]),
            Value::Bytes(vec![0]),
            Value::Bytes(vec![0xf8]),
            Value::Ref(rid(1)),
            Value::Ref(rid(u128::MAX >> 8)),
            Value::List(vec![]),
            Value::List(vec![int(1)]),
            Value::List(vec![int(1), int(2)]),
            Value::List(vec![int(2)]),
            Value::List(vec![Value::Text("a".into())]),
            Value::List(vec![Value::Text("a\u{0}".into())]),
            Value::Set(vec![int(2), int(1)]),
            Value::Set(vec![int(1), int(2)]),
            Value::Map(vec![(int(2), Value::Null), (int(1), Value::Bool(true))]),
            Value::List(vec![Value::List(vec![])]),
        ];
        v.push(Value::List(v.clone()));
        v
    }

    #[test]
    fn stored_form_round_trips_every_variant_exactly() {
        for v in samples() {
            let bytes = encode_value(&v).unwrap();
            let back = decode_value(&bytes).unwrap();
            assert_eq!(back, v);
            // -0.0 keeps its sign, which PartialEq of f64 would not show.
            if let (Value::Float(a), Value::Float(b)) = (&v, &back) {
                assert_eq!(a.get().to_bits(), b.get().to_bits());
            }
        }
    }

    #[test]
    fn order_key_order_and_equality_are_compare_key() {
        let s = samples();
        for a in &s {
            for b in &s {
                let (ka, kb) = (order_key(a).unwrap(), order_key(b).unwrap());
                assert_eq!(ka.cmp(&kb), compare_key(a, b), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(
            compare_key(&int(0), &Value::float(-0.0).unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn damaged_stored_forms_are_refused() {
        let good = encode_value(&Value::List(vec![Value::Text("x".into())])).unwrap();
        for n in 0..good.len() {
            assert_eq!(decode_value(&good[..n]), Err(Corrupt), "prefix {n}");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(decode_value(&trailing), Err(Corrupt));
        let bad = [
            vec![99],
            vec![T_BOOL, 2],
            vec![T_INT, 0, 0x20, 0, 0, 0, 0, 0, 0], // 2^53, outside Int
            vec![T_FLOAT, 0x7f, 0xf0, 0, 0, 0, 0, 0, 0], // infinity
            vec![T_TEXT, 0, 0, 0, 1, 0xff],         // not UTF-8
            vec![T_LIST, 0xff, 0xff, 0xff, 0xff],   // count beyond the data
        ];
        for b in bad {
            assert_eq!(decode_value(&b), Err(Corrupt), "{b:?}");
        }
    }

    #[test]
    fn nesting_is_bounded_on_write_and_read() {
        let mut v = Value::Null;
        for _ in 0..MAX_DEPTH {
            v = Value::List(vec![v]);
        }
        let bytes = encode_value(&v).unwrap();
        assert!(decode_value(&bytes).is_ok());
        assert!(order_key(&v).is_some());
        let deeper = Value::List(vec![v]);
        assert_eq!(encode_value(&deeper), None);
        assert_eq!(order_key(&deeper), None);
        // A damaged file with a far deeper chain fails without exhausting the stack.
        let mut chain = Vec::new();
        for _ in 0..100_000 {
            chain.extend_from_slice(&[T_LIST, 0, 0, 0, 1]);
        }
        chain.push(T_NULL);
        assert_eq!(decode_value(&chain), Err(Corrupt));
    }

    #[test]
    fn ids_and_stamps_keep_their_order_as_bytes() {
        let a = Hlc::new(5, 1, ReplicaId(9)).unwrap();
        let b = Hlc::new(5, 2, ReplicaId(0)).unwrap();
        let c = Hlc::new((1 << 48) - 1, u16::MAX, ReplicaId(u64::MAX)).unwrap();
        for h in [a, b, c] {
            assert_eq!(hlc_from(&hlc_bytes(h)).unwrap(), h);
            let id = RowId::new(h);
            assert_eq!(row_id_from(&row_id_bytes(id)).unwrap(), id);
        }
        assert!(hlc_bytes(a) < hlc_bytes(b) && hlc_bytes(b) < hlc_bytes(c));
        assert_eq!(hlc_from(&[0; 15]), Err(Corrupt));
        assert_eq!(row_id_from(&[0; 17]), Err(Corrupt));
        assert_eq!(op_id_from(&[0; 8], -1), Err(Corrupt));
        assert_eq!(op_id_from(&[0; 8], i64::from(u32::MAX) + 1), Err(Corrupt));
    }
}

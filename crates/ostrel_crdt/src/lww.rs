//! Last writer wins register (ARCHITECTURE 5.1), the merge of every scalar field, enum,
//! reference, `Rank` and whole value `List`. Counterpart of
//! `runtime/js/crdt/register/register.mjs`.

use ostrel_core::canon::{encode, encode_object};
use ostrel_core::ids::Hlc;
use ostrel_core::value::Value;

use crate::CrdtError;

/// What [`LwwRegister::value`] reads before the first write.
static NULL: Value = Value::Null;

/// A register: the write with the highest [`Hlc`] wins. `Hlc` is a total order (wall time,
/// counter, replica), so no further tie breaker exists and every replica that applied the
/// same writes holds the same register, whatever the order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LwwRegister {
    current: Option<(Hlc, Value)>,
}

impl LwwRegister {
    /// A register no write has reached: value `null`, no `Hlc`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one write. Returns whether the register changed: `false` for a write that
    /// lost or was already applied. Fails with [`CrdtError::ConflictingWrite`] when the
    /// register already holds a different value under the same `Hlc`.
    pub fn apply(&mut self, hlc: Hlc, value: Value) -> Result<bool, CrdtError> {
        match &self.current {
            Some((held, _)) if *held > hlc => Ok(false),
            Some((held, old)) if *held == hlc => {
                if same_value(old, &value) {
                    Ok(false)
                } else {
                    Err(CrdtError::ConflictingWrite(hlc))
                }
            }
            _ => {
                self.current = Some((hlc, value));
                Ok(true)
            }
        }
    }

    /// Merges another replica's register into this one; the same rule as [`Self::apply`].
    pub fn merge(&mut self, other: &Self) -> Result<bool, CrdtError> {
        match &other.current {
            Some((hlc, value)) => self.apply(*hlc, value.clone()),
            None => Ok(false),
        }
    }

    /// The value a program reads: the winning write, or `null` before the first write.
    pub fn value(&self) -> &Value {
        self.current.as_ref().map_or(&NULL, |(_, value)| value)
    }

    /// The `Hlc` of the winning write, `None` before the first write.
    pub fn hlc(&self) -> Option<Hlc> {
        self.current.as_ref().map(|(hlc, _)| *hlc)
    }

    /// Canonical encoding of [`Self::value`].
    pub fn encode_value(&self) -> String {
        encode(self.value())
    }

    /// Canonical encoding of the full state `{"hlc": h, "value": v}`, `h` the 32 digit hex
    /// or `null` before the first write.
    pub fn encode_state(&self) -> String {
        let hlc = self
            .hlc()
            .map_or_else(|| "null".to_owned(), |h| encode(&Value::Text(h.to_hex())));
        // The two keys are fixed and distinct, so `encode_object` cannot fail.
        encode_object(&[("hlc", hlc), ("value", self.encode_value())]).unwrap_or_default()
    }
}

/// Two values are the same write when their canonical encodings agree (`-0` equals `0`,
/// set order does not matter), as in the JS runtime.
pub(crate) fn same_value(a: &Value, b: &Value) -> bool {
    a == b || encode(a) == encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ostrel_core::ids::ReplicaId;

    fn hlc(wall: u64, counter: u16, replica: u64) -> Hlc {
        Hlc::new(wall, counter, ReplicaId(replica)).unwrap()
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    #[test]
    fn empty_register_reads_null_without_hlc() {
        let r = LwwRegister::new();
        assert_eq!(r.value(), &Value::Null);
        assert_eq!(r.hlc(), None);
        assert_eq!(r.encode_state(), r#"{"hlc":null,"value":null}"#);
    }

    #[test]
    fn highest_hlc_wins_in_every_order() {
        let writes = [
            (hlc(5, 0, 1), text("a")),
            (hlc(5, 1, 0), text("b")),
            (hlc(4, 9, 9), text("c")),
        ];
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [2, 0, 1]] {
            let mut r = LwwRegister::new();
            for i in order {
                let (h, v) = writes[i].clone();
                r.apply(h, v).unwrap();
            }
            assert_eq!(r.value(), &text("b"));
            assert_eq!(r.hlc(), Some(hlc(5, 1, 0)));
        }
    }

    #[test]
    fn replay_is_no_change_and_conflict_is_refused() {
        let mut r = LwwRegister::new();
        assert_eq!(r.apply(hlc(1, 0, 1), text("x")), Ok(true));
        assert_eq!(r.apply(hlc(1, 0, 1), text("x")), Ok(false));
        assert_eq!(r.apply(hlc(0, 0, 1), text("old")), Ok(false));
        assert_eq!(
            r.apply(hlc(1, 0, 1), text("y")),
            Err(CrdtError::ConflictingWrite(hlc(1, 0, 1)))
        );
        assert_eq!(r.value(), &text("x"));
        // -0 and 0 are the same write.
        let mut z = LwwRegister::new();
        z.apply(hlc(1, 0, 1), Value::float(0.0).unwrap()).unwrap();
        assert_eq!(
            z.apply(hlc(1, 0, 1), Value::float(-0.0).unwrap()),
            Ok(false)
        );
    }

    #[test]
    fn merge_is_commutative_and_idempotent() {
        let mut a = LwwRegister::new();
        a.apply(hlc(1, 0, 1), text("a")).unwrap();
        let mut b = LwwRegister::new();
        b.apply(hlc(1, 0, 2), text("b")).unwrap();
        let mut ab = a.clone();
        ab.merge(&b).unwrap();
        let mut ba = b.clone();
        ba.merge(&a).unwrap();
        assert_eq!(ab, ba);
        assert_eq!(ab.merge(&ab.clone()), Ok(false));
        assert_eq!(ab.merge(&LwwRegister::new()), Ok(false));
        assert_eq!(
            ab.encode_state(),
            r#"{"hlc":"00000000000100000000000000000002","value":"b"}"#
        );
    }
}

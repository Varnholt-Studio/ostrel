//! `Float` arithmetic and text form (ARCHITECTURE 7.4 and 3.4, D185, SPEC 12.4).
//!
//! `Float` is IEEE 754 binary64 with round to nearest, and every value is finite. The
//! operations here are the single Rust implementation of these rules: the bytecode
//! instructions for `Float` use them once the IR carries `Float` operands, and the
//! shared golden vectors of `tests/runtime/` pin the same results for the JS runtime.
//!
//! Every function expects finite operands. A result that is not finite is never
//! returned as a value: it is `DivisionByZero` for a zero divisor of `/` or `%` and
//! `FloatOverflow` otherwise.

use std::cmp::Ordering;

use crate::RuntimeErrorKind;

/// Binary arithmetic operator on two `Float` operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`, the remainder with the sign of the dividend.
    Rem,
}

/// Checked `Float` arithmetic.
///
/// `/` or `%` with a divisor of `0.0` or `-0.0` is `DivisionByZero`, also `0.0 / 0.0`;
/// that is the only way finite operands could produce NaN. Any other result that is
/// not finite is `FloatOverflow`. Underflow to `0.0` or `-0.0` is no error.
pub fn arith(op: FloatOp, a: f64, b: f64) -> Result<f64, RuntimeErrorKind> {
    let value = match op {
        FloatOp::Add => a + b,
        FloatOp::Sub => a - b,
        FloatOp::Mul => a * b,
        // `-0.0 == 0.0`, so this covers both zeros.
        FloatOp::Div | FloatOp::Rem if b == 0.0 => return Err(RuntimeErrorKind::DivisionByZero),
        FloatOp::Div => a / b,
        FloatOp::Rem => a % b,
    };
    finite(value)
}

/// Unary `-`. The negation of a finite value is finite; a value that is not finite
/// is refused all the same, so no such value ever leaves this module.
pub fn neg(a: f64) -> Result<f64, RuntimeErrorKind> {
    finite(-a)
}

/// `==` on two `Float` values: IEEE equality, so `-0.0 == 0.0` is true.
pub fn equal(a: f64, b: f64) -> bool {
    a == b
}

/// Order of two `Float` values for `<`, `<=`, `>` and `>=`. `-0.0` and `0.0` are
/// equal: neither orders before the other.
pub fn order(a: f64, b: f64) -> Ordering {
    // Finite values always compare; `Equal` only covers a defect, never a value.
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// Text form of a `Float` for `print`, interpolation and `ToText`: the canonical writer
/// of ARCHITECTURE 5.3, which is ECMAScript `Number.prototype.toString`. `-0.0` is
/// written as `0`.
pub fn to_text(a: f64) -> String {
    ostrel_core::canon::encode_f64(a)
}

/// Maps a result that is not finite to `FloatOverflow`.
fn finite(value: f64) -> Result<f64, RuntimeErrorKind> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(RuntimeErrorKind::FloatOverflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIV_ZERO: Result<f64, RuntimeErrorKind> = Err(RuntimeErrorKind::DivisionByZero);
    const OVERFLOW: Result<f64, RuntimeErrorKind> = Err(RuntimeErrorKind::FloatOverflow);

    /// Compares results bit for bit, so `0.0` and `-0.0` are told apart.
    fn same(got: Result<f64, RuntimeErrorKind>, want: Result<f64, RuntimeErrorKind>) -> bool {
        match (got, want) {
            (Ok(a), Ok(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        }
    }

    #[test]
    fn spec_vectors_of_12_4() {
        let cases = [
            (FloatOp::Div, 1.0, 0.0, DIV_ZERO),
            (FloatOp::Rem, 1.0, -0.0, DIV_ZERO),
            (FloatOp::Div, 0.0, 0.0, DIV_ZERO),
            (FloatOp::Mul, 1e308, 10.0, OVERFLOW),
            (FloatOp::Sub, -1e308, 1e308, OVERFLOW),
            (FloatOp::Div, 5e-324, 2.0, Ok(0.0)),
        ];
        for (op, a, b, want) in cases {
            let got = arith(op, a, b);
            assert!(
                same(got, want),
                "{op:?} {a:e} {b:e}: {got:?}, want {want:?}"
            );
        }
    }

    #[test]
    fn every_zero_divisor_is_division_by_zero() {
        for op in [FloatOp::Div, FloatOp::Rem] {
            for a in [0.0, -0.0, 1.0, -1.0, 5e-324, f64::MAX, f64::MIN] {
                for b in [0.0, -0.0] {
                    assert_eq!(arith(op, a, b), DIV_ZERO, "{op:?} {a:e} {b:e}");
                }
            }
        }
    }

    #[test]
    fn overflow_in_every_operator() {
        let cases = [
            (FloatOp::Add, f64::MAX, f64::MAX),
            (FloatOp::Add, f64::MIN, f64::MIN),
            (FloatOp::Sub, f64::MAX, f64::MIN),
            (FloatOp::Mul, f64::MAX, -2.0),
            (FloatOp::Div, f64::MAX, 0.5),
            (FloatOp::Div, 1.0, 5e-324),
        ];
        for (op, a, b) in cases {
            assert_eq!(arith(op, a, b), OVERFLOW, "{op:?} {a:e} {b:e}");
        }
    }

    #[test]
    fn largest_finite_results_are_values() {
        let cases = [
            (FloatOp::Add, f64::MAX, 0.0, Ok(f64::MAX)),
            (FloatOp::Mul, f64::MAX, -1.0, Ok(f64::MIN)),
            // Rounds back to the largest finite value under round to nearest.
            (FloatOp::Add, f64::MAX, 1.0, Ok(f64::MAX)),
            (FloatOp::Div, f64::MAX, 1.0, Ok(f64::MAX)),
        ];
        for (op, a, b, want) in cases {
            let got = arith(op, a, b);
            assert!(same(got, want), "{op:?} {a:e} {b:e}: {got:?}");
        }
    }

    #[test]
    fn underflow_keeps_the_sign_of_zero() {
        assert!(same(arith(FloatOp::Mul, 5e-324, 0.5), Ok(0.0)));
        assert!(same(arith(FloatOp::Mul, -5e-324, 0.5), Ok(-0.0)));
        assert!(same(arith(FloatOp::Div, -5e-324, 4.0), Ok(-0.0)));
    }

    #[test]
    fn plain_results_are_ieee() {
        let cases = [
            (FloatOp::Add, 0.1, 0.2, 0.300_000_000_000_000_04),
            (FloatOp::Sub, 1.0, 1.5, -0.5),
            (FloatOp::Mul, 1.5, -4.0, -6.0),
            (FloatOp::Div, 1.0, 3.0, 1.0 / 3.0),
        ];
        for (op, a, b, want) in cases {
            assert!(same(arith(op, a, b), Ok(want)), "{op:?} {a:e} {b:e}");
        }
    }

    #[test]
    fn remainder_has_the_sign_of_the_dividend() {
        let cases = [
            (5.5, 2.0, 1.5),
            (-5.5, 2.0, -1.5),
            (5.5, -2.0, 1.5),
            (-5.5, -2.0, -1.5),
            (-4.0, 2.0, -0.0),
            (4.0, -2.0, 0.0),
            (1.0, f64::MAX, 1.0),
        ];
        for (a, b, want) in cases {
            assert!(same(arith(FloatOp::Rem, a, b), Ok(want)), "{a:e} % {b:e}");
        }
    }

    #[test]
    fn negation_is_finite_and_flips_zero() {
        assert!(same(neg(0.0), Ok(-0.0)));
        assert!(same(neg(-0.0), Ok(0.0)));
        assert!(same(neg(f64::MAX), Ok(f64::MIN)));
        assert!(same(neg(-1.5), Ok(1.5)));
    }

    #[test]
    fn values_that_are_not_finite_never_pass() {
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(neg(x), OVERFLOW);
            assert_eq!(arith(FloatOp::Add, x, 1.0), OVERFLOW);
            assert_eq!(arith(FloatOp::Div, x, 2.0), OVERFLOW);
        }
    }

    #[test]
    fn zeros_are_equal_and_unordered() {
        assert!(equal(-0.0, 0.0));
        assert!(equal(0.0, -0.0));
        assert_eq!(order(-0.0, 0.0), Ordering::Equal);
        assert_eq!(order(0.0, -0.0), Ordering::Equal);
        assert_eq!(order(-1.0, 0.0), Ordering::Less);
        assert_eq!(order(f64::MAX, 1.0), Ordering::Greater);
        assert!(!equal(0.1 + 0.2, 0.3));
    }

    #[test]
    fn text_form_is_the_canonical_writer() {
        let cases = [
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (5e-7, "5e-7"),
            (-0.0, "0"),
            (0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (0.1 + 0.2, "0.30000000000000004"),
        ];
        for (v, want) in cases {
            assert_eq!(to_text(v), want, "{v:e}");
        }
    }
}

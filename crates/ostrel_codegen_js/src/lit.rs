//! JavaScript literals.

use std::fmt::Write;

/// Appends `text` as a single quoted JavaScript string literal. Printable
/// ASCII stays as is, except the quote and the backslash; every other
/// character becomes a `\u{...}` escape, so the output is plain ASCII and no
/// line terminator (U+2028, U+2029 included) can end the literal.
pub(crate) fn string(text: &str, out: &mut String) {
    out.push('\'');
    for c in text.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            _ => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
        }
    }
    out.push('\'');
}

/// Appends a finite `f64` as a JavaScript number literal that reads back as
/// the same value (Rust prints the shortest decimal that round trips), in
/// parentheses so a sign never joins a preceding operator. `None` for NaN and
/// the infinities, which are not `Float` values.
pub(crate) fn float(value: f64, out: &mut String) -> Option<()> {
    if !value.is_finite() {
        return None;
    }
    let _ = write!(out, "({value})");
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> String {
        let mut out = String::new();
        string(text, &mut out);
        out
    }

    #[test]
    fn strings_are_ascii_with_escapes() {
        assert_eq!(lit("a'b\\c"), "'a\\'b\\\\c'");
        assert_eq!(lit("\n\u{2028}\u{1F600}"), "'\\u{a}\\u{2028}\\u{1f600}'");
        assert_eq!(lit(""), "''");
    }

    #[test]
    fn floats_round_trip_and_reject_non_finite() {
        let mut out = String::new();
        assert_eq!(float(-0.0, &mut out), Some(()));
        assert_eq!(float(0.1, &mut out), Some(()));
        assert_eq!(out, "(-0)(0.1)");
        assert_eq!(float(f64::NAN, &mut out), None);
        assert_eq!(float(f64::INFINITY, &mut out), None);
    }
}

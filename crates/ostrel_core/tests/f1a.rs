//! Contract tests of the F1a surface of `ostrel_core` as other crates use it.

use ostrel_core::{Code, Diagnostic, FileId, Severity, SourceMap, Span};

#[test]
fn renders_spec_line_format() {
    let mut map = SourceMap::new();
    let text = "fn main()\n  let s = \"\u{e9}\u{1F600}\n";
    let file = map.add("tests/errors/unterminated.ostl", text).unwrap();
    let quote = u32::try_from(text.find('"').unwrap()).unwrap();
    let d = Diagnostic::error(
        Code::new(3),
        Span::new(file, quote, quote + 1),
        "unterminated string literal",
    );
    assert_eq!(
        d.render(&map),
        "tests/errors/unterminated.ostl:2:11: error[E0003]: unterminated string literal"
    );
    assert!(d.is_error());
}

#[test]
fn column_after_astral_character_counts_one() {
    let mut map = SourceMap::new();
    let text = "x\u{1F600}y";
    let file = map.add("p.ostl", text).unwrap();
    let y = u32::try_from(text.find('y').unwrap()).unwrap();
    let d = Diagnostic::error(Code::new(8), Span::point(file, y), "unexpected character");
    assert_eq!(
        d.render(&map),
        "p.ostl:1:3: error[E0008]: unexpected character"
    );
}

#[test]
fn warning_and_notes() {
    let mut map = SourceMap::new();
    let file = map.add("w.ostl", "abc").unwrap();
    let d = Diagnostic::warning(Code::new(20), Span::new(file, 1, 2), "line too long")
        .with_note("left as is");
    assert_eq!(d.severity, Severity::Warning);
    assert!(!d.is_error());
    assert_eq!(
        d.render_with_notes(&map),
        "w.ostl:1:2: warning[E0020]: line too long\n  note: left as is\n"
    );
}

#[test]
fn hostile_text_cannot_break_the_line() {
    let mut map = SourceMap::new();
    let file = map.add("evil\n.ostl", "a").unwrap();
    let d = Diagnostic::error(
        Code::new(4),
        Span::point(file, 0),
        "bad escape `\u{1b}]0;x\u{7}\r\nfake:1:1: error`",
    );
    let line = d.render(&map);
    assert!(!line.contains('\n') && !line.contains('\r') && !line.contains('\u{1b}'));
    assert_eq!(
        line,
        "evil\\u{000A}.ostl:1:1: error[E0004]: bad escape `\\u{001B}]0;x\\u{0007}\\u{000D}\\u{000A}fake:1:1: error`"
    );
}

#[test]
fn unknown_file_and_bad_offsets_do_not_panic() {
    let map = SourceMap::new();
    let d = Diagnostic::error(
        Code::new(1),
        Span::new(FileId::from_raw(42), u32::MAX, 0),
        "x",
    );
    assert_eq!(d.render(&map), "<unknown>:0:0: error[E0001]: x");

    let mut map = SourceMap::new();
    let file = map.add("o.ostl", "\u{1F600}").unwrap();
    let d = Diagnostic::error(Code::new(1), Span::new(file, 2, u32::MAX), "x");
    assert_eq!(d.render(&map), "o.ostl:1:1: error[E0001]: x");
}

#[test]
fn files_get_distinct_ids() {
    let mut map = SourceMap::new();
    let a = map.add("a.ostl", "1").unwrap();
    let b = map.add("b.ostl", "2").unwrap();
    assert_ne!(a, b);
    assert_eq!(map.path(b), Some("b.ostl"));
    assert_eq!(map.text(a), Some("1"));
    assert_eq!(map.len(), 2);
}

//! The protocol types never depend on the codec (D135): `wire` builds `protocol` types, never
//! the other way round, so a reader of `protocol.rs` needs no codec in mind.

const PROTOCOL: &str = include_str!("../src/protocol.rs");

/// The places in `source` that name the `wire` module, as `line: text`. In code a path such as
/// `crate::wire::encode_op` counts; in a `use` item the word `wire` anywhere counts, which also
/// catches a grouped import (`use crate::{protocol, wire};`) spread over several lines.
fn uses_of_wire(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    // The `use` item being read: the number of its first line and its text so far.
    let mut item: Option<(usize, String)> = None;
    for (index, line) in source.lines().enumerate() {
        let number = index + 1;
        let code = line.split("//").next().unwrap_or_default();
        let trimmed = code.trim_start();
        if item.is_none() {
            if !(trimmed.starts_with("use ") || trimmed.starts_with("pub use ")) {
                if code.contains("wire::") {
                    found.push(format!("{number}: {}", line.trim()));
                }
                continue;
            }
            item = Some((number, String::new()));
        }
        let complete = match &mut item {
            Some((_, text)) => {
                text.push_str(code);
                text.push(' ');
                text.contains(';')
            }
            None => false,
        };
        if complete
            && let Some((first, text)) = item.take()
            && names_wire(&text)
        {
            found.push(format!("{first}: {}", text.trim()));
        }
    }
    found
}

/// True when `wire` stands as a whole word in `text`.
fn names_wire(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| word == "wire")
}

#[test]
fn protocol_imports_nothing_from_wire() {
    assert_eq!(uses_of_wire(PROTOCOL), Vec::<String>::new());
}

#[test]
fn the_check_finds_plain_grouped_and_nested_imports() {
    for source in [
        "use crate::wire::WireTy;",
        "use crate::{wire, protocol};",
        "use crate::{protocol::Op, wire::{self, WireTy}};",
        "use super::{\n    protocol,\n    wire,\n};",
        "pub use crate::wire;",
        "fn f() { crate::wire::encode_op(); }",
    ] {
        assert_eq!(uses_of_wire(source).len(), 1, "{source}");
    }
    for source in [
        "use crate::wired::X;",
        "use crate::protocol::Op; // wire::X",
        "/// The wire form.",
        "use crate::{protocol, wirelike};",
    ] {
        assert_eq!(uses_of_wire(source), Vec::<String>::new(), "{source}");
    }
}

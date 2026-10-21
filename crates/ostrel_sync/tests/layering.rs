//! The protocol types never depend on the codec (D135): `wire` builds `protocol` types, never
//! the other way round, so a reader of `protocol.rs` needs no codec in mind.

const PROTOCOL: &str = include_str!("../src/protocol.rs");

#[test]
fn protocol_imports_nothing_from_wire() {
    for (number, line) in PROTOCOL.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        assert!(
            !["crate::wire", "super::wire", "wire::"]
                .iter()
                .any(|path| code.contains(path)),
            "src/protocol.rs:{} uses the codec: {line}",
            number + 1
        );
    }
}

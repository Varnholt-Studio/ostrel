//! The VM side of `Url.parse` against the shared URL corpus (ARCHITECTURE 7.5, SPEC 12.13,
//! D144).
//!
//! `tests/hostile/url/corpus.txt` is also run by `runtime/js/view/safe/safe.test.mjs`
//! against `parseUrl`, so every case is a promise that client and server decide alike.
//! Format: one case per line, `ok` TAB input TAB expected text, or `none` TAB input;
//! escapes `\t`, `\\` and `\u{H}` only, no raw control characters.

use std::collections::HashSet;
use std::path::PathBuf;

use ostrel_vm::url::parse_url;

/// One corpus case: where it stands, its input and the expected result.
struct Case {
    place: String,
    input: String,
    expected: Option<String>,
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/hostile/url/corpus.txt")
}

/// Decodes the escapes of the corpus format: `\t`, `\\` and `\u{H}` (1 to 6 hex digits, a
/// Unicode scalar value) only. Any other backslash sequence is an error.
fn unescape(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let rest = chars.as_str();
                let decoded = rest
                    .strip_prefix('{')
                    .and_then(|r| r.split_once('}'))
                    .filter(|(hex, _)| {
                        (1..=6).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    .and_then(|(hex, after)| {
                        let scalar = u32::from_str_radix(hex, 16).ok()?;
                        Some((char::from_u32(scalar)?, after))
                    });
                let Some((scalar, after)) = decoded else {
                    return Err(format!("unknown escape near {:?}", text));
                };
                out.push(scalar);
                chars = after.chars();
            }
            other => {
                return Err(format!(
                    "unknown escape \\{}",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        }
    }
    Ok(out)
}

fn read_corpus() -> Vec<Case> {
    let path = corpus_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let body = text
        .strip_suffix('\n')
        .expect("the corpus ends with a line end");
    body.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let place = format!("corpus.txt:{}", index + 1);
            assert!(
                !line
                    .chars()
                    .any(|c| c != '\t' && (c <= '\u{1F}' || ('\u{7F}'..='\u{9F}').contains(&c))),
                "{place}: raw control character"
            );
            let decode = |field: &str| unescape(field).unwrap_or_else(|e| panic!("{place}: {e}"));
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["ok", input, expected] => Case {
                    input: decode(input),
                    expected: Some(decode(expected)),
                    place,
                },
                ["none", input] => Case {
                    input: decode(input),
                    expected: None,
                    place,
                },
                _ => {
                    panic!("{place}: expected \"ok<TAB>input<TAB>expected\" or \"none<TAB>input\"")
                }
            }
        })
        .collect()
}

#[test]
fn corpus_escapes_decode_exactly_as_the_format_says() {
    assert_eq!(
        unescape("a\\tb\\\\c\\u{FC}\\u{1F600}").as_deref(),
        Ok("a\tb\\c\u{FC}\u{1F600}")
    );
    for bad in [
        "\\n",
        "\\u{}",
        "\\u{110000}",
        "\\u{D800}",
        "\\u{41",
        "\\",
        "\\u{1234567}",
    ] {
        assert!(unescape(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn parse_url_gives_the_expected_result_for_every_corpus_case() {
    let cases = read_corpus();
    assert!(
        cases.len() >= 21 + 17,
        "the 21 cases of SPEC 12.13 v1.14 and the 17 host rule cases of v1.15"
    );
    let mut inputs = HashSet::new();
    for case in &cases {
        assert!(
            inputs.insert(case.input.as_str()),
            "{}: duplicate input",
            case.place
        );
        assert_eq!(
            parse_url(&case.input),
            case.expected,
            "{}: {:?}",
            case.place,
            case.input
        );
    }
}

/// Inputs built from the characters the rule cares about, in every short combination
/// after a scheme prefix, must give a result and never panic. An accepted result is
/// always the input without surrounding spaces.
#[test]
fn parse_url_is_total_on_short_hostile_texts() {
    let alphabet = [
        "",
        "a",
        "0",
        "1",
        "255",
        "256",
        "0x",
        ".",
        ":",
        "/",
        "?",
        "#",
        "@",
        "[",
        "]",
        "\\",
        "%",
        " ",
        "-",
        "\u{FC}",
        "\u{1F600}",
        "\u{85}",
        "\t",
        "65535",
        "65536",
    ];
    let prefixes = ["", "http:", "http://", "HTTPS://", "mailto:", "javascript:"];
    let mut runs = 0usize;
    for prefix in prefixes {
        for a in alphabet {
            for b in alphabet {
                for c in alphabet {
                    let input = format!("{prefix}{a}{b}{c}");
                    if let Some(url) = parse_url(&input) {
                        assert_eq!(url, input.trim_matches(' '), "{input:?}");
                        assert!(!url.is_empty(), "{input:?}");
                    }
                    runs += 1;
                }
            }
        }
    }
    assert_eq!(runs, prefixes.len() * alphabet.len().pow(3));
}

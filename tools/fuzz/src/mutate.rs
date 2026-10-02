//! Byte level mutation of seed inputs (corpus files or generated programs).

use crate::rng::Rng;

/// Byte sequences that matter to the Ostrel lexer: structure, string syntax,
/// whitespace, invalid UTF-8 and a raw bidi control (D53).
const INTERESTING: &[&[u8]] = &[
    b"{",
    b"}",
    b"(",
    b")",
    b"[",
    b"]",
    b"\"",
    b"\\",
    b"\n",
    b"\r\n",
    b"  ",
    b"\t",
    b"~",
    b":",
    b"//",
    b"/*",
    b"*/",
    b"\\u{",
    b"..",
    b"??",
    b"\x00",
    b"\xff",
    b"\xc0\x80",
    "\u{202E}".as_bytes(),
    "\u{FEFF}".as_bytes(),
    b"9223372036854775808",
    b"fn main()\n",
];

/// Applies between one and `max_steps` random mutations; keeps the result at most
/// `max_len` bytes long.
pub fn mutate(
    rng: &mut Rng,
    input: &mut Vec<u8>,
    other: Option<&[u8]>,
    max_steps: usize,
    max_len: usize,
) {
    let steps = rng.range(1, max_steps.max(1));
    for _ in 0..steps {
        step(rng, input, other);
        if input.len() > max_len {
            input.truncate(max_len);
        }
    }
}

fn step(rng: &mut Rng, input: &mut Vec<u8>, other: Option<&[u8]>) {
    let len = input.len();
    match rng.below(8) {
        // Flip one bit.
        0 if len > 0 => {
            let i = rng.below(len);
            if let Some(b) = input.get_mut(i) {
                *b ^= 1 << rng.below(8);
            }
        }
        // Overwrite one byte with a random value.
        1 if len > 0 => {
            let i = rng.below(len);
            if let Some(b) = input.get_mut(i) {
                *b = rng.below(256) as u8;
            }
        }
        // Delete a range.
        2 if len > 0 => {
            let start = rng.below(len);
            let end = rng.range(start + 1, (start + 64).min(len));
            input.drain(start..end);
        }
        // Duplicate a range in place.
        3 if len > 0 => {
            let start = rng.below(len);
            let end = rng.range(start + 1, (start + 256).min(len));
            let chunk: Vec<u8> = input
                .get(start..end)
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            let times = rng.range(1, 8);
            let at = rng.below(len + 1);
            for _ in 0..times {
                input.splice(at..at, chunk.iter().copied());
            }
        }
        // Splice a range of another input.
        4 => {
            if let Some(o) = other.filter(|o| !o.is_empty()) {
                let start = rng.below(o.len());
                let end = rng.range(start + 1, (start + 512).min(o.len()));
                let at = rng.below(len + 1);
                if let Some(chunk) = o.get(start..end) {
                    input.splice(at..at, chunk.iter().copied());
                }
            } else {
                insert_interesting(rng, input);
            }
        }
        // Swap two lines (changes block structure without breaking tokens).
        5 => swap_lines(rng, input),
        // Insert an interesting token.
        _ => insert_interesting(rng, input),
    }
}

fn insert_interesting(rng: &mut Rng, input: &mut Vec<u8>) {
    let at = rng.below(input.len() + 1);
    let tok = rng.pick(INTERESTING).copied().unwrap_or(b"{");
    input.splice(at..at, tok.iter().copied());
}

fn swap_lines(rng: &mut Rng, input: &mut Vec<u8>) {
    let mut lines: Vec<Vec<u8>> = input.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
    if lines.len() < 2 {
        insert_interesting(rng, input);
        return;
    }
    let a = rng.below(lines.len());
    let b = rng.below(lines.len());
    lines.swap(a, b);
    *input = lines.join(&b'\n');
}

#[cfg(test)]
mod tests {
    use super::mutate;
    use crate::rng::Rng;

    #[test]
    fn mutation_is_deterministic_and_bounded() {
        let seed = b"fn main()\n  let x = \"a{y}b\"\n  print(x)\n".to_vec();
        let other = b"data Room\n  name: Text\n".to_vec();
        for i in 0..2000 {
            let mut a = seed.clone();
            let mut b = seed.clone();
            mutate(&mut Rng::for_input(1, i), &mut a, Some(&other), 8, 128);
            mutate(&mut Rng::for_input(1, i), &mut b, Some(&other), 8, 128);
            assert_eq!(a, b);
            assert!(a.len() <= 128);
        }
    }

    #[test]
    fn empty_input_survives() {
        for i in 0..500 {
            let mut v = Vec::new();
            mutate(&mut Rng::for_input(2, i), &mut v, None, 4, 64);
            assert!(v.len() <= 64);
        }
    }
}

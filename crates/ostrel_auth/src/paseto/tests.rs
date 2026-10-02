use super::*;

// Known answer cases 4-S-1 and 4-S-3 from https://github.com/paseto-standard/test-vectors
// (v4.json, ISC License, Copyright (c) 2021 Paragon Initiative Enterprises). The full
// vector file and its runner are committed separately under tests/vectors/.
const VECTOR_SECRET: &str = "b4cbfb43df4ce210727d953e4a713307fa19bb7d9f85041438d9e11b942a3774\
                             1eb9dbbbbc047c03fd70604e0071f0987e16b28b757225c11f00415d0e20b1a2";
const VECTOR_PUBLIC: &str = "1eb9dbbbbc047c03fd70604e0071f0987e16b28b757225c11f00415d0e20b1a2";
const VECTOR_MESSAGE: &str =
    r#"{"data":"this is a signed message","exp":"2022-01-01T00:00:00+00:00"}"#;
const VECTOR_FOOTER: &str = r#"{"kid":"zVhMiPBP9fRf2snEcT7gFTioeA9COcNy9DfgL1W60haN"}"#;
const VECTOR_IMPLICIT: &str = r#"{"test-vector":"4-S-3"}"#;
const TOKEN_4_S_1: &str = "v4.public.eyJkYXRhIjoidGhpcyBpcyBhIHNpZ25lZCBtZXNzYWdlIiwiZXhwIjoiMjAyMi0wMS0wMVQwMDowMDowMCswMDowMCJ9bg_XBBzds8lTZShVlwwKSgeKpLT3yukTw6JUz3W4h_ExsQV-P0V54zemZDcAxFaSeef1QlXEFtkqxT1ciiQEDA";
const TOKEN_4_S_3: &str = "v4.public.eyJkYXRhIjoidGhpcyBpcyBhIHNpZ25lZCBtZXNzYWdlIiwiZXhwIjoiMjAyMi0wMS0wMVQwMDowMDowMCswMDowMCJ9NPWciuD3d0o5eXJXG5pJy-DiVEoyPYWs1YSTwWHNJq6DZD3je5gf-0M4JR9ipdUSJbIovzmBECeaWmaqcaP0DQ.eyJraWQiOiJ6VmhNaVBCUDlmUmYyc25FY1Q3Z0ZUaW9lQTlDT2NOeTlEZmdMMVc2MGhhTiJ9";

fn hex<const N: usize>(text: &str) -> [u8; N] {
    let text: String = text.split_whitespace().collect();
    assert_eq!(text.len(), 2 * N, "hex length");
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn vector_secret() -> SecretKey {
    SecretKey::from_keypair_bytes(&hex::<64>(VECTOR_SECRET)).unwrap()
}

fn vector_public() -> PublicKey {
    PublicKey::from_bytes(&hex::<32>(VECTOR_PUBLIC)).unwrap()
}

fn test_key(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

/// Replaces the payload segment of a token with the base64url form of `payload`.
fn with_payload(token: &str, payload: &[u8]) -> String {
    let footer = token[V4_PUBLIC_HEADER.len()..].split('.').nth(1);
    let mut out = format!("{V4_PUBLIC_HEADER}{}", URL_SAFE_NO_PAD.encode(payload));
    if let Some(footer) = footer {
        out.push('.');
        out.push_str(footer);
    }
    out
}

fn payload_of(token: &str) -> Vec<u8> {
    let text = token[V4_PUBLIC_HEADER.len()..].split('.').next().unwrap();
    URL_SAFE_NO_PAD.decode(text).unwrap()
}

// PAE

#[test]
fn pae_matches_the_specification_examples() {
    assert_eq!(pae(&[]), vec![0; 8]);
    assert_eq!(
        pae(&[b""]),
        [vec![1, 0, 0, 0, 0, 0, 0, 0], vec![0; 8]].concat()
    );
    assert_eq!(
        pae(&[b"test"]),
        [
            vec![1, 0, 0, 0, 0, 0, 0, 0],
            vec![4, 0, 0, 0, 0, 0, 0, 0],
            b"test".to_vec()
        ]
        .concat()
    );
}

#[test]
fn pae_separates_pieces_that_concatenate_equally() {
    assert_ne!(pae(&[b"ab", b"c"]), pae(&[b"a", b"bc"]));
    assert_ne!(pae(&[b"abc"]), pae(&[b"abc", b""]));
}

#[test]
fn le64_clears_the_most_significant_bit() {
    assert_eq!(le64(usize::MAX)[7], 0x7f);
}

// Official vectors

#[test]
fn vector_keypair_bytes_match_the_public_key() {
    assert_eq!(vector_secret().public_key(), vector_public());
}

#[test]
fn signs_vector_4_s_1_byte_for_byte() {
    let token = sign(&vector_secret(), VECTOR_MESSAGE.as_bytes(), b"", b"");
    assert_eq!(token, TOKEN_4_S_1);
}

#[test]
fn signs_vector_4_s_3_byte_for_byte() {
    let token = sign(
        &vector_secret(),
        VECTOR_MESSAGE.as_bytes(),
        VECTOR_FOOTER.as_bytes(),
        VECTOR_IMPLICIT.as_bytes(),
    );
    assert_eq!(token, TOKEN_4_S_3);
}

#[test]
fn verifies_vector_4_s_1() {
    let verified = verify(&vector_public(), TOKEN_4_S_1, b"").unwrap();
    assert_eq!(verified.message, VECTOR_MESSAGE.as_bytes());
    assert!(verified.footer.is_empty());
}

#[test]
fn verifies_vector_4_s_3() {
    let verified = verify(&vector_public(), TOKEN_4_S_3, VECTOR_IMPLICIT.as_bytes()).unwrap();
    assert_eq!(verified.message, VECTOR_MESSAGE.as_bytes());
    assert_eq!(verified.footer, VECTOR_FOOTER.as_bytes());
}

#[test]
fn vector_4_s_3_needs_its_implicit_assertion() {
    assert_eq!(
        verify(&vector_public(), TOKEN_4_S_3, b""),
        Err(Error::BadSignature)
    );
}

// Round trips

#[test]
fn round_trips_message_footer_and_implicit_assertion() {
    let key = test_key(7);
    for (message, footer, implicit) in [
        (&b"{}"[..], &b""[..], &b""[..]),
        (b"claims", b"kid", b""),
        (b"claims", b"", b"session"),
        (b"\x00\x01\xff binary", b"\x00", b"\xff"),
    ] {
        let token = sign(&key, message, footer, implicit);
        let verified = verify(&key.public_key(), &token, implicit).unwrap();
        assert_eq!(verified.message, message);
        assert_eq!(verified.footer, footer);
    }
}

#[test]
fn round_trips_an_empty_message() {
    let key = test_key(1);
    let token = sign(&key, b"", b"", b"");
    assert_eq!(payload_of(&token).len(), SIGNATURE_LEN);
    let verified = verify(&key.public_key(), &token, b"").unwrap();
    assert!(verified.message.is_empty());
}

#[test]
fn leaves_an_empty_footer_out_of_the_token() {
    let token = sign(&test_key(1), b"m", b"", b"");
    assert_eq!(token.matches('.').count(), 2);
    let token = sign(&test_key(1), b"m", b"f", b"");
    assert_eq!(token.matches('.').count(), 3);
}

#[test]
fn round_trips_a_message_close_to_the_length_limit() {
    let key = test_key(2);
    // base64url turns 3 bytes into 4 characters.
    let room = (MAX_TOKEN_LEN - V4_PUBLIC_HEADER.len()) / 4 * 3 - SIGNATURE_LEN;
    let message = vec![b'a'; room];
    let token = sign(&key, &message, b"", b"");
    assert!(token.len() <= MAX_TOKEN_LEN);
    assert_eq!(
        verify(&key.public_key(), &token, b"").unwrap().message,
        message
    );
}

// Rejections: key, signature, implicit assertion

#[test]
fn rejects_a_token_signed_with_another_key() {
    let token = sign(&test_key(1), b"m", b"", b"");
    assert_eq!(
        verify(&test_key(2).public_key(), &token, b""),
        Err(Error::BadSignature)
    );
}

#[test]
fn rejects_a_different_implicit_assertion() {
    let key = test_key(1);
    let token = sign(&key, b"m", b"", b"a");
    assert_eq!(
        verify(&key.public_key(), &token, b"b"),
        Err(Error::BadSignature)
    );
    assert_eq!(
        verify(&key.public_key(), &token, b""),
        Err(Error::BadSignature)
    );
}

#[test]
fn rejects_a_changed_message() {
    let key = test_key(1);
    let token = sign(&key, b"{\"sub\":\"alice\"}", b"", b"");
    let mut payload = payload_of(&token);
    payload[9] = b'm';
    assert_eq!(
        verify(&key.public_key(), &with_payload(&token, &payload), b""),
        Err(Error::BadSignature)
    );
}

#[test]
fn rejects_a_changed_signature() {
    let key = test_key(1);
    let token = sign(&key, b"m", b"", b"");
    let mut payload = payload_of(&token);
    let last = payload.len() - 1;
    payload[last] ^= 0x01;
    assert!(verify(&key.public_key(), &with_payload(&token, &payload), b"").is_err());
}

#[test]
fn rejects_a_changed_removed_or_added_footer() {
    let key = test_key(1);
    let with_footer = sign(&key, b"m", b"kid-1", b"");
    let other_footer = sign(&key, b"m", b"kid-2", b"");
    let without_footer = sign(&key, b"m", b"", b"");
    let public = key.public_key();

    let (body, _) = with_footer.rsplit_once('.').unwrap();
    let (_, foreign) = other_footer.rsplit_once('.').unwrap();
    let swapped = format!("{body}.{foreign}");
    assert_eq!(verify(&public, &swapped, b""), Err(Error::BadSignature));
    assert_eq!(verify(&public, body, b""), Err(Error::BadSignature));
    let added = format!("{without_footer}.{foreign}");
    assert_eq!(verify(&public, &added, b""), Err(Error::BadSignature));
}

#[test]
fn rejects_a_message_moved_into_the_footer() {
    // The signed parts are length prefixed, so shifting bytes between them must fail.
    let key = test_key(1);
    let token = sign(&key, b"ab", b"c", b"");
    let payload = payload_of(&token);
    let mut moved = b"a".to_vec();
    moved.extend_from_slice(&payload[2..]);
    let forged = format!(
        "{V4_PUBLIC_HEADER}{}.{}",
        URL_SAFE_NO_PAD.encode(&moved),
        URL_SAFE_NO_PAD.encode(b"bc")
    );
    assert_eq!(
        verify(&key.public_key(), &forged, b""),
        Err(Error::BadSignature)
    );
}

#[test]
fn rejects_the_trivial_forgery_under_a_small_order_key() {
    // The identity point is a valid encoding, but with R = identity and S = 0 every
    // message would verify under lax rules. verify_strict must refuse it.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let public = PublicKey::from_bytes(&identity).unwrap();
    let mut payload = b"{\"sub\":\"admin\"}".to_vec();
    payload.extend_from_slice(&identity);
    payload.extend_from_slice(&[0u8; 32]);
    let forged = format!("{V4_PUBLIC_HEADER}{}", URL_SAFE_NO_PAD.encode(&payload));
    assert_eq!(verify(&public, &forged, b""), Err(Error::BadSignature));
}

#[test]
fn rejects_a_signature_with_a_non_canonical_scalar() {
    // Adding the group order L to S yields the same point equation; the encoding must
    // still be rejected, otherwise one token would have two valid forms.
    const L: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
    ];
    let key = test_key(3);
    let token = sign(&key, b"m", b"", b"");
    let mut payload = payload_of(&token);
    let s_start = payload.len() - 32;
    let mut carry = 0u16;
    for (i, l) in L.iter().enumerate() {
        let sum = u16::from(payload[s_start + i]) + u16::from(*l) + carry;
        payload[s_start + i] = (sum & 0xff) as u8;
        carry = sum >> 8;
    }
    assert!(verify(&key.public_key(), &with_payload(&token, &payload), b"").is_err());
}

// Rejections: header

#[test]
fn rejects_other_versions_and_purposes() {
    let key = test_key(1);
    let token = sign(&key, b"m", b"", b"");
    let body = &token[V4_PUBLIC_HEADER.len()..];
    for header in [
        "v4.local.",
        "v3.public.",
        "v2.public.",
        "v1.public.",
        "V4.PUBLIC.",
        "v4.Public.",
        "v4public.",
        " v4.public.",
        "k4.public.",
    ] {
        let other = format!("{header}{body}");
        assert_eq!(
            verify(&key.public_key(), &other, b""),
            Err(Error::UnsupportedHeader),
            "{header}"
        );
    }
}

#[test]
fn rejects_tokens_without_a_full_header() {
    let public = test_key(1).public_key();
    for token in ["", "v4", "v4.public", "v4.", "\u{feff}v4.public.AAAA", "ä"] {
        assert_eq!(
            verify(&public, token, b""),
            Err(Error::UnsupportedHeader),
            "{token:?}"
        );
    }
}

// Rejections: shape and encoding

#[test]
fn rejects_wrong_segment_counts() {
    let key = test_key(1);
    let token = sign(&key, b"m", b"f", b"");
    let public = key.public_key();
    assert_eq!(verify(&public, "v4.public.", b""), Err(Error::Malformed));
    assert_eq!(verify(&public, "v4.public..", b""), Err(Error::Malformed));
    assert_eq!(
        verify(&public, "v4.public..AAAA", b""),
        Err(Error::Malformed)
    );
    assert_eq!(
        verify(&public, &format!("{token}.AAAA"), b""),
        Err(Error::Malformed)
    );
    assert_eq!(
        verify(&public, &format!("{token}."), b""),
        Err(Error::Malformed)
    );
}

#[test]
fn rejects_an_empty_footer_segment_on_a_valid_token() {
    let key = test_key(1);
    let token = sign(&key, b"m", b"", b"");
    assert_eq!(
        verify(&key.public_key(), &format!("{token}."), b""),
        Err(Error::Malformed)
    );
}

#[test]
fn rejects_non_canonical_base64url() {
    let key = test_key(1);
    let public = key.public_key();
    let token = sign(&key, b"m", b"", b"");
    let body = &token[V4_PUBLIC_HEADER.len()..];

    // 65 payload bytes encode to 87 characters with 2 unused trailing bits.
    assert_eq!(body.len(), 87);
    let padded = format!("{token}=");
    assert_eq!(verify(&public, &padded, b""), Err(Error::Encoding));

    let standard = token.replace('-', "+").replace('_', "/");
    if standard != token {
        assert_eq!(verify(&public, &standard, b""), Err(Error::Encoding));
    }
    for bad in [" ", "\n", "*", "é"] {
        let other = format!("{token}{bad}");
        assert_eq!(
            verify(&public, &other, b""),
            Err(Error::Encoding),
            "{bad:?}"
        );
    }

    // Setting an unused trailing bit keeps the bytes but changes the text.
    let last = body.chars().last().unwrap();
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let index = alphabet.find(last).unwrap();
    let flipped = alphabet.chars().nth(index ^ 0b01).unwrap();
    let mut other = token.clone();
    other.pop();
    other.push(flipped);
    assert_eq!(verify(&public, &other, b""), Err(Error::Encoding));
}

#[test]
fn rejects_an_impossible_base64url_length() {
    let public = test_key(1).public_key();
    // A length of 1 modulo 4 never results from encoding.
    let token = format!("{V4_PUBLIC_HEADER}{}", "A".repeat(89));
    assert_eq!(verify(&public, &token, b""), Err(Error::Encoding));
}

#[test]
fn rejects_payloads_shorter_than_a_signature() {
    let public = test_key(1).public_key();
    for len in [1usize, 32, 63] {
        let token = format!(
            "{V4_PUBLIC_HEADER}{}",
            URL_SAFE_NO_PAD.encode(vec![0u8; len])
        );
        assert_eq!(
            verify(&public, &token, b""),
            Err(Error::PayloadTooShort),
            "{len}"
        );
    }
}

#[test]
fn rejects_tokens_over_the_length_limit_before_decoding() {
    let public = test_key(1).public_key();
    let at_limit = format!("{V4_PUBLIC_HEADER}{}", "A".repeat(MAX_TOKEN_LEN - 10));
    assert_eq!(at_limit.len(), MAX_TOKEN_LEN);
    assert_ne!(verify(&public, &at_limit, b""), Err(Error::TooLong));
    let over = format!("{at_limit}A");
    assert_eq!(verify(&public, &over, b""), Err(Error::TooLong));
    let huge = "x".repeat(1 << 20);
    assert_eq!(verify(&public, &huge, b""), Err(Error::TooLong));
}

#[test]
fn mutated_tokens_never_verify() {
    // Seeded xorshift: replaces, inserts or deletes one byte of a valid token and checks
    // that verification fails without panicking unless the token is unchanged.
    let key = test_key(9);
    let public = key.public_key();
    let token = sign(&key, b"{\"sub\":\"u1\",\"kid\":\"d1\"}", b"footer", b"");
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.=+/ \x00";
    for _ in 0..3000 {
        let mut bytes = token.clone().into_bytes();
        let pos = (next() % bytes.len() as u64) as usize;
        let ch = charset[(next() % charset.len() as u64) as usize];
        match next() % 3 {
            0 => bytes[pos] = ch,
            1 => bytes.insert(pos, ch),
            _ => {
                bytes.remove(pos);
            }
        }
        let mutated = String::from_utf8(bytes).unwrap();
        let result = verify(&public, &mutated, b"");
        if mutated == token {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err(), "{mutated}");
        }
    }
}

// Keys

#[test]
fn rejects_keypair_bytes_with_a_foreign_public_half() {
    let mut bytes = hex::<64>(VECTOR_SECRET);
    bytes[63] ^= 0x01;
    assert_eq!(
        SecretKey::from_keypair_bytes(&bytes).map(|_| ()),
        Err(Error::InvalidKey)
    );
}

#[test]
fn rejects_public_key_bytes_that_are_not_a_curve_point() {
    let invalid = (2u8..=40).find(|y| {
        let mut bytes = [0u8; 32];
        bytes[0] = *y;
        PublicKey::from_bytes(&bytes).is_err()
    });
    assert!(invalid.is_some());
}

#[test]
fn public_key_bytes_round_trip() {
    let public = vector_public();
    assert_eq!(public.to_bytes(), hex::<32>(VECTOR_PUBLIC));
}

#[test]
fn debug_output_does_not_contain_the_secret_seed() {
    let text = format!("{:?}", vector_secret());
    assert!(!text.contains(&VECTOR_SECRET[..64]));
    assert!(text.contains(VECTOR_PUBLIC));
}

#[test]
fn errors_have_a_message() {
    for error in [
        Error::TooLong,
        Error::UnsupportedHeader,
        Error::Malformed,
        Error::Encoding,
        Error::PayloadTooShort,
        Error::BadSignature,
        Error::InvalidKey,
    ] {
        assert!(!error.to_string().is_empty());
    }
}

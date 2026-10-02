//! PASETO `v4.public`: Ed25519 signatures over the pre authentication encoding.
//!
//! Token layout: `v4.public.` + base64url(message || signature) and, when the footer is
//! not empty, `.` + base64url(footer). The signature covers
//! `PAE("v4.public.", message, footer, implicit_assertion)`.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};

/// Header of every token this crate signs or accepts.
pub const V4_PUBLIC_HEADER: &str = "v4.public.";

/// Upper bound for the length of a token in bytes, checked before any decoding.
///
/// Session tokens carry a few short claims and are far below this bound; the limit keeps
/// hostile input from costing more than a small, fixed amount of work and memory.
pub const MAX_TOKEN_LEN: usize = 8 * 1024;

const SIGNATURE_LEN: usize = 64;

/// Why a key or token was rejected.
///
/// The variants are meant for logs and tests. A server must not tell the client which
/// check failed; every variant means the same thing to the client: no principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The token is longer than [`MAX_TOKEN_LEN`].
    TooLong,
    /// The token is not `v4.public` (another version, another purpose, or no header).
    UnsupportedHeader,
    /// The token does not have the shape `v4.public.<payload>[.<footer>]`.
    Malformed,
    /// A segment is not canonical unpadded base64url.
    Encoding,
    /// The decoded payload is too short to hold a signature.
    PayloadTooShort,
    /// The signature does not verify under the given public key.
    BadSignature,
    /// Key bytes do not form a valid Ed25519 key.
    InvalidKey,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Error::TooLong => "token is too long",
            Error::UnsupportedHeader => "token is not a v4.public token",
            Error::Malformed => "token is malformed",
            Error::Encoding => "token segment is not canonical base64url",
            Error::PayloadTooShort => "token payload is too short to hold a signature",
            Error::BadSignature => "token signature is invalid",
            Error::InvalidKey => "key is not a valid Ed25519 key",
        };
        f.write_str(text)
    }
}

impl std::error::Error for Error {}

/// An Ed25519 secret key that signs `v4.public` tokens.
#[derive(Clone)]
pub struct SecretKey(SigningKey);

impl SecretKey {
    /// Builds the key from its 32 byte seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        SecretKey(SigningKey::from_bytes(seed))
    }

    /// Builds the key from the 64 byte form `seed || public key`, as used by the PASETO
    /// test vectors. Fails when the public half does not belong to the seed.
    pub fn from_keypair_bytes(bytes: &[u8; 64]) -> Result<Self, Error> {
        SigningKey::from_keypair_bytes(bytes)
            .map(SecretKey)
            .map_err(|_| Error::InvalidKey)
    }

    /// The public key that verifies tokens signed with this key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print secret material, not even in debug output.
        f.debug_struct("SecretKey")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

/// An Ed25519 public key that verifies `v4.public` tokens.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Builds the key from its 32 byte encoding. Fails for bytes that are not a point
    /// on the curve.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, Error> {
        VerifyingKey::from_bytes(bytes)
            .map(PublicKey)
            .map_err(|_| Error::InvalidKey)
    }

    /// The 32 byte encoding of the key.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey(")?;
        for byte in self.0.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

/// The content of a token whose signature verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The signed message (for session tokens: the JSON claims).
    pub message: Vec<u8>,
    /// The footer, empty when the token has none. It is covered by the signature.
    pub footer: Vec<u8>,
}

/// Pre authentication encoding (PAE) of the PASETO specification.
///
/// The output is the little endian 64 bit count of pieces, then for each piece its
/// little endian 64 bit length followed by its bytes. The most significant bit of every
/// length is cleared, as the specification requires.
pub fn pae(pieces: &[&[u8]]) -> Vec<u8> {
    let total: usize = pieces.iter().map(|piece| 8 + piece.len()).sum();
    let mut out = Vec::with_capacity(8 + total);
    out.extend_from_slice(&le64(pieces.len()));
    for piece in pieces {
        out.extend_from_slice(&le64(piece.len()));
        out.extend_from_slice(piece);
    }
    out
}

fn le64(n: usize) -> [u8; 8] {
    // usize is at most 64 bits on every supported target.
    ((n as u64) & (u64::MAX >> 1)).to_le_bytes()
}

/// Signs `message` as a `v4.public` token.
///
/// `footer` is appended in clear text and covered by the signature; an empty footer is
/// left out of the token. `implicit` is covered by the signature but not stored in the
/// token; the verifier must supply the same bytes.
pub fn sign(key: &SecretKey, message: &[u8], footer: &[u8], implicit: &[u8]) -> String {
    let signed = pae(&[V4_PUBLIC_HEADER.as_bytes(), message, footer, implicit]);
    let signature = key.0.sign(&signed);

    let mut payload = Vec::with_capacity(message.len() + SIGNATURE_LEN);
    payload.extend_from_slice(message);
    payload.extend_from_slice(&signature.to_bytes());

    let mut token = String::from(V4_PUBLIC_HEADER);
    URL_SAFE_NO_PAD.encode_string(&payload, &mut token);
    if !footer.is_empty() {
        token.push('.');
        URL_SAFE_NO_PAD.encode_string(footer, &mut token);
    }
    token
}

/// Verifies a `v4.public` token and returns its message and footer.
///
/// Rejects tokens that are too long, of another version or purpose, not of the form
/// `v4.public.<payload>[.<footer>]`, not canonical unpadded base64url, too short, or
/// whose signature does not verify under `key` with the given implicit assertion.
/// An empty footer segment (a trailing `.`) is rejected, because [`sign`] never
/// produces it and each signed token has exactly one encoding.
///
/// The caller still has to check the claims in the message (for example `exp`) and, if
/// it expects a particular footer, compare it.
pub fn verify(key: &PublicKey, token: &str, implicit: &[u8]) -> Result<Verified, Error> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(Error::TooLong);
    }
    let rest = token
        .strip_prefix(V4_PUBLIC_HEADER)
        .ok_or(Error::UnsupportedHeader)?;

    let mut segments = rest.split('.');
    let payload_text = segments.next().ok_or(Error::Malformed)?;
    let footer_text = segments.next();
    if segments.next().is_some() || payload_text.is_empty() || footer_text == Some("") {
        return Err(Error::Malformed);
    }

    let payload = decode(payload_text)?;
    let footer = match footer_text {
        Some(text) => decode(text)?,
        None => Vec::new(),
    };

    let split = payload
        .len()
        .checked_sub(SIGNATURE_LEN)
        .ok_or(Error::PayloadTooShort)?;
    let (message, signature) = payload.split_at_checked(split).ok_or(Error::Malformed)?;
    let signature = Signature::from_slice(signature).map_err(|_| Error::Malformed)?;

    let signed = pae(&[V4_PUBLIC_HEADER.as_bytes(), message, &footer, implicit]);
    // verify_strict also rejects weak public keys and non canonical signature points,
    // so a signature cannot be altered into a second valid form.
    key.0
        .verify_strict(&signed, &signature)
        .map_err(|_| Error::BadSignature)?;

    Ok(Verified {
        message: message.to_vec(),
        footer,
    })
}

/// Decodes unpadded base64url and rejects padding, foreign characters and non zero
/// trailing bits, so every byte string has exactly one accepted encoding.
fn decode(text: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD.decode(text).map_err(|_| Error::Encoding)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests;

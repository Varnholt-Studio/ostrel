//! Binding of session tokens to one app (ARCHITECTURE 8, D128, AC-63 (d)).
//!
//! Every session token is signed and verified with the PASETO v4 implicit assertion
//! `ostrel-v1:<app name>:<app_id>`, where `app_id` is 16 random bytes in lowercase hex that
//! `ostrel keygen` stores in the same key file as the server key. The assertion is covered
//! by the signature but never part of the token, so a token minted for another app fails
//! verification, also when that app runs with a copy of the same server key: its app name
//! or its `app_id` differs.

use std::fmt;

use crate::paseto::{Error, PublicKey, SecretKey, Verified, sign, verify};

/// Length of an `app_id` in bytes.
pub const APP_ID_LEN: usize = 16;

/// Prefix of every implicit assertion; names the version of the binding.
const ASSERTION_PREFIX: &str = "ostrel-v1:";

/// The app a server process verifies tokens for: its name and its `app_id`.
///
/// It is the only way other crates sign or verify a PASETO token with this crate: the plain
/// `sign` and `verify` that take a free assertion are private to the crate, so no caller can
/// forget the binding or build the assertion differently.
#[derive(Clone, PartialEq, Eq)]
pub struct AppBinding {
    assertion: String,
}

impl AppBinding {
    /// The binding for the app `name` with `app_id`, or `None` when `name` is empty or
    /// contains a byte other than an ASCII letter, digit or `_`.
    ///
    /// App names are identifiers (SYNTAX: `app Chat`), which have no length limit (SPEC 12)
    /// and consist of exactly these bytes, so this never refuses the name of a program that
    /// compiles. Refusing every other byte keeps the assertion unambiguous: a name can never
    /// contain the `:` that separates it from the `app_id`. The assertion is not part of the
    /// token, so a long name does not make the token longer.
    pub fn new(name: &str, app_id: &[u8; APP_ID_LEN]) -> Option<Self> {
        let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !ok {
            return None;
        }
        let mut assertion =
            String::with_capacity(ASSERTION_PREFIX.len() + name.len() + 1 + 2 * APP_ID_LEN);
        assertion.push_str(ASSERTION_PREFIX);
        assertion.push_str(name);
        assertion.push(':');
        for byte in app_id {
            push_hex(&mut assertion, *byte);
        }
        Some(AppBinding { assertion })
    }

    /// The implicit assertion as UTF-8 bytes, `ostrel-v1:<app name>:<app_id hex>`.
    pub fn assertion(&self) -> &[u8] {
        self.assertion.as_bytes()
    }

    /// Signs a session token for this app.
    pub fn sign(&self, key: &SecretKey, message: &[u8], footer: &[u8]) -> String {
        sign(key, message, footer, self.assertion())
    }

    /// Verifies a session token for this app. A token signed for any other app fails with
    /// [`Error::BadSignature`], like a forged one.
    pub fn verify(&self, key: &PublicKey, token: &str) -> Result<Verified, Error> {
        verify(key, token, self.assertion())
    }
}

impl fmt::Debug for AppBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AppBinding({})", self.assertion)
    }
}

fn push_hex(out: &mut String, byte: u8) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for nibble in [byte >> 4, byte & 0x0f] {
        // A nibble is below 16, so the lookup always hits.
        if let Some(&digit) = DIGITS.get(usize::from(nibble)) {
            out.push(char::from(digit));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: [u8; APP_ID_LEN] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0xff,
    ];
    const ID_B: [u8; APP_ID_LEN] = [0x5a; APP_ID_LEN];
    const MESSAGE: &[u8] = br#"{"sub":"00000000000000000000000000000001","kid":"dev-a"}"#;

    fn binding(name: &str, id: &[u8; APP_ID_LEN]) -> AppBinding {
        AppBinding::new(name, id).unwrap_or_else(|| unreachable!("valid app name"))
    }

    fn server_key() -> SecretKey {
        SecretKey::from_seed(&[7; 32])
    }

    #[test]
    fn assertion_has_the_documented_form() {
        assert_eq!(
            binding("Chat", &ID_A).assertion(),
            b"ostrel-v1:Chat:000102030405060708090a0b0c0d0eff"
        );
        assert_eq!(
            format!("{:?}", binding("Chat", &ID_A)),
            "AppBinding(ostrel-v1:Chat:000102030405060708090a0b0c0d0eff)"
        );
    }

    #[test]
    fn app_names_are_identifiers() {
        for good in ["Chat", "Issues2", "my_app", "A"] {
            assert!(AppBinding::new(good, &ID_A).is_some(), "{good:?}");
        }
        for bad in ["", "a:b", "Chat:", "a b", "a-b", "Ch\u{e4}t", "a\0"] {
            assert!(AppBinding::new(bad, &ID_A).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn long_app_names_are_accepted() {
        // uma #2794 B1: identifiers have no length limit, `app` with a 200 byte name compiles.
        let name = "A".repeat(200);
        let app = binding(&name, &ID_A);
        let mut expected = format!("ostrel-v1:{name}:").into_bytes();
        expected.extend_from_slice(b"000102030405060708090a0b0c0d0eff");
        assert_eq!(app.assertion(), expected.as_slice());
        let token = app.sign(&server_key(), MESSAGE, b"");
        let verified = app.verify(&server_key().public_key(), &token);
        assert_eq!(verified.map(|v| v.message), Ok(MESSAGE.to_vec()));
        let shorter = binding(&"A".repeat(199), &ID_A);
        assert_eq!(
            shorter
                .verify(&server_key().public_key(), &token)
                .map(|v| v.message),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn token_verifies_for_its_own_app() {
        let app = binding("Chat", &ID_A);
        let token = app.sign(&server_key(), MESSAGE, b"");
        let verified = app.verify(&server_key().public_key(), &token);
        assert_eq!(verified.map(|v| v.message), Ok(MESSAGE.to_vec()));
    }

    #[test]
    fn token_of_another_app_fails_with_a_copied_key() {
        // Same server key in both apps: only the binding tells them apart.
        let key = server_key();
        let token = binding("Chat", &ID_A).sign(&key, MESSAGE, b"");
        for other in [
            binding("Issues", &ID_A),
            binding("Chat", &ID_B),
            binding("Chat2", &ID_A),
            binding("chat", &ID_A),
        ] {
            assert_eq!(
                other.verify(&key.public_key(), &token).map(|v| v.message),
                Err(Error::BadSignature),
                "{other:?}"
            );
        }
    }

    #[test]
    fn token_without_assertion_fails() {
        let key = server_key();
        let token = sign(&key, MESSAGE, b"", b"");
        assert_eq!(
            binding("Chat", &ID_A)
                .verify(&key.public_key(), &token)
                .map(|v| v.message),
            Err(Error::BadSignature)
        );
    }
}

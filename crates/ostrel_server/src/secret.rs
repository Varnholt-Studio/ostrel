//! The `replica_secret` of apps without `auth` (ARCHITECTURE 5.3, 5.4, D111).
//!
//! The server issues the secret in `Welcome` and keeps only its SHA-256. The secret itself
//! never reaches a log: [`ReplicaSecret`] has no `Display`, and its `Debug` output is
//! `<redacted>` (D122 F9).

use std::fmt;

use sha2::{Digest, Sha256};

/// A replica secret as it travels in `Hello` and `Welcome`.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplicaSecret(String);

impl ReplicaSecret {
    /// Wraps the secret text of a frame.
    pub fn new(secret: String) -> Self {
        ReplicaSecret(secret)
    }

    /// The secret text, for the frame that carries it and nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// SHA-256 of the UTF-8 bytes of the secret, the only form the server stores (D111).
    pub fn sha256(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }

    /// Whether this secret hashes to `stored`. Every byte of the digest is compared, also
    /// after the first difference, so the time does not tell how many bytes matched.
    pub fn matches(&self, stored: &[u8; 32]) -> bool {
        let digest = self.sha256();
        let mut diff = 0u8;
        for (a, b) in digest.iter().zip(stored.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl fmt::Debug for ReplicaSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReplicaSecret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256_hex;

    #[test]
    fn debug_never_shows_the_secret() {
        let secret = ReplicaSecret::new("rs-0123456789abcdef".to_owned());
        let debug = format!("{secret:?} {:?}", Some(&secret));
        assert!(!debug.contains("0123456789abcdef"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn stored_form_is_sha256_of_the_text() {
        let secret = ReplicaSecret::new("abc".to_owned());
        assert_eq!(
            sha256_hex(&secret.sha256()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(secret.matches(&secret.sha256()));
        assert!(!ReplicaSecret::new("abd".to_owned()).matches(&secret.sha256()));
        assert_eq!(secret.expose(), "abc");
    }
}

//! The principal of a request (ARCHITECTURE 8, AC-12, D18, D127).
//!
//! Order of precedence, fixed by D127:
//!
//! 1. A token is present and valid: its user is the principal, whatever else the request
//!    carries.
//! 2. A token is present and invalid: the request fails with
//!    [`IdentityError::InvalidToken`]. It never falls back to the development identity or to
//!    an anonymous principal.
//! 3. No token: in a build with the Cargo feature `dev-identity` (debug builds only, see the
//!    crate root) the development identity the test harness supplied is the principal. In
//!    every other build the development identity is ignored and the request is anonymous.

use std::fmt;

use ostrel_core::ids::RowId;

/// Whether this build honours a development identity (feature `dev-identity`, D127).
pub const DEV_IDENTITY_ENABLED: bool = cfg!(feature = "dev-identity");

/// What a request presents to identify its user.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    token: Option<String>,
    dev_identity: Option<RowId>,
}

impl Credentials {
    /// A request without any identity.
    pub fn anonymous() -> Self {
        Credentials::default()
    }

    /// Adds the session token of the request.
    pub fn with_token(mut self, token: String) -> Self {
        self.token = Some(token);
        self
    }

    /// Adds the development identity the test harness supplied. Builds without the feature
    /// `dev-identity` accept it and ignore it.
    pub fn with_dev_identity(mut self, user: RowId) -> Self {
        self.dev_identity = Some(user);
        self
    }

    /// Whether the request carries a token.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("dev_identity", &self.dev_identity.map(RowId::to_hex))
            .finish()
    }
}

/// Checks a session token and names its user.
///
/// ASSUMPTION: the implementation on PASETO v4.public (`ostrel_auth::verify`, the claims of
/// ARCHITECTURE 8, the key status of D102 and the implicit assertion of D128) is the auth
/// task of T4; this crate only fixes how its answer decides the principal.
pub trait TokenVerifier {
    /// The user of `token`, or why the token is not accepted.
    fn verify(&self, token: &str) -> Result<RowId, TokenRejected>;
}

/// Why a token was not accepted. The text is for the server log and never contains the
/// token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRejected(pub String);

/// A verifier for servers of apps without `auth`: there are no tokens, so every token is
/// rejected (ARCHITECTURE 8: an app without `auth` gets no auth code).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTokens;

impl TokenVerifier for NoTokens {
    fn verify(&self, _token: &str) -> Result<RowId, TokenRejected> {
        Err(TokenRejected("this app has no auth".to_owned()))
    }
}

/// The request could not be given a principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The request carried a token that did not verify.
    InvalidToken(TokenRejected),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::InvalidToken(why) => write!(f, "invalid token: {}", why.0),
        }
    }
}

impl std::error::Error for IdentityError {}

/// The principal of a request with `credentials`, by the order of the module documentation.
/// `Ok(None)` is an anonymous request.
pub fn resolve_principal(
    credentials: &Credentials,
    verifier: &dyn TokenVerifier,
) -> Result<Option<RowId>, IdentityError> {
    if let Some(token) = &credentials.token {
        return verifier
            .verify(token)
            .map(Some)
            .map_err(IdentityError::InvalidToken);
    }
    Ok(dev_identity(credentials))
}

#[cfg(feature = "dev-identity")]
fn dev_identity(credentials: &Credentials) -> Option<RowId> {
    credentials.dev_identity
}

#[cfg(not(feature = "dev-identity"))]
fn dev_identity(_credentials: &Credentials) -> Option<RowId> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "v4.public.valid-session-token";

    fn user(n: u8) -> RowId {
        RowId::from_hex(&format!("{n:032x}")).unwrap_or_else(|_| unreachable!("valid hex"))
    }

    /// Accepts exactly [`TOKEN`] as user 7.
    struct OneToken;

    impl TokenVerifier for OneToken {
        fn verify(&self, token: &str) -> Result<RowId, TokenRejected> {
            if token == TOKEN {
                Ok(user(7))
            } else {
                Err(TokenRejected("signature does not verify".to_owned()))
            }
        }
    }

    #[test]
    fn no_credentials_is_anonymous() {
        assert_eq!(
            resolve_principal(&Credentials::anonymous(), &OneToken),
            Ok(None)
        );
    }

    #[test]
    fn valid_token_wins_over_the_dev_identity() {
        let creds = Credentials::anonymous()
            .with_dev_identity(user(1))
            .with_token(TOKEN.to_owned());
        assert_eq!(resolve_principal(&creds, &OneToken), Ok(Some(user(7))));
    }

    #[test]
    fn invalid_token_never_falls_back() {
        let creds = Credentials::anonymous()
            .with_dev_identity(user(1))
            .with_token("v4.public.forged".to_owned());
        assert_eq!(
            resolve_principal(&creds, &OneToken),
            Err(IdentityError::InvalidToken(TokenRejected(
                "signature does not verify".to_owned()
            )))
        );
        let alone = Credentials::anonymous().with_token("v4.public.forged".to_owned());
        assert!(resolve_principal(&alone, &OneToken).is_err());
    }

    #[test]
    fn apps_without_auth_reject_every_token() {
        let creds = Credentials::anonymous().with_token(TOKEN.to_owned());
        assert!(resolve_principal(&creds, &NoTokens).is_err());
        assert_eq!(
            resolve_principal(&Credentials::anonymous(), &NoTokens),
            Ok(None)
        );
    }

    #[test]
    fn debug_never_shows_the_token() {
        let creds = Credentials::anonymous().with_token(TOKEN.to_owned());
        let debug = format!("{creds:?}");
        assert!(!debug.contains("valid-session-token"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(creds.has_token());
    }

    /// Groundwork for AC-12 (the release proof is T133): a build without the feature
    /// `dev-identity`, which every release build is, treats a supplied development identity as
    /// anonymous.
    #[cfg(not(feature = "dev-identity"))]
    #[test]
    fn build_without_feature_treats_dev_identity_as_anonymous() {
        const { assert!(!DEV_IDENTITY_ENABLED) };
        let creds = Credentials::anonymous().with_dev_identity(user(1));
        assert_eq!(resolve_principal(&creds, &OneToken), Ok(None));
    }

    /// Groundwork for AC-12 (the release proof is T133): with the feature (debug builds only) the
    /// development identity applies when no token is present.
    #[cfg(feature = "dev-identity")]
    #[test]
    fn debug_build_with_feature_uses_dev_identity() {
        const { assert!(DEV_IDENTITY_ENABLED) };
        let creds = Credentials::anonymous().with_dev_identity(user(1));
        assert_eq!(resolve_principal(&creds, &OneToken), Ok(Some(user(1))));
    }
}

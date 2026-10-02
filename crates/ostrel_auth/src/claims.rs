//! Checks of the registered claims of a session token: `sub`, `kid`, `exp` and `nbf`.
//!
//! A session token carries `sub` (the user id), `kid` (the key that signed the sign in
//! challenge) and `exp` (ARCHITECTURE 8); `nbf` is optional. This module checks claim
//! values that were already taken out of the verified message. Reading them from the
//! JSON message is a separate step, so the rules here do not depend on a JSON parser.
//!
//! Times are PASETO date times: RFC 3339 with a mandatory offset, for example
//! `2026-10-02T20:45:16Z` or `2022-01-01T00:00:00+00:00`. They are compared with
//! nanosecond precision against a caller supplied `now`, so every check is
//! deterministic in tests.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Number of lowercase hex digits of a user id (a row id, ARCHITECTURE 5.3).
pub const SUB_HEX_LEN: usize = 32;

/// Upper bound for the length of a `kid` in bytes.
pub const MAX_KID_LEN: usize = 128;

/// Upper bound for the length of a date time claim in bytes, checked before parsing.
const MAX_TIME_LEN: usize = 64;

const NANOS_PER_SEC: i128 = 1_000_000_000;

/// A point in time as signed nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixTime(i128);

impl UnixTime {
    /// The time `secs` seconds after the Unix epoch (negative: before it).
    pub fn from_secs(secs: i64) -> Self {
        UnixTime(i128::from(secs) * NANOS_PER_SEC)
    }

    /// The time `nanos` nanoseconds after the Unix epoch (negative: before it).
    pub fn from_nanos(nanos: i128) -> Self {
        UnixTime(nanos)
    }

    /// Nanoseconds since the Unix epoch.
    pub fn as_nanos(self) -> i128 {
        self.0
    }

    /// The current system time.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Converts a system time, including times before the Unix epoch.
    pub fn from_system_time(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => UnixTime(duration_nanos(after)),
            Err(before) => UnixTime(-duration_nanos(before.duration())),
        }
    }

    fn saturating_add(self, d: Duration) -> Self {
        UnixTime(self.0.saturating_add(duration_nanos(d)))
    }

    fn saturating_sub(self, d: Duration) -> Self {
        UnixTime(self.0.saturating_sub(duration_nanos(d)))
    }
}

fn duration_nanos(d: Duration) -> i128 {
    // A Duration holds at most about 1.8e28 ns, far below i128::MAX.
    i128::try_from(d.as_nanos()).unwrap_or(i128::MAX)
}

/// One of the claims this module checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// `sub`: the user id.
    Sub,
    /// `kid`: the id of the key that signed the sign in challenge.
    Kid,
    /// `exp`: the time from which the token is no longer valid.
    Exp,
    /// `nbf`: the time before which the token is not yet valid.
    Nbf,
}

impl Claim {
    /// The claim name as it appears in the token.
    pub fn name(self) -> &'static str {
        match self {
            Claim::Sub => "sub",
            Claim::Kid => "kid",
            Claim::Exp => "exp",
            Claim::Nbf => "nbf",
        }
    }
}

/// Why the claims of a token were rejected.
///
/// As with [`crate::Error`], the variants are for logs and tests; a server tells the
/// client only that there is no principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimsError {
    /// A required claim is absent.
    Missing(Claim),
    /// A claim is present but its value has the wrong form.
    Invalid(Claim),
    /// `now` is at or after `exp` (plus the leeway).
    Expired,
    /// `now` is before `nbf` (minus the leeway).
    NotYetValid,
    /// `exp` lies further in the future than the policy allows.
    LifetimeTooLong,
}

impl fmt::Display for ClaimsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimsError::Missing(claim) => write!(f, "claim `{}` is missing", claim.name()),
            ClaimsError::Invalid(claim) => write!(f, "claim `{}` is invalid", claim.name()),
            ClaimsError::Expired => f.write_str("token has expired"),
            ClaimsError::NotYetValid => f.write_str("token is not valid yet"),
            ClaimsError::LifetimeTooLong => f.write_str("token expires too far in the future"),
        }
    }
}

impl std::error::Error for ClaimsError {}

/// Claim values as read from a verified message, before any check.
///
/// `None` means the claim is absent. Each value is the decoded JSON string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimInput<'a> {
    /// Value of `sub`.
    pub sub: Option<&'a str>,
    /// Value of `kid`.
    pub kid: Option<&'a str>,
    /// Value of `exp`.
    pub exp: Option<&'a str>,
    /// Value of `nbf`.
    pub nbf: Option<&'a str>,
}

/// How strictly the times are checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Allowed clock difference, applied in favour of the token to `exp` and `nbf`.
    pub leeway: Duration,
    /// Longest accepted distance from `now` to `exp`; `None` accepts any distance.
    pub max_ttl: Option<Duration>,
}

impl Default for Policy {
    /// No leeway (the server checks tokens it signed itself) and the 24 hour session
    /// lifetime of ARCHITECTURE 8.
    fn default() -> Self {
        Policy {
            leeway: Duration::ZERO,
            max_ttl: Some(Duration::from_secs(24 * 60 * 60)),
        }
    }
}

/// Claims that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    /// The user id: 32 lowercase hex digits.
    pub sub: String,
    /// The key id. The caller still looks it up and checks that the key is not revoked.
    pub kid: String,
    /// The token is valid strictly before this time.
    pub exp: UnixTime,
    /// The token is valid from this time on, when present.
    pub nbf: Option<UnixTime>,
}

/// Checks the claims of a verified token at time `now`.
///
/// `sub`, `kid` and `exp` are required, `nbf` is optional. The token is valid when
/// `nbf - leeway <= now < exp + leeway`. A token whose `nbf` is not before its `exp`
/// is never valid and is rejected as an invalid `nbf`, whatever the leeway.
///
/// The checks run in a fixed order (form of every claim first, then the times), so the
/// result does not depend on which claims a caller looks at first.
pub fn check_claims(
    input: &ClaimInput<'_>,
    policy: &Policy,
    now: UnixTime,
) -> Result<Claims, ClaimsError> {
    let sub = input.sub.ok_or(ClaimsError::Missing(Claim::Sub))?;
    if !is_user_id(sub) {
        return Err(ClaimsError::Invalid(Claim::Sub));
    }
    let kid = input.kid.ok_or(ClaimsError::Missing(Claim::Kid))?;
    if !is_key_id(kid) {
        return Err(ClaimsError::Invalid(Claim::Kid));
    }
    let exp_text = input.exp.ok_or(ClaimsError::Missing(Claim::Exp))?;
    let exp = parse_date_time(exp_text).map_err(|_| ClaimsError::Invalid(Claim::Exp))?;
    let nbf = match input.nbf {
        Some(text) => Some(parse_date_time(text).map_err(|_| ClaimsError::Invalid(Claim::Nbf))?),
        None => None,
    };
    if nbf.is_some_and(|nbf| nbf >= exp) {
        return Err(ClaimsError::Invalid(Claim::Nbf));
    }

    if now >= exp.saturating_add(policy.leeway) {
        return Err(ClaimsError::Expired);
    }
    if nbf.is_some_and(|nbf| now < nbf.saturating_sub(policy.leeway)) {
        return Err(ClaimsError::NotYetValid);
    }
    if let Some(max_ttl) = policy.max_ttl
        && exp > now.saturating_add(max_ttl).saturating_add(policy.leeway)
    {
        return Err(ClaimsError::LifetimeTooLong);
    }

    Ok(Claims {
        sub: sub.to_owned(),
        kid: kid.to_owned(),
        exp,
        nbf,
    })
}

/// A user id is a row id: exactly [`SUB_HEX_LEN`] lowercase hex digits.
fn is_user_id(text: &str) -> bool {
    text.len() == SUB_HEX_LEN && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A key id is 1 to [`MAX_KID_LEN`] bytes of printable ASCII without spaces, so it can
/// be logged and compared byte by byte without normalisation.
fn is_key_id(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_KID_LEN && text.bytes().all(|b| b.is_ascii_graphic())
}

/// Why a date time was rejected. Kept private: callers see [`ClaimsError::Invalid`].
#[derive(Debug, PartialEq, Eq)]
struct BadDateTime;

/// Parses an RFC 3339 date time with a mandatory offset.
///
/// Accepted: `YYYY-MM-DDTHH:MM:SS`, an optional fraction of 1 to 9 digits, then `Z` or
/// `+HH:MM` / `-HH:MM`. The letters must be upper case. Rejected: dates that do not
/// exist (such as February 29 of a common year), hour 24, leap second 60, offsets beyond
/// 23:59, and anything longer than 64 bytes.
fn parse_date_time(text: &str) -> Result<UnixTime, BadDateTime> {
    if text.len() > MAX_TIME_LEN {
        return Err(BadDateTime);
    }
    let mut cursor = Cursor {
        bytes: text.as_bytes(),
        pos: 0,
    };

    let year = cursor.digits(4)?;
    cursor.expect(b'-')?;
    let month = cursor.digits(2)?;
    cursor.expect(b'-')?;
    let day = cursor.digits(2)?;
    cursor.expect(b'T')?;
    let hour = cursor.digits(2)?;
    cursor.expect(b':')?;
    let minute = cursor.digits(2)?;
    cursor.expect(b':')?;
    let second = cursor.digits(2)?;

    let mut nanos: i128 = 0;
    if cursor.peek() == Some(b'.') {
        cursor.pos += 1;
        let mut count = 0u32;
        while let Some(b) = cursor.peek().filter(u8::is_ascii_digit) {
            if count == 9 {
                return Err(BadDateTime);
            }
            nanos = nanos * 10 + i128::from(b - b'0');
            count += 1;
            cursor.pos += 1;
        }
        if count == 0 {
            return Err(BadDateTime);
        }
        nanos *= 10i128.pow(9 - count);
    }

    let offset_minutes: i64 = match cursor.next() {
        Some(b'Z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = cursor.digits(2)?;
            cursor.expect(b':')?;
            let om = cursor.digits(2)?;
            if oh > 23 || om > 59 {
                return Err(BadDateTime);
            }
            let minutes = oh * 60 + om;
            if sign == b'+' { minutes } else { -minutes }
        }
        _ => return Err(BadDateTime),
    };
    if cursor.pos != cursor.bytes.len() {
        return Err(BadDateTime);
    }

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(BadDateTime);
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(BadDateTime);
    }

    let days = days_from_civil(year, month, day);
    let local = days * 86_400 + hour * 3_600 + minute * 60 + second;
    let utc = local - offset_minutes * 60;
    Ok(UnixTime(i128::from(utc) * NANOS_PER_SEC + nanos))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn expect(&mut self, want: u8) -> Result<(), BadDateTime> {
        if self.next() == Some(want) {
            Ok(())
        } else {
            Err(BadDateTime)
        }
    }

    /// Reads exactly `count` ASCII digits.
    fn digits(&mut self, count: usize) -> Result<i64, BadDateTime> {
        let mut value = 0i64;
        for _ in 0..count {
            match self.next() {
                Some(b @ b'0'..=b'9') => value = value * 10 + i64::from(b - b'0'),
                _ => return Err(BadDateTime),
            }
        }
        Ok(value)
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date (negative before it).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests;

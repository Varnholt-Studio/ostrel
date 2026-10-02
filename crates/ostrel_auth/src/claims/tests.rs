use super::*;

const SUB: &str = "0123456789abcdef0123456789abcdef";
const KID: &str = "zVhMiPBP9fRf2snEcT7gFTioeA9COcNy9DfgL1W60haN";

/// 2026-10-02T12:00:00Z
const NOON: i64 = 1_790_942_400;

fn at(secs: i64) -> UnixTime {
    UnixTime::from_secs(secs)
}

fn parse(text: &str) -> Option<UnixTime> {
    parse_date_time(text).ok()
}

fn input<'a>(exp: &'a str) -> ClaimInput<'a> {
    ClaimInput {
        sub: Some(SUB),
        kid: Some(KID),
        exp: Some(exp),
        nbf: None,
    }
}

fn no_ttl() -> Policy {
    Policy {
        leeway: Duration::ZERO,
        max_ttl: None,
    }
}

// Date time parsing.

#[test]
fn parses_epoch_and_known_instants() {
    assert_eq!(parse("1970-01-01T00:00:00Z"), Some(at(0)));
    assert_eq!(parse("2026-10-02T12:00:00Z"), Some(at(NOON)));
    // The exp value of the official PASETO vectors.
    assert_eq!(parse("2022-01-01T00:00:00+00:00"), Some(at(1_640_995_200)));
    assert_eq!(parse("2000-02-29T00:00:00Z"), Some(at(951_782_400)));
    assert_eq!(parse("1969-12-31T23:59:59Z"), Some(at(-1)));
    assert_eq!(parse("0000-01-01T00:00:00Z"), Some(at(-62_167_219_200)));
    assert_eq!(parse("9999-12-31T23:59:59Z"), Some(at(253_402_300_799)));
}

#[test]
fn applies_offsets() {
    assert_eq!(parse("2026-10-02T14:00:00+02:00"), Some(at(NOON)));
    assert_eq!(parse("2026-10-02T07:30:00-04:30"), Some(at(NOON)));
    assert_eq!(parse("2026-10-02T12:00:00-00:00"), Some(at(NOON)));
    // An offset can move the instant across a day, month and year boundary.
    assert_eq!(
        parse("2027-01-01T00:59:00+23:59"),
        Some(at(1_798_761_600 - 23 * 3600))
    );
    assert_eq!(
        parse("2026-12-31T23:59:00-23:59"),
        parse("2027-01-01T23:58:00Z")
    );
}

#[test]
fn keeps_fractions_to_the_nanosecond() {
    let base = at(NOON).as_nanos();
    assert_eq!(
        parse("2026-10-02T12:00:00.5Z").unwrap().as_nanos(),
        base + 500_000_000
    );
    assert_eq!(
        parse("2026-10-02T12:00:00.000000001Z").unwrap().as_nanos(),
        base + 1
    );
    assert_eq!(
        parse("2026-10-02T12:00:00.999999999Z").unwrap().as_nanos(),
        base + 999_999_999
    );
    assert_eq!(parse("2026-10-02T12:00:00.000Z"), Some(at(NOON)));
}

#[test]
fn rejects_dates_that_do_not_exist() {
    for text in [
        "2026-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-00-10T00:00:00Z",
        "2026-13-10T00:00:00Z",
        "2026-10-00T00:00:00Z",
        "2026-10-32T00:00:00Z",
        "2026-10-02T24:00:00Z",
        "2026-10-02T12:60:00Z",
        "2026-12-31T23:59:60Z",
        "2026-10-02T12:00:00+24:00",
        "2026-10-02T12:00:00+00:60",
    ] {
        assert_eq!(parse(text), None, "{text}");
    }
}

#[test]
fn rejects_malformed_date_times() {
    for text in [
        "",
        "Z",
        "2026-10-02",
        "2026-10-02T12:00:00",
        "2026-10-02 12:00:00Z",
        "2026-10-02t12:00:00Z",
        "2026-10-02T12:00:00z",
        "2026-10-02T12:00Z",
        "2026-10-02T12:00:00.Z",
        "2026-10-02T12:00:00.1234567890Z",
        "2026-10-02T12:00:00,5Z",
        "2026-10-02T12:00:00+0200",
        "2026-10-02T12:00:00+02",
        "2026-10-02T12:00:00Z ",
        " 2026-10-02T12:00:00Z",
        "2026-10-02T12:00:00ZZ",
        "+2026-10-02T12:00:00Z",
        "12026-10-02T12:00:00Z",
        "2026-1-02T12:00:00Z",
        "2026-10-02T12:00:00\u{0}Z",
        "\u{663}026-10-02T12:00:00Z",
        "2026-10-02T12:00:00.\u{661}Z",
    ] {
        assert_eq!(parse(text), None, "{text:?}");
    }
    let long = format!("2026-10-02T12:00:00.{}Z", "0".repeat(60));
    assert_eq!(parse(&long), None);
}

#[test]
fn every_day_from_1970_to_2100_round_trips() {
    // Walk day by day and compare the parser against a running counter.
    let mut expected = 0i64;
    for year in 1970..2100 {
        for month in 1..=12 {
            for day in 1..=days_in_month(year, month) {
                let text = format!("{year:04}-{month:02}-{day:02}T00:00:00Z");
                assert_eq!(parse(&text), Some(at(expected * 86_400)), "{text}");
                expected += 1;
            }
        }
    }
}

// Claim checks.

#[test]
fn accepts_valid_claims() {
    let claims =
        check_claims(&input("2026-10-02T13:00:00Z"), &Policy::default(), at(NOON)).unwrap();
    assert_eq!(
        claims,
        Claims {
            sub: SUB.to_owned(),
            kid: KID.to_owned(),
            exp: at(NOON + 3600),
            nbf: None,
        }
    );
}

#[test]
fn reports_missing_claims_in_fixed_order() {
    let none = ClaimInput::default();
    assert_eq!(
        check_claims(&none, &no_ttl(), at(NOON)),
        Err(ClaimsError::Missing(Claim::Sub))
    );
    let no_kid = ClaimInput {
        kid: None,
        ..input("2026-10-02T13:00:00Z")
    };
    assert_eq!(
        check_claims(&no_kid, &no_ttl(), at(NOON)),
        Err(ClaimsError::Missing(Claim::Kid))
    );
    let no_exp = ClaimInput {
        exp: None,
        ..input("")
    };
    assert_eq!(
        check_claims(&no_exp, &no_ttl(), at(NOON)),
        Err(ClaimsError::Missing(Claim::Exp))
    );
    // A malformed sub is reported even when the token is also expired.
    let bad_sub = ClaimInput {
        sub: Some("x"),
        ..input("1970-01-01T00:00:00Z")
    };
    assert_eq!(
        check_claims(&bad_sub, &no_ttl(), at(NOON)),
        Err(ClaimsError::Invalid(Claim::Sub))
    );
}

#[test]
fn sub_must_be_a_row_id() {
    let exp = "2026-10-02T13:00:00Z";
    for sub in [
        "",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcdeg",
        " 123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcd\u{e9}",
    ] {
        let claims = ClaimInput {
            sub: Some(sub),
            ..input(exp)
        };
        assert_eq!(
            check_claims(&claims, &no_ttl(), at(NOON)),
            Err(ClaimsError::Invalid(Claim::Sub)),
            "{sub:?}"
        );
    }
}

#[test]
fn kid_must_be_printable_ascii_within_bounds() {
    let exp = "2026-10-02T13:00:00Z";
    let at_limit = "k".repeat(MAX_KID_LEN);
    let ok = ClaimInput {
        kid: Some(&at_limit),
        ..input(exp)
    };
    assert!(check_claims(&ok, &no_ttl(), at(NOON)).is_ok());
    let one_char = ClaimInput {
        kid: Some("k"),
        ..input(exp)
    };
    assert!(check_claims(&one_char, &no_ttl(), at(NOON)).is_ok());

    let too_long = "k".repeat(MAX_KID_LEN + 1);
    for kid in [
        "",
        "a b",
        "a\tb",
        "a\u{7f}",
        "\u{e9}",
        "a\u{0}",
        too_long.as_str(),
    ] {
        let claims = ClaimInput {
            kid: Some(kid),
            ..input(exp)
        };
        assert_eq!(
            check_claims(&claims, &no_ttl(), at(NOON)),
            Err(ClaimsError::Invalid(Claim::Kid)),
            "{kid:?}"
        );
    }
}

#[test]
fn malformed_times_are_invalid_claims() {
    let claims = input("2026-10-02T13:00:00");
    assert_eq!(
        check_claims(&claims, &no_ttl(), at(NOON)),
        Err(ClaimsError::Invalid(Claim::Exp))
    );
    let claims = ClaimInput {
        nbf: Some("yesterday"),
        ..input("2026-10-02T13:00:00Z")
    };
    assert_eq!(
        check_claims(&claims, &no_ttl(), at(NOON)),
        Err(ClaimsError::Invalid(Claim::Nbf))
    );
}

#[test]
fn exp_is_exclusive_to_the_nanosecond() {
    let claims = input("2026-10-02T12:00:00Z");
    let exp = at(NOON).as_nanos();
    assert!(check_claims(&claims, &no_ttl(), UnixTime::from_nanos(exp - 1)).is_ok());
    assert_eq!(
        check_claims(&claims, &no_ttl(), UnixTime::from_nanos(exp)),
        Err(ClaimsError::Expired)
    );
    assert_eq!(
        check_claims(&claims, &no_ttl(), UnixTime::from_nanos(exp + 1)),
        Err(ClaimsError::Expired)
    );
}

#[test]
fn nbf_is_inclusive_to_the_nanosecond() {
    let claims = ClaimInput {
        nbf: Some("2026-10-02T12:00:00Z"),
        ..input("2026-10-02T13:00:00Z")
    };
    let nbf = at(NOON).as_nanos();
    assert_eq!(
        check_claims(&claims, &no_ttl(), UnixTime::from_nanos(nbf - 1)),
        Err(ClaimsError::NotYetValid)
    );
    let ok = check_claims(&claims, &no_ttl(), UnixTime::from_nanos(nbf)).unwrap();
    assert_eq!(ok.nbf, Some(at(NOON)));
}

#[test]
fn nbf_not_before_exp_is_never_valid() {
    for nbf in ["2026-10-02T13:00:00Z", "2026-10-02T14:00:00Z"] {
        let claims = ClaimInput {
            nbf: Some(nbf),
            ..input("2026-10-02T13:00:00Z")
        };
        let generous = Policy {
            leeway: Duration::from_secs(86_400),
            max_ttl: None,
        };
        assert_eq!(
            check_claims(&claims, &generous, at(NOON + 3600)),
            Err(ClaimsError::Invalid(Claim::Nbf)),
            "{nbf}"
        );
    }
}

#[test]
fn leeway_extends_both_bounds_exactly() {
    let claims = ClaimInput {
        nbf: Some("2026-10-02T12:00:00Z"),
        ..input("2026-10-02T13:00:00Z")
    };
    let policy = Policy {
        leeway: Duration::from_secs(30),
        max_ttl: None,
    };
    assert!(check_claims(&claims, &policy, at(NOON - 30)).is_ok());
    assert_eq!(
        check_claims(&claims, &policy, at(NOON - 31)),
        Err(ClaimsError::NotYetValid)
    );
    assert!(check_claims(&claims, &policy, at(NOON + 3600 + 29)).is_ok());
    assert_eq!(
        check_claims(&claims, &policy, at(NOON + 3600 + 30)),
        Err(ClaimsError::Expired)
    );
}

#[test]
fn default_policy_caps_the_lifetime_at_24_hours() {
    let policy = Policy::default();
    assert_eq!(policy.leeway, Duration::ZERO);
    let day = input("2026-10-03T12:00:00Z");
    assert!(check_claims(&day, &policy, at(NOON)).is_ok());
    let day_and_a_nanosecond = input("2026-10-03T12:00:00.000000001Z");
    assert_eq!(
        check_claims(&day_and_a_nanosecond, &policy, at(NOON)),
        Err(ClaimsError::LifetimeTooLong)
    );
    let far = input("9999-12-31T23:59:59Z");
    assert_eq!(
        check_claims(&far, &policy, at(NOON)),
        Err(ClaimsError::LifetimeTooLong)
    );
    assert!(check_claims(&far, &no_ttl(), at(NOON)).is_ok());
}

#[test]
fn extreme_policies_and_times_do_not_overflow() {
    let huge = Policy {
        leeway: Duration::MAX,
        max_ttl: Some(Duration::MAX),
    };
    let claims = ClaimInput {
        nbf: Some("0000-01-01T00:00:00Z"),
        ..input("9999-12-31T23:59:59Z")
    };
    assert!(check_claims(&claims, &huge, at(i64::MIN)).is_ok());
    assert!(check_claims(&claims, &huge, at(i64::MAX)).is_ok());
    assert_eq!(
        check_claims(&claims, &no_ttl(), UnixTime::from_nanos(i128::MAX)),
        Err(ClaimsError::Expired)
    );
}

#[test]
fn system_time_conversion_handles_both_sides_of_the_epoch() {
    assert_eq!(UnixTime::from_system_time(UNIX_EPOCH), at(0));
    let after = UNIX_EPOCH + Duration::new(5, 7);
    assert_eq!(UnixTime::from_system_time(after).as_nanos(), 5_000_000_007);
    let before = UNIX_EPOCH - Duration::new(5, 7);
    assert_eq!(
        UnixTime::from_system_time(before).as_nanos(),
        -5_000_000_007
    );
    assert!(UnixTime::now() > at(NOON - 86_400 * 365));
}

#[test]
fn errors_name_the_claim() {
    assert_eq!(
        ClaimsError::Missing(Claim::Kid).to_string(),
        "claim `kid` is missing"
    );
    assert_eq!(
        ClaimsError::Invalid(Claim::Nbf).to_string(),
        "claim `nbf` is invalid"
    );
    assert_eq!(ClaimsError::Expired.to_string(), "token has expired");
}

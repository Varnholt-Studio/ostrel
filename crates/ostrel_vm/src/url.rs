//! The URL rule of `Url.parse` (ARCHITECTURE 7.5, SPEC 12.13, D144).
//!
//! The client runtime applies the same rule in `parseUrl`
//! (`runtime/js/view/safe/url.js`), and both sides are tested against one corpus,
//! `tests/hostile/url/corpus.txt`. The contract is the same result on both sides, not
//! equality with the WHATWG URL Standard.
//!
//! A text is accepted when it holds no control character, is not empty after removing
//! leading and trailing spaces, and is either relative or absolute with the scheme
//! `http`, `https` or `mailto`. An `http` or `https` text must also pass the host rule H:
//! host names only in ASCII (international names in their `xn--` form), no IPv6
//! literals, no user info, no empty port. Security rests on the control character rule
//! and the scheme allowlist; H only decides whether a link is usable.
//!
//! The function is total: it never panics and runs in time linear in the input.

/// Applies the URL rule to `text`.
///
/// Returns the accepted URL text, which is `text` without leading and trailing U+0020
/// and otherwise unchanged, or `None` when the rule refuses it.
pub fn parse_url(text: &str) -> Option<String> {
    if text.chars().any(is_control) {
        return None;
    }
    let url = text.trim_matches(' ');
    if url.is_empty() {
        return None;
    }
    let Some((scheme, after_scheme)) = split_scheme(url) else {
        // Relative: path, query, fragment or "//host/path".
        return Some(url.to_owned());
    };
    let accepted = if scheme.eq_ignore_ascii_case("mailto") {
        true
    } else if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        passes_host_rule(after_scheme)
    } else {
        false
    };
    accepted.then(|| url.to_owned())
}

/// U+0000 to U+001F and U+007F to U+009F (C0 and C1 controls, DEL).
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// Splits off the scheme: the longest run `[A-Za-z][A-Za-z0-9+.-]*` at the start, directly
/// followed by `:`. Returns the scheme and the text after the `:`, or `None` when the
/// text has no scheme and so is relative.
fn split_scheme(url: &str) -> Option<(&str, &str)> {
    if !url.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let end = url
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
        .unwrap_or(url.len());
    let (scheme, rest) = url.split_at_checked(end)?;
    let after_scheme = rest.strip_prefix(':')?;
    Some((scheme, after_scheme))
}

/// Host rule H for the text after `http:` or `https:`.
fn passes_host_rule(after_scheme: &str) -> bool {
    // H1: the scheme is followed by "//".
    let Some(rest) = after_scheme.strip_prefix("//") else {
        return false;
    };
    // H2: the authority runs up to the first "/", "?", "#" or the end.
    // H7: what follows (path, query, fragment) is not checked.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // H3: the authority is a host, optionally followed by ":" and a port. Any other
    // character ("@", "[", "]", "\", "%", space, non ASCII) fails the host or port check.
    match authority.split_once(':') {
        Some((host, port)) => is_port(port) && is_host(host),
        None => is_host(authority),
    }
}

/// H4 and H5: one or more labels of `[A-Za-z0-9-]` separated by single dots, at most one
/// trailing dot; a host whose last label looks numeric must be a plain IPv4 address.
fn is_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    // An empty host gives one empty label and is refused here.
    if !host.split('.').all(is_label) {
        return false;
    }
    let last = host.rsplit('.').next().unwrap_or_default();
    if looks_numeric(last) {
        return is_ipv4(host);
    }
    true
}

fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn looks_numeric(label: &str) -> bool {
    label.bytes().all(|b| b.is_ascii_digit()) || label.starts_with("0x") || label.starts_with("0X")
}

/// Exactly four decimal parts from 0 to 255, without leading zeros.
fn is_ipv4(host: &str) -> bool {
    host.split('.').count() == 4 && host.split('.').all(is_ipv4_part)
}

fn is_ipv4_part(part: &str) -> bool {
    let digits_only = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let leading_zero = part.len() > 1 && part.starts_with('0');
    digits_only && !leading_zero && part.len() <= 3 && part.parse::<u16>().is_ok_and(|v| v <= 255)
}

/// H6: 1 to 5 digits with a value of at most 65535.
fn is_port(port: &str) -> bool {
    (1..=5).contains(&port.len())
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|v| v <= 65_535)
}

#[cfg(test)]
mod tests {
    use super::parse_url;

    #[test]
    fn accepted_text_is_trimmed_of_spaces_only() {
        assert_eq!(
            parse_url("  https://a.b/x  ").as_deref(),
            Some("https://a.b/x")
        );
        assert_eq!(parse_url("\u{A0}/x"), Some("\u{A0}/x".to_owned()));
    }

    #[test]
    fn a_colon_after_a_slash_belongs_to_a_relative_url() {
        assert_eq!(parse_url("a/b:c").as_deref(), Some("a/b:c"));
        assert_eq!(parse_url("1a:b").as_deref(), Some("1a:b"));
    }

    #[test]
    fn a_scheme_with_a_dot_or_plus_is_still_a_scheme() {
        assert_eq!(parse_url("a.b:c"), None);
        assert_eq!(parse_url("http+x://a.b"), None);
    }

    #[test]
    fn non_ascii_in_the_host_is_refused_without_panic() {
        assert_eq!(parse_url("http://\u{1F600}"), None);
        assert_eq!(parse_url("http://a.b:\u{664}"), None);
        assert_eq!(parse_url("http://\u{FC}:80/x"), None);
    }

    #[test]
    fn non_ascii_after_the_authority_is_not_checked() {
        assert_eq!(
            parse_url("https://a.b/\u{FC}").as_deref(),
            Some("https://a.b/\u{FC}")
        );
    }
}

//! The network half of the read contract: what a `fetch` may name.
//!
//! `fetch` is the one read-only kind with no path, and treating it like
//! a filesystem read made the scope check vacuous: a URL is not a
//! filesystem path, so it joined onto the root and looked contained, and
//! a call that named no location at all passed with nothing to inspect.
//! The rule is explicit instead — a fetch proceeds only when it names at
//! least one string and every string it names is an absolute,
//! credential-free `http`/`https` URL. A `file:` URL, a bare host, a
//! call with no location, an `Authorization` header riding along, and
//! an embedded `user:password@` are all denied.

/// Longest accepted URL. A URL past this is not a document reference; it
/// is a payload, and the permission decision must stay bounded.
const MAX_URL_CHARS: usize = 2_048;

/// Whether a `fetch` call's named strings are all acceptable URLs, and
/// whether it named any at all.
pub(crate) fn permits(candidates: &[String]) -> bool {
    !candidates.is_empty() && candidates.iter().all(|candidate| permits_url(candidate))
}

fn permits_url(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.chars().count() > MAX_URL_CHARS {
        return false;
    }
    if candidate.chars().any(char::is_control) {
        return false;
    }
    let Some(rest) = candidate
        .strip_prefix("http://")
        .or_else(|| candidate.strip_prefix("https://"))
    else {
        return false;
    };
    // The authority ends at the first path, query, or fragment
    // delimiter, so a userinfo check cannot be dodged by appending one.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && !authority.contains(char::is_whitespace)
        && !authority.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn absolute_http_urls_are_the_only_accepted_shape() {
        assert!(permits(&urls(&["https://docs.example.com/guide"])));
        assert!(permits(&urls(&["http://example.com"])));
        assert!(permits(&urls(&[
            "https://docs.example.com/a?b=c#d",
            "http://example.com/other"
        ])));
    }

    #[test]
    fn a_fetch_that_names_nothing_is_denied() {
        assert!(!permits(&[]), "a pathless fetch has nothing to bound");
    }

    #[test]
    fn non_http_and_relative_targets_are_denied() {
        for candidate in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "//example.com/x",
            "example.com/x",
            "/etc/shadow",
            "https:/example.com",
            "",
        ] {
            assert!(
                !permits(&urls(&[candidate])),
                "{candidate:?} must not be fetchable"
            );
        }
    }

    #[test]
    fn credentialed_and_unnamed_hosts_are_denied() {
        for candidate in [
            "https://user:s3cret@example.com/x",
            "https://token@example.com/x",
            "https://",
            "https:// example.com",
            "https://example.com\\@evil.test",
        ] {
            assert!(
                !permits(&urls(&[candidate])),
                "{candidate:?} must not be fetchable"
            );
        }
    }

    #[test]
    fn one_bad_string_denies_the_whole_call() {
        assert!(!permits(&urls(&[
            "https://docs.example.com/",
            "Authorization: Bearer sk-value"
        ])));
        assert!(!permits(&urls(&[
            "https://docs.example.com/",
            &"h".repeat(MAX_URL_CHARS + 1)
        ])));
    }
}

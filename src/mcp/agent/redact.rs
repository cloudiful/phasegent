//! Redaction for agent-owned output.
//!
//! The ACP child is a separate process with its own configuration, and
//! its stderr is the one stream this adapter attaches to a failure so a
//! crash stays diagnosable. That stream is not ours: an agent that logs
//! its own request headers, dumps its config, or reports a URL with
//! credentials would otherwise move a secret straight into an error
//! message — and an error message is persisted in the run ledger and
//! returned to a caller. Redaction happens here, at the single point
//! the text is handed out, so no call site can attach raw agent output.
//!
//! The rule is deliberately literal: named credential fields, an
//! authorization scheme and its value, URL userinfo, and the well-known
//! API-key and JWT prefixes. Anything unrecognized stays, because a
//! diagnostics tail that has been over-scrubbed is worse than useless.

/// Replacement for a redacted span. Fixed text, so a reader can tell a
/// redaction from the agent's own output.
const MARKER: &str = "[redacted]";

/// Field-name fragments that make the value beside them a secret.
/// Matched against the lowercased name with `-` folded to `_`.
const CREDENTIAL_KEYS: &[&str] = &[
    "authorization",
    "auth",
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "api_key",
    "apikey",
    "access_key",
    "secret_key",
    "private_key",
    "credential",
    "cookie",
    "signature",
    "bearer",
];

/// Authorization schemes whose following value is a secret.
const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "digest", "apikey"];

/// Value prefixes that identify a secret on their own. The length
/// floor keeps an ordinary word or path fragment from matching.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "sk-ant-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xapp-",
    "akia",
    "asia",
    "aiza",
    "eyj",
    "-----begin",
];

/// Shortest value still treated as a prefixed secret. Long enough that
/// an ordinary word or path fragment does not match, short enough that
/// a `-----BEGIN` PEM header is caught.
const MIN_SECRET_CHARS: usize = 8;

/// Replace secret-like spans in untrusted text. The result keeps the
/// original whitespace and every span that is not matched.
pub fn redact(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut redact_value = false;
    for token in raw.split_inclusive(char::is_whitespace) {
        let body = token.trim_end_matches(char::is_whitespace);
        let trailing = &token[body.len()..];
        if redact_value {
            redact_value = false;
            if !body.is_empty() {
                out.push_str(MARKER);
                out.push_str(trailing);
                continue;
            }
        }
        let redacted = redact_token(body);
        out.push_str(&redacted.text);
        out.push_str(trailing);
        redact_value = redacted.needs_value;
    }
    out
}

/// What one token becomes, and whether the token *after* it is a value
/// belonging to a credential name.
struct Redacted {
    text: String,
    needs_value: bool,
}

fn redact_token(body: &str) -> Redacted {
    if let Some((key, value)) = body.split_once('=')
        && !value.is_empty()
        && is_credential_key(key)
    {
        return Redacted {
            text: format!("{key}={MARKER}"),
            needs_value: false,
        };
    }
    // `Authorization: <value>` and a bare scheme: the value is the next
    // token, not this one.
    if let Some(name) = body.strip_suffix(':')
        && is_credential_key(name)
    {
        return Redacted {
            text: body.to_owned(),
            needs_value: true,
        };
    }
    let lowered = body.to_ascii_lowercase();
    if AUTH_SCHEMES.contains(&lowered.trim_matches(['"', '\''])) {
        return Redacted {
            text: body.to_owned(),
            needs_value: true,
        };
    }
    if is_secret_value(body) {
        return Redacted {
            text: MARKER.to_owned(),
            needs_value: false,
        };
    }
    Redacted {
        text: strip_userinfo(body),
        needs_value: false,
    }
}

fn is_credential_key(key: &str) -> bool {
    let normalized = key.trim_matches(['"', '\'']).to_ascii_lowercase();
    let folded = normalized.replace('-', "_");
    CREDENTIAL_KEYS
        .iter()
        .any(|needle| normalized.contains(needle) || folded.contains(needle))
}

fn is_secret_value(body: &str) -> bool {
    if body.chars().count() < MIN_SECRET_CHARS {
        return false;
    }
    let lowered = body.to_ascii_lowercase();
    SECRET_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

/// `https://user:secret@host/x` keeps its host and loses its userinfo.
fn strip_userinfo(body: &str) -> String {
    let Some(scheme_end) = body.find("://") else {
        return body.to_owned();
    };
    let (scheme, rest) = body.split_at(scheme_end + 3);
    let authority_end = rest.find(['/', '?', '#', ' ', '\t']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) if !userinfo.is_empty() && !host.is_empty() => {
            format!("{scheme}{MARKER}@{host}{tail}")
        }
        _ => body.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_authorization_header_loses_its_value() {
        let redacted = redact("request failed Authorization: Bearer sk-live-ABCDEF0123456789");
        assert!(redacted.contains(MARKER), "{redacted}");
        assert!(!redacted.contains("sk-live-ABCDEF0123456789"), "{redacted}");
        assert!(redacted.contains("Authorization"), "{redacted}");
    }

    #[test]
    fn named_credential_fields_lose_their_values() {
        for line in [
            "api_key=abcd1234efgh5678",
            "MINIMAX_API_KEY: zzzzyyyyxxxx",
            "X-Api-Key: qqqqwwwweeee",
            "password=hunter2hunter2",
            "authorization=Bearer aaaabbbbcccc",
        ] {
            let redacted = redact(line);
            assert!(redacted.contains(MARKER), "{line} -> {redacted}");
        }
        assert!(!redact("api_key=abcd1234efgh5678").contains("abcd1234efgh"));
    }

    #[test]
    fn url_userinfo_is_removed_but_the_host_survives() {
        let redacted = redact("connect https://user:s3cr3tpw@example.com/api failed");
        assert!(!redacted.contains("s3cr3tpw"), "{redacted}");
        assert!(!redacted.contains("user:"), "{redacted}");
        assert!(redacted.contains("example.com/api"), "{redacted}");
    }

    #[test]
    fn prefixed_and_jwt_values_are_redacted_on_their_own() {
        for secret in [
            "sk-minimap-0123456789abcdef",
            "ghp_0123456789abcdefghij",
            "github_pat_11ABCDEFG0abcdefgh",
            "AKIA0123456789ABCDEF",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "-----BEGIN RSA PRIVATE KEY-----",
        ] {
            let redacted = redact(&format!("token in log: {secret}"));
            assert!(
                !redacted.contains(secret),
                "{secret} survived as {redacted}"
            );
        }
    }

    #[test]
    fn ordinary_diagnostics_survive_untouched() {
        for line in [
            "connect ECONNREFUSED 127.0.0.1:8080",
            "session/load returned a different session id",
            "reading /home/dev/wt/src/main.rs",
            "took 1.5s for 12 modules in src/",
        ] {
            assert_eq!(redact(line), line, "{line} must stay diagnosable");
        }
    }

    #[test]
    fn whitespace_and_short_values_are_preserved() {
        assert_eq!(redact("  a\n\tb  "), "  a\n\tb  ");
        assert_eq!(redact("sk-"), "sk-", "a short token is not a secret");
        assert_eq!(redact(""), "");
    }
}

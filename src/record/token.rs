//! Token validation for record metadata.
//!
//! Every free-form field a record carries (the request key, the phase,
//! the recon label) is an ASCII token from `[A-Za-z0-9._:-]` with a
//! length of 1..=128. The token set is deliberately narrow so a value
//! can never terminate the generated HTML envelope, inject a second
//! `-->` sequence, or contain a control character, which is what lets
//! the header be serialized and parsed without any escaping.

/// Longest accepted token. Bounded so one record cannot carry an
/// unbounded identifier in its generated header.
pub(crate) const MAX_TOKEN_LEN: usize = 128;

/// Whether `value` is a usable record token.
pub(crate) fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// Validate `value` as a token, naming `label` in the error message.
pub(crate) fn validate(label: &str, value: &str) -> Result<(), String> {
    if is_token(value) {
        Ok(())
    } else {
        Err(format!(
            "{label} '{value}' is invalid; expected 1-{MAX_TOKEN_LEN} ASCII characters from [A-Za-z0-9._:-]"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_documented_token_set() {
        for value in ["a", "issue754-P1-a1", "P1", "a.b_c:d-e", "0"] {
            assert!(is_token(value), "{value} must be accepted");
        }
    }

    #[test]
    fn rejects_anything_that_could_break_the_envelope() {
        for value in ["", "a b", "-->", "<!--", "a\nb", "\"x\"", "{", "é"] {
            assert!(!is_token(value), "{value:?} must be rejected");
        }
    }

    #[test]
    fn rejects_tokens_beyond_the_length_cap() {
        assert!(is_token(&"a".repeat(MAX_TOKEN_LEN)));
        assert!(!is_token(&"a".repeat(MAX_TOKEN_LEN + 1)));
    }
}

use crate::command::{self, Command};
use crate::notifications::config::{EmailTlsSelection, resolve_email_tls};

const PASSWORD: &str = "PHASEGENT_NOTIFY_EMAIL_PASSWORD";
const USERNAME: &str = "PHASEGENT_NOTIFY_EMAIL_USERNAME";

fn parse_config_set(args: &[&str], role: Option<&str>) -> Result<Command, String> {
    let args = args
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    command::parse_with_role_env(&args, role).map(|invocation| invocation.command)
}

#[test]
fn email_password_accepts_direct_value_and_username_is_not_secret() {
    match parse_config_set(
        &[
            "admin",
            "config",
            "set",
            "notify-email-password",
            "cf-token",
        ],
        Some("admin"),
    )
    .expect("direct email password must parse")
    {
        Command::ConfigSet { setting, value, .. } => {
            assert_eq!(setting, PASSWORD);
            assert_eq!(value.as_deref(), Some("cf-token"));
        }
        other => panic!("expected ConfigSet, got {other:?}"),
    }
    match parse_config_set(
        &[
            "admin",
            "config",
            "set",
            "notify-email-username",
            "api_token",
        ],
        Some("admin"),
    )
    .expect("api_token username must parse")
    {
        Command::ConfigSet { setting, value, .. } => {
            assert_eq!(setting, USERNAME);
            assert_eq!(value.as_deref(), Some("api_token"));
        }
        other => panic!("expected ConfigSet, got {other:?}"),
    }
}

#[test]
fn email_password_keeps_stdin_path_and_mutual_exclusion() {
    match parse_config_set(
        &["admin", "config", "set", "notify-email-password", "--stdin"],
        Some("admin"),
    )
    .expect("email password stdin must parse")
    {
        Command::ConfigSet { stdin, value, .. } => {
            assert!(stdin);
            assert!(value.is_none());
        }
        other => panic!("expected ConfigSet, got {other:?}"),
    }
    let error = parse_config_set(
        &[
            "admin",
            "config",
            "set",
            "notify-email-password",
            "cf-token",
            "--stdin",
        ],
        Some("admin"),
    )
    .expect_err("value plus --stdin must be rejected");
    assert!(
        error.contains("cannot provide both a value and --stdin"),
        "got: {error}"
    );
    assert!(!error.contains("cf-token"), "error must not echo: {error}");
}

#[test]
fn other_secrets_still_reject_direct_values() {
    for (alias, secret) in [
        ("notify-ntfy-token", "ntfy-direct-shhh"),
        ("notify-webhook-token", "webhook-direct-shhh"),
        ("notify-dingtalk-secret", "dingtalk-direct-shhh"),
        ("redmine-git-mirror-api-key", "mirror-direct-shhh"),
    ] {
        let error = parse_config_set(&["admin", "config", "set", alias, secret], Some("admin"))
            .expect_err("non-email secret direct value must be rejected");
        assert!(
            error.contains("does not accept a direct value"),
            "{alias}: {error}"
        );
        assert!(!error.contains(secret), "{alias} leaked: {error}");
    }
}

#[test]
fn dash_prefixed_secret_candidates_are_never_echoed() {
    for token in ["-cf-api-token-shhh", "--cf-api-token-shhh", "-", "-x"] {
        let error = parse_config_set(
            &["admin", "config", "set", "notify-email-password", token],
            Some("admin"),
        )
        .expect_err("dash-prefixed candidate must be rejected");
        assert!(
            error.contains("does not accept option-like values"),
            "{token}: {error}"
        );
        assert!(
            error.contains("--stdin") && error.contains("interactive prompt"),
            "{token}: {error}"
        );
        assert!(
            !error.contains("cf-api-token-shhh"),
            "{token} leaked candidate: {error}"
        );
        assert!(
            !error.contains("unknown option"),
            "{token} must not be reported as a flag: {error}"
        );
    }
    // Every other secret gets the same generic rejection, not an echoed flag.
    let error = parse_config_set(
        &["admin", "config", "set", "notify-ntfy-token", "-ntfy-shhh"],
        Some("admin"),
    )
    .expect_err("dash-prefixed non-email secret candidate must be rejected");
    assert!(
        error.contains("does not accept option-like values"),
        "got: {error}"
    );
    assert!(!error.contains("ntfy-shhh"), "got: {error}");
}

#[test]
fn malformed_secret_arguments_never_expose_the_candidate() {
    for args in [
        vec![
            "admin",
            "config",
            "set",
            "notify-email-password",
            "--stdin",
            "cf-api-token-shhh",
        ],
        vec![
            "admin",
            "config",
            "set",
            "notify-email-password",
            "cf-api-token-shhh",
            "extra",
        ],
        vec![
            "admin",
            "config",
            "set",
            "notify-email-password",
            "--stdin",
            "--stdin",
        ],
        vec![
            "admin",
            "config",
            "set",
            "notify-webhook-token",
            "webhook-shhh",
        ],
    ] {
        let error = parse_config_set(&args, Some("admin"))
            .expect_err("malformed secret invocation must error");
        for leaked in ["cf-api-token-shhh", "webhook-shhh"] {
            assert!(!error.contains(leaked), "error leaked '{leaked}': {error}");
        }
    }
}

#[test]
fn email_tls_resolution_matrix() {
    use EmailTlsSelection::{Implicit, Plain, StartTls};

    let cases = [
        (None, None, StartTls),
        (None, Some(465), Implicit),
        (None, Some(587), StartTls),
        (None, Some(25), StartTls),
        (Some("auto"), None, StartTls),
        (Some("auto"), Some(465), Implicit),
        (Some("auto"), Some(587), StartTls),
        (Some(" AUTO "), Some(465), Implicit),
        (Some("implicit"), Some(587), Implicit),
        (Some("starttls"), Some(465), StartTls),
        (Some("plain"), Some(465), Plain),
    ];
    for (mode, port, expected) in cases {
        assert_eq!(
            resolve_email_tls(mode, port),
            expected,
            "mode {mode:?} port {port:?}"
        );
    }
}

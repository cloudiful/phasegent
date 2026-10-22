use super::support::*;
use super::*;

use crate::notifications::config::{EmailTlsSelection, resolve_email_tls};

const PASSWORD: &str = "PHASEGENT_NOTIFY_EMAIL_PASSWORD";
const USERNAME: &str = "PHASEGENT_NOTIFY_EMAIL_USERNAME";
const TLS: &str = "PHASEGENT_NOTIFY_EMAIL_TLS";

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
fn direct_email_password_persists_without_echo() {
    with_isolated_storage("notify-email-direct-password", |_db_path, storage| {
        let outcome =
            config_write::set_setting_value(None, PASSWORD, PASSWORD_TOKEN, storage).unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(!text.contains(PASSWORD_TOKEN), "outcome leaked: {text}");
        assert!(text.contains(PASSWORD), "outcome names setting: {text}");
        assert_eq!(
            storage.load_global_setting(PASSWORD).unwrap().as_deref(),
            Some(PASSWORD_TOKEN)
        );

        let snapshot = config::show(None, storage).unwrap();
        let snap_text = serde_json::to_string(&snapshot).unwrap();
        assert!(!snap_text.contains(PASSWORD_TOKEN), "snapshot leaked");
        let entry = snapshot["global_settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == PASSWORD)
            .expect("password entry");
        assert_eq!(entry["present"], Value::Bool(true));
        assert_eq!(entry["length"], Value::from(PASSWORD_TOKEN.len()));
    });
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

const PASSWORD_TOKEN: &str = "cf-api-token-shhh-123";

#[test]
fn email_password_stdin_path_still_persists() {
    with_isolated_storage("notify-email-stdin-password", |_db_path, storage| {
        let outcome = config_write::set_setting_stdin_content(
            None,
            PASSWORD,
            &format!("  {PASSWORD_TOKEN}  \n"),
            storage,
        )
        .unwrap();
        assert!(
            !serde_json::to_string(&outcome)
                .unwrap()
                .contains(PASSWORD_TOKEN)
        );
        assert_eq!(
            storage.load_global_setting(PASSWORD).unwrap().as_deref(),
            Some(PASSWORD_TOKEN)
        );
    });
}

#[test]
fn dispatch_direct_email_password_persists_and_rejects_other_secret_direct() {
    with_isolated_storage("notify-email-dispatch", |_db_path, storage| {
        let outcome =
            config_write::dispatch_set(None, PASSWORD, Some(PASSWORD_TOKEN), false, storage)
                .expect("dispatch must accept a direct email password");
        assert!(
            !serde_json::to_string(&outcome)
                .unwrap()
                .contains(PASSWORD_TOKEN)
        );
        assert_eq!(
            storage.load_global_setting(PASSWORD).unwrap().as_deref(),
            Some(PASSWORD_TOKEN)
        );

        let error = config_write::dispatch_set(
            None,
            "PHASEGENT_NOTIFY_WEBHOOK_TOKEN",
            Some("webhook-direct-shhh"),
            false,
            storage,
        )
        .expect_err("other secrets must reject direct dispatch");
        assert!(
            error.contains("does not accept a direct value"),
            "got: {error}"
        );
        assert!(!error.contains("webhook-direct-shhh"), "got: {error}");
    });
}

#[test]
fn email_username_accepts_arbitrary_nonempty_values() {
    with_isolated_storage("notify-email-username", |_db_path, storage| {
        for username in ["api_token", "user:name", "smtp-user"] {
            config_write::set_setting_value(None, USERNAME, username, storage)
                .unwrap_or_else(|error| panic!("username '{username}' must persist: {error}"));
            assert_eq!(
                storage.load_global_setting(USERNAME).unwrap().as_deref(),
                Some(username)
            );
        }
        let error = config_write::set_setting_value(None, USERNAME, "bad\nname", storage)
            .expect_err("control characters must be rejected");
        assert!(error.contains("control characters"), "got: {error}");
        let long = "u".repeat(2049);
        let error = config_write::set_setting_value(None, USERNAME, &long, storage)
            .expect_err("overlong username must be rejected");
        assert!(error.contains("too long"), "got: {error}");
    });
}

#[test]
fn email_addresses_without_at_are_still_rejected() {
    with_isolated_storage("notify-email-address", |_db_path, storage| {
        for setting in [
            "PHASEGENT_NOTIFY_EMAIL_FROM",
            "PHASEGENT_NOTIFY_EMAIL_TO",
            "PHASEGENT_NOTIFY_EMAIL_REPLY_TO",
        ] {
            let error = config_write::set_setting_value(None, setting, "api_token", storage)
                .expect_err("address without @ must be rejected");
            assert!(error.contains("must contain '@'"), "{setting}: {error}");
        }
    });
}

#[test]
fn email_tls_auto_is_persisted_and_invalid_modes_rejected() {
    with_isolated_storage("notify-email-tls-set", |_db_path, storage| {
        for mode in ["auto", "implicit", "starttls", "plain"] {
            config_write::set_setting_value(None, TLS, mode, storage)
                .unwrap_or_else(|error| panic!("TLS mode '{mode}' must persist: {error}"));
            assert_eq!(
                storage.load_global_setting(TLS).unwrap().as_deref(),
                Some(mode)
            );
        }
        let error = config_write::set_setting_value(None, TLS, "opportunistic", storage)
            .expect_err("unknown TLS mode must be rejected");
        assert!(
            error.contains("expected auto, implicit, starttls, or plain"),
            "got: {error}"
        );
    });
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

#[test]
fn notify_config_load_accepts_auto_and_rejects_unknown_tls() {
    with_isolated_storage("notify-email-load", |_db_path, storage| {
        let _env = EnvRemoveGuard::remove(&[
            "PHASEGENT_NOTIFY_ENABLED",
            "PHASEGENT_NOTIFY_CHANNEL",
            "PHASEGENT_NOTIFY_EMAIL_SMTP_HOST",
            "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT",
            "PHASEGENT_NOTIFY_EMAIL_TLS",
            "PHASEGENT_NOTIFY_EMAIL_USERNAME",
            "PHASEGENT_NOTIFY_EMAIL_PASSWORD",
            "PHASEGENT_NOTIFY_EMAIL_FROM",
            "PHASEGENT_NOTIFY_EMAIL_TO",
            "PHASEGENT_NOTIFY_EMAIL_REPLY_TO",
        ]);
        storage
            .save_global_setting("PHASEGENT_NOTIFY_ENABLED", "true")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_CHANNEL", "email")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_EMAIL_SMTP_HOST", "smtp.mx.cloudflare.net")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_EMAIL_SMTP_PORT", "465")
            .unwrap();
        storage.save_global_setting(TLS, "auto").unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_EMAIL_USERNAME", "api_token")
            .unwrap();
        storage
            .save_global_setting(PASSWORD, PASSWORD_TOKEN)
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_EMAIL_FROM", "alerts@example.com")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_NOTIFY_EMAIL_TO", "ops@example.com")
            .unwrap();

        let config = crate::notifications::config::load(storage)
            .unwrap()
            .expect("email config must load");
        assert_eq!(config.email_tls.as_deref(), Some("auto"));
        assert_eq!(
            resolve_email_tls(config.email_tls.as_deref(), config.email_smtp_port),
            EmailTlsSelection::Implicit
        );

        storage.save_global_setting(TLS, "bogus").unwrap();
        let error = crate::notifications::config::load(storage).unwrap_err();
        assert!(
            error.contains("expected auto, implicit, starttls, or plain"),
            "got: {error}"
        );
    });
}

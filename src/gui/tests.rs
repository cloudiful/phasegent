//! Focused GUI boundary tests: input validation and redaction.
//!
//! Pure helpers are exercised without network or real credentials.
//! The snapshot test drives an isolated throwaway database via
//! `PHASEGENT_DB_PATH` so the operator's real config is untouched.

use super::*;

#[test]
fn app_metadata_contains_no_secrets() {
    let metadata = app_metadata();
    assert_eq!(metadata.name, "phasegent");
    assert_eq!(metadata.identifier, "com.cloud1ful.phasegent");
    assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
    let encoded = serde_json::to_string(&metadata).expect("metadata must serialize");
    for forbidden in ["token", "secret", "password", "credential", "api-key"] {
        assert!(
            !encoded.to_ascii_lowercase().contains(forbidden),
            "metadata must not mention {forbidden}: {encoded}"
        );
    }
}

#[test]
fn config_snapshot_helper_is_redacted() {
    let dir = std::env::temp_dir().join(format!(
        "phasegent-gui-snapshot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let db_path = dir.join("phasegent.sqlite3");
    let guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let snapshot = read_config_snapshot().expect("snapshot must render");
    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
    let encoded = serde_json::to_string(&snapshot).expect("snapshot must serialize");
    assert!(encoded.contains("database_path"));
    assert!(encoded.contains("roles"));
}

#[test]
fn task_limit_validation_is_bounded() {
    assert_eq!(validate_task_limit(None).unwrap(), 20);
    assert_eq!(validate_task_limit(Some(1)).unwrap(), 1);
    assert_eq!(validate_task_limit(Some(50)).unwrap(), 50);
    assert!(validate_task_limit(Some(0)).is_err());
    assert!(validate_task_limit(Some(51)).is_err());
}

#[test]
fn task_state_validation_rejects_unknown() {
    assert_eq!(validate_task_state(None).unwrap(), "open");
    assert_eq!(validate_task_state(Some("closed")).unwrap(), "closed");
    assert_eq!(validate_task_state(Some("ALL")).unwrap(), "all");
    assert!(validate_task_state(Some("bogus")).is_err());
    assert!(validate_task_state(Some("")).unwrap() == "open");
}

#[test]
fn role_and_provider_parsing_is_validated() {
    assert_eq!(
        parse_role_with_default(None).unwrap(),
        crate::policy::Role::Executor
    );
    assert_eq!(
        parse_role_with_default(Some("admin")).unwrap(),
        crate::policy::Role::Admin
    );
    assert!(parse_role_with_default(Some("bogus")).is_err());
    assert!(parse_provider_optional(None).unwrap().is_none());
    assert_eq!(
        parse_provider_optional(Some("redmine")).unwrap(),
        Some(crate::providers::config::ProviderKind::Redmine)
    );
    assert!(parse_provider_optional(Some("bogus")).is_err());
    assert!(parse_role_required("  ").is_err());
    assert!(parse_provider_required("").is_err());
}

#[test]
fn secret_settings_are_rejected_from_plain_path() {
    let secret = canonical_non_secret_setting("PHASEGENT_INDEX_PG_URL");
    assert!(secret.is_err());
    assert!(secret.unwrap_err().contains("credential path"));
    assert!(canonical_non_secret_setting("bogus-setting").is_err());
    assert_eq!(
        canonical_non_secret_setting("api-base").unwrap(),
        "PHASEGENT_API_BASE"
    );
}

#[test]
fn credential_and_setting_values_are_bounded() {
    assert!(validate_credential_value("  ").is_err());
    assert!(validate_credential_value("valid-token-123").is_ok());
    assert!(validate_credential_value("bad\x01token").is_err());
    assert!(validate_setting_value("PHASEGENT_API_BASE", "  ").is_err());
    assert!(validate_setting_value("PHASEGENT_API_BASE", "https://x.example").is_ok());
    // Secrets never echo in errors.
    let err = validate_credential_value("  ").unwrap_err();
    assert!(!err.contains("hunter2"));
}

#[test]
fn bound_message_truncates_and_strips_controls() {
    let long = "a".repeat(500);
    let bounded = bound_message(&long);
    assert!(bounded.len() <= 300);
    let with_controls = "ok\x01\x02 message\nwith newline";
    let cleaned = bound_message(with_controls);
    assert!(!cleaned.contains('\n'));
    assert!(!cleaned.contains('\x01'));
}

#[test]
fn task_and_status_payloads_serialize_without_secrets() {
    let payload = TasksPayload {
        branch: Some("feature/x".to_owned()),
        bound_issue: Some(149),
        provider: "redmine".to_owned(),
        role: "executor".to_owned(),
        items: vec![TaskEntry {
            number: 149,
            title: "Example task".to_owned(),
            state: "open".to_owned(),
            url: Some("https://redmine.example/issues/149".to_owned()),
        }],
        total_count: Some(1),
        has_more: false,
        data_source: "provider".to_owned(),
        fetched_at: 1_700_000_000,
        warning: None,
    };
    let encoded = serde_json::to_string(&payload).expect("tasks must serialize");
    for forbidden in [
        "token",
        "secret",
        "password",
        "credential",
        "api-key",
        "userinfo",
    ] {
        assert!(!encoded.to_ascii_lowercase().contains(forbidden));
    }
    let status = StatusPayload {
        branch: Some("feature/x".to_owned()),
        bound_issue: Some(149),
        bound_issue_title: Some("Example".to_owned()),
        bound_issue_state: Some("open".to_owned()),
        provider: "redmine".to_owned(),
        role: "executor".to_owned(),
        endpoint: Some("https://redmine.example".to_owned()),
        connection: "connected".to_owned(),
        running_timers: 0,
        recent_timers: vec![],
        fetched_at: 1_700_000_000,
        warning: None,
        statuses_unsupported: None,
        frontend_dist_hash: Some("0011223344556677".to_owned()),
    };
    let encoded = serde_json::to_string(&status).expect("status must serialize");
    assert!(encoded.contains("connected"));
}

#[test]
fn sanitize_url_never_echoes_userinfo() {
    let sanitized =
        crate::config_snapshot::sanitize_url("https://user:secret@example.com/x?token=hush#frag");
    assert_eq!(sanitized, "https://example.com/x");
    assert!(!sanitized.contains("user"));
    assert!(!sanitized.contains("secret"));
}

/// Minimal scoped env guard so parallel tests do not leak
/// `PHASEGENT_DB_PATH` overrides.
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

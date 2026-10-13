//! Role and kind denial gates: every denial is a structured `permission`
//! envelope on stderr with exit 3 and nothing on stdout, and it fires
//! before the provider is resolved.

use super::fixtures::{
    Scratch, error_envelope, run_local, run_local_with_role, run_local_with_session,
};
use super::support::{stderr_text, stdout_text};

#[cfg(unix)]
fn isolated_raw_context(
    scratch: &Scratch,
    variable: &str,
    value: &std::ffi::OsStr,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(super::support::phasegent_bin())
        .args(args)
        .current_dir(scratch.path())
        .env_clear()
        .env("PHASEGENT_CONFIG_PATH", scratch.path().join("missing.toml"))
        .env("PHASEGENT_DB_PATH", scratch.path().join("runtime.sqlite3"))
        .env("PHASEGENT_LOCAL_DB_PATH", scratch.local_db())
        .env(variable, value)
        .output()
        .expect("spawn isolated context verification")
}

#[cfg(unix)]
#[test]
fn reviewer_operator_local_issue_create_and_read_round_trip() {
    let scratch = Scratch::new();
    let created = isolated_raw_context(
        &scratch,
        "REVIEWER_TEST",
        std::ffi::OsStr::new(""),
        &[
            "--provider",
            "local",
            "issue",
            "create",
            "--title",
            "isolated operator verification",
            "--body",
            "verification body",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let issue = super::fixtures::json_stdout(&created);
    assert_eq!(issue["title"], "isolated operator verification");
    let id = issue["id"].as_u64().expect("created issue id").to_string();
    let fetched = isolated_raw_context(
        &scratch,
        "REVIEWER_TEST",
        std::ffi::OsStr::new(""),
        &["--provider", "local", "issue", "get", &id],
    );
    assert_eq!(fetched.status.code(), Some(0), "{}", stderr_text(&fetched));
    assert_eq!(
        super::fixtures::json_stdout(&fetched)["body"],
        "verification body"
    );
}

#[cfg(unix)]
#[test]
fn reviewer_non_utf8_session_presence_must_fail_closed() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new();
    let out = isolated_raw_context(
        &scratch,
        "PHASEGENT_SESSION_ID",
        std::ffi::OsStr::from_bytes(b"\xff"),
        &["--provider", "local", "record", "list", "1"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={}",
        stdout_text(&out),
        stderr_text(&out)
    );
    assert!(stderr_text(&out).contains("managed session requires a role"));
    assert!(
        !scratch.local_db().exists(),
        "denial must precede storage access"
    );
}

#[cfg(unix)]
#[test]
fn reviewer_non_utf8_role_must_error_instead_of_operator_downgrade() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new();
    let out = isolated_raw_context(
        &scratch,
        "PHASEGENT_ROLE",
        std::ffi::OsStr::from_bytes(b"\xff"),
        &["--provider", "local", "record", "list", "1"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={}",
        stdout_text(&out),
        stderr_text(&out)
    );
    assert!(stderr_text(&out).contains("PHASEGENT_ROLE"));
    assert!(
        !scratch.local_db().exists(),
        "invalid role must precede storage access"
    );
}

/// Every denial is a structured `permission` envelope on stderr with exit
/// 3 and nothing on stdout, and it fires before the provider is resolved.
#[test]
fn record_create_denials_fire_before_any_provider_access() {
    let scratch = Scratch::new();
    let db = scratch.local_db();

    let deny = |role: &str, args: &[&str]| -> serde_json::Value {
        let out = run_local(&db, role, args);
        assert_eq!(
            out.status.code(),
            Some(3),
            "role={role} args={args:?} stderr={}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "", "a denial prints nothing on stdout");
        error_envelope(&out)
    };

    // admin is never an agent role.
    let envelope = deny(
        "admin",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "record create");

    // executor may not publish another role's kind.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "reviewer",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--review",
            "final",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reviewer")
    );

    // recon belongs to explore, not executor.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "recon",
            "--key",
            "k",
            "--recon",
            "scan",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("recon")
    );

    // explore may only publish recon...
    let envelope = deny(
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("executor")
    );

    // ...and only with explicit authorization.
    let envelope = deny(
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "recon",
            "--key",
            "k",
            "--recon",
            "scan",
            "--body",
            "n",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--authorized")
    );

    // A child role always needs the explicit flag, even for its own kind.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--authorized")
    );
}

/// Operator `record create` stays denied with the comment hint, explicit
/// `Admin` stays narrow, malformed roles error, and managed sessions fail
/// closed — all before any provider access and with nothing on stdout.
#[test]
fn operator_admin_malformed_and_managed_record_denials() {
    let scratch = Scratch::new();
    let db = scratch.local_db();

    // Operator denial carries the comment-create hint.
    let out = run_local_with_role(
        &db,
        None,
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "");
    let envelope = error_envelope(&out);
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "record create");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("use comment create instead")
    );

    // Explicit Admin publishes no record kind (narrow, unchanged).
    let out = run_local(
        &db,
        "admin",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "recon",
            "--key",
            "k",
            "--recon",
            "scan",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr_text(&out));
    assert_eq!(error_envelope(&out)["error"]["kind"], "permission");

    // Malformed role errors as an argument failure, never a downgrade.
    let out = run_local_with_role(
        &db,
        Some("wizard"),
        &["--provider", "local", "record", "list", "1"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert!(
        stderr_text(&out).contains("PHASEGENT_ROLE is invalid"),
        "malformed role must error: {}",
        stderr_text(&out)
    );

    // Managed without a role fails closed at the parser layer (exit 2,
    // argument envelope) before any provider access.
    for args in [
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
            "--authorized",
        ][..],
        &["--provider", "local", "record", "get", "1", "1"][..],
        &["--provider", "local", "record", "list", "1"][..],
    ] {
        let out = run_local_with_session(&db, None, Some("managed-session"), args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "args={args:?}: {}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "");
        assert!(
            error_envelope(&out)["error"]["message"]
                .as_str()
                .unwrap()
                .contains("managed session requires a role")
        );
    }
}

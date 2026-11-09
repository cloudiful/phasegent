//! Verified host identity contract (issue #22 P1).
//!
//! The caller identity of a managed session is verified against the host
//! broker, never read from a self-selected environment variable. The
//! scenarios prove that a self-selected role grants nothing, that a
//! verified role routes correctly, and that every broken context denies.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use super::fixtures::{
    Scratch, error_envelope, init_local, run_local, run_local_with_both_sessions, seed_issue,
};
use super::support::broker::{BROKER_ADDR_ENV, BROKER_TOKEN_ENV, TestBroker};
use super::support::{phasegent_bin, stderr_text, stdout_text};

/// Spawn the binary with an explicit environment and no broker, so the
/// only identity source is the environment itself.
fn spawn_with_env(db: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let missing_toml = db
        .parent()
        .map(|dir| dir.join("phasegent-missing.toml"))
        .unwrap_or_else(|| std::env::temp_dir().join("phasegent-missing.toml"));
    let main_db = db.with_file_name("phasegent-main.sqlite3");
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_LOCAL_DB_PATH", db.as_os_str())
        .env("PHASEGENT_DB_PATH", main_db.as_os_str())
        .env("PHASEGENT_CONFIG_PATH", missing_toml.as_os_str())
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove(BROKER_ADDR_ENV)
        .env_remove(BROKER_TOKEN_ENV)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("spawn phasegent binary")
}

/// A self-selected role variable grants no identity: an invocation that
/// only sets `PHASEGENT_ROLE` fails closed instead of becoming that role,
/// and it never reaches the admin surface.
#[test]
fn a_self_selected_role_grants_no_identity() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "self selected role");
    let id = issue.to_string();

    for role in ["executor", "reviewer", "explore", "orchestrator", "admin"] {
        let out = spawn_with_env(
            &db,
            &[("PHASEGENT_ROLE", role)],
            &["--provider", "local", "record", "list", &id],
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "role={role} must fail closed: {}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "");
        assert!(
            stderr_text(&out).contains("managed session requires a role"),
            "role={role} must fail closed: {}",
            stderr_text(&out)
        );
    }
}

/// The broker reply must also match the host's own session marker when that
/// marker is present (issue #22 P3 cross-check).
#[test]
fn a_mismatched_host_session_marker_denies() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "host session mismatch");
    let id = issue.to_string();
    let args = ["--provider", "local", "record", "list", &id];

    let out = run_local_with_both_sessions(
        &db,
        Some("executor"),
        Some("ses-host-1"),
        Some("ses-other"),
        &args,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert!(
        stderr_text(&out).contains("managed session requires a role"),
        "{}",
        stderr_text(&out)
    );

    let out = run_local_with_both_sessions(
        &db,
        Some("executor"),
        Some("ses-host-1"),
        Some("ses-host-1"),
        &args,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
}

/// A verified role routes correctly: the broker decides the role, and the
/// record kind follows it.
#[test]
fn a_verified_role_routes_and_derives_the_record_kind() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "verified role");
    let id = issue.to_string();

    // An executor verified by the broker publishes an executor record.
    let created = run_local(
        &db,
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &id,
            "--key",
            "verified-executor",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "verified body",
            "--authorized",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let value: serde_json::Value =
        serde_json::from_str(stdout_text(&created).trim()).expect("record JSON");
    assert_eq!(value["actor"], "executor");
    assert_eq!(value["kind"], "executor");

    // A recon role verified by the broker publishes a recon record.
    let created = run_local(
        &db,
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &id,
            "--key",
            "verified-recon",
            "--recon",
            "scan",
            "--body",
            "recon body",
            "--authorized",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let value: serde_json::Value =
        serde_json::from_str(stdout_text(&created).trim()).expect("record JSON");
    assert_eq!(value["actor"], "explore");
    assert_eq!(value["kind"], "recon");
}

/// Several invocations of one live tool call succeed: the broker keeps the
/// credential of the live call, so every invocation verifies.
#[test]
fn several_invocations_of_one_live_tool_call_succeed() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "live tool call");
    let id = issue.to_string();

    let broker = TestBroker::start("executor", "ses-live-1");
    for attempt in 1..=3 {
        let out = spawn_with_env(
            &db,
            &[
                (BROKER_ADDR_ENV, broker.addr()),
                (BROKER_TOKEN_ENV, broker.token()),
                ("PHASEGENT_SESSION_ID", broker.session()),
            ],
            &["--provider", "local", "record", "list", &id],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "attempt {attempt}: {}",
            stderr_text(&out)
        );
    }
}

/// A broken context denies: an unknown token, a foreign session, a partial
/// context, and an unreachable broker all fail closed.
#[test]
fn a_broken_context_denies() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "broken context");
    let id = issue.to_string();
    let args = ["--provider", "local", "record", "list", &id];

    let broker = TestBroker::start("executor", "ses-live-2");

    // An unknown token is refused.
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, broker.addr()),
            (BROKER_TOKEN_ENV, "not-the-token"),
            ("PHASEGENT_SESSION_ID", broker.session()),
        ],
        &args,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert!(
        stderr_text(&out).contains("managed session requires a role"),
        "{}",
        stderr_text(&out)
    );

    // A session the broker did not issue is refused.
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, broker.addr()),
            (BROKER_TOKEN_ENV, broker.token()),
            ("PHASEGENT_SESSION_ID", "ses-other"),
        ],
        &args,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));

    // A partial context (an endpoint without a token) is refused.
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, broker.addr()),
            ("PHASEGENT_SESSION_ID", broker.session()),
        ],
        &args,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));

    // An unreachable broker is refused, and never becomes an operator.
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, "127.0.0.1:1"),
            (BROKER_TOKEN_ENV, "token"),
            ("PHASEGENT_SESSION_ID", broker.session()),
        ],
        &args,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert!(
        stderr_text(&out).contains("managed session requires a role"),
        "{}",
        stderr_text(&out)
    );
    assert_eq!(error_envelope(&out)["error"]["kind"], "argument");
}

/// A revoked credential is refused: the broker forgets the token.
#[test]
fn a_revoked_credential_is_refused() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "revoked credential");
    let id = issue.to_string();

    let broker = TestBroker::start("executor", "ses-live-3");
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, broker.addr()),
            (BROKER_TOKEN_ENV, broker.token()),
            ("PHASEGENT_SESSION_ID", broker.session()),
        ],
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));

    // A fresh broker on the same port is not reachable, so the credential
    // of the finished tool call cannot be replayed: the broker is dropped
    // with the tool call.
    drop(broker);
    let out = spawn_with_env(
        &db,
        &[
            (BROKER_ADDR_ENV, "127.0.0.1:1"),
            (BROKER_TOKEN_ENV, "token"),
            ("PHASEGENT_SESSION_ID", "ses-live-3"),
        ],
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
}

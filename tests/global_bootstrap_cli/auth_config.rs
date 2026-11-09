//! Human provisioning compatibility (issue #22 P4).
//!
//! `--target-role` is a human-only storage destination, never a caller
//! identity. These cases exercise the CLI against a real SQLite store: the
//! default and selected rows land where the flag says, global settings reject
//! the flag, and every verified agent or managed-without-role caller is denied
//! before any secret or storage access.

use super::fixtures::{run_cli_in_with_role, run_cli_in_with_role_stdin};
use super::support::{make_test_db, stderr_text};
use rusqlite::Connection;

fn working_dir(db: &std::path::Path) -> &std::path::Path {
    db.parent().expect("database temp directory")
}

fn read_one<T: rusqlite::types::FromSql>(
    db: &std::path::Path,
    sql: &str,
    params: impl rusqlite::Params,
) -> Option<T> {
    Connection::open(db)
        .expect("open test database")
        .query_row(sql, params, |row| row.get(0))
        .ok()
}

#[test]
fn operator_target_role_writes_one_forgejo_credential_row() {
    let db = make_test_db("http://127.0.0.1:1");
    let output = run_cli_in_with_role_stdin(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        Some("executor-token\n"),
        &[
            "admin",
            "auth",
            "setup",
            "--provider",
            "forgejo",
            "--repository",
            "owner/repo",
            "--api-base",
            "https://forgejo.example",
            "--target-role",
            "executor",
            "--stdin",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["role"], "executor");
    assert_eq!(report["provider"], "forgejo");

    let conn = Connection::open(&db.path).unwrap();
    let rows: Vec<(String, String, String, String, String)> = conn
        .prepare(
            "SELECT rc.role, f.api_base, f.repository, rc.credential, f.source \
             FROM role_credential rc JOIN role_forgejo_config f ON f.role = rc.role \
             WHERE rc.provider = 'forgejo' ORDER BY rc.role",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(
            "executor".to_owned(),
            "https://forgejo.example".to_owned(),
            "owner/repo".to_owned(),
            "executor-token".to_owned(),
            "admin-auth-setup".to_owned(),
        )]
    );
}

#[test]
fn operator_default_storage_role_remains_admin() {
    let db = make_test_db("http://127.0.0.1:1");
    let output = run_cli_in_with_role_stdin(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        Some("admin-token\n"),
        &[
            "admin",
            "auth",
            "setup",
            "--provider",
            "forgejo",
            "--repository",
            "owner/repo",
            "--api-base",
            "https://forgejo.example",
            "--stdin",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["role"], "admin");
    let role: Option<String> = read_one(
        &db.path,
        "SELECT role FROM role_forgejo_config WHERE role = 'admin'",
        [],
    );
    assert_eq!(role.as_deref(), Some("admin"));
}

#[test]
fn role_scoped_config_writes_and_clears_the_target_row() {
    let db = make_test_db("http://127.0.0.1:1");
    let output = run_cli_in_with_role(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        &[
            "admin",
            "config",
            "set",
            "redmine-close-status-id",
            "42",
            "--target-role",
            "reviewer",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["role"], "reviewer");
    let value: Option<u64> = read_one(
        &db.path,
        "SELECT close_status_id FROM role_redmine_config WHERE role = 'reviewer'",
        [],
    );
    assert_eq!(value, Some(42));

    let cleared = run_cli_in_with_role(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        &[
            "admin",
            "config",
            "clear",
            "redmine-close-status-id",
            "--target-role",
            "reviewer",
        ],
    );
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr_text(&cleared));
    let value: Option<u64> = read_one(
        &db.path,
        "SELECT close_status_id FROM role_redmine_config WHERE role = 'reviewer'",
        [],
    );
    assert_eq!(value, None);
}

#[test]
fn global_keys_reject_target_role_before_any_write() {
    let db = make_test_db("http://127.0.0.1:1");
    let cases: [&[&str]; 2] = [
        &[
            "admin",
            "config",
            "set",
            "redmine-api-base",
            "https://redmine.example",
            "--target-role",
            "executor",
        ],
        &[
            "admin",
            "config",
            "clear",
            "redmine-api-base",
            "--target-role",
            "executor",
        ],
    ];
    for args in cases {
        let output = run_cli_in_with_role(
            working_dir(&db.path),
            &db.path,
            "http://127.0.0.1:1",
            None,
            None,
            args,
        );
        assert_eq!(output.status.code(), Some(2), "{}", stderr_text(&output));
        assert!(
            stderr_text(&output).contains("--target-role is not valid for global setting"),
            "{}",
            stderr_text(&output)
        );
    }
    let stored: Option<String> = read_one(
        &db.path,
        "SELECT value FROM global_setting WHERE name = 'PHASEGENT_REDMINE_API_BASE'",
        [],
    );
    assert_eq!(stored, None);

    let top_level = run_cli_in_with_role(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        &[
            "config",
            "set",
            "redmine-close-status-id",
            "42",
            "--target-role",
            "reviewer",
        ],
    );
    assert_eq!(
        top_level.status.code(),
        Some(2),
        "{}",
        stderr_text(&top_level)
    );
    assert!(
        stderr_text(&top_level).contains("only valid with `admin config set|clear`"),
        "{}",
        stderr_text(&top_level)
    );
}

#[test]
fn unknown_target_role_is_an_argument_error() {
    let db = make_test_db("http://127.0.0.1:1");
    let output = run_cli_in_with_role(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        None,
        &[
            "admin",
            "auth",
            "setup",
            "--provider",
            "local",
            "--target-role",
            "root",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr_text(&output));
    assert!(
        stderr_text(&output).contains("invalid role 'root'"),
        "{}",
        stderr_text(&output)
    );
}

#[test]
fn agents_and_managed_sessions_cannot_provision_even_with_target_role() {
    let db = make_test_db("http://127.0.0.1:1");
    for role in ["orchestrator", "executor", "reviewer", "explore"] {
        let output = run_cli_in_with_role(
            working_dir(&db.path),
            &db.path,
            "http://127.0.0.1:1",
            Some(role),
            None,
            &[
                "admin",
                "auth",
                "setup",
                "--provider",
                "local",
                "--target-role",
                "executor",
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(3),
            "agent {role} must be denied: {}",
            stderr_text(&output)
        );
        assert!(
            stderr_text(&output).contains("is not allowed to perform"),
            "{}",
            stderr_text(&output)
        );
    }

    // No denied attempt changed the executor's provider preference.
    let provider: Option<String> = read_one(
        &db.path,
        "SELECT provider FROM role_config WHERE role = 'executor'",
        [],
    );
    assert_eq!(provider.as_deref(), Some("redmine"));

    let managed = run_cli_in_with_role(
        working_dir(&db.path),
        &db.path,
        "http://127.0.0.1:1",
        None,
        Some("managed-session"),
        &[
            "admin",
            "auth",
            "setup",
            "--provider",
            "local",
            "--target-role",
            "executor",
        ],
    );
    assert_eq!(managed.status.code(), Some(2), "{}", stderr_text(&managed));
    assert!(
        stderr_text(&managed).contains("managed session requires a role"),
        "{}",
        stderr_text(&managed)
    );
}

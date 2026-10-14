//! Shared fixtures for the `record` CLI black-box contract.
//!
//! A per-test [`Scratch`] directory holds the isolated local SQLite file
//! and any `--body-file` fixtures, and `run_local` spawns the compiled
//! binary against it with a guaranteed-missing TOML overlay so no
//! developer environment leaks in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::support::{phasegent_bin, stderr_text};

/// Per-test scratch directory holding the isolated local SQLite file and
/// any `--body-file` fixtures.
pub(super) struct Scratch {
    dir: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Scratch {
    pub(super) fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = super::support::scratch_root().join(format!(
            "phasegent-it-record-{}-{}-{}",
            std::process::id(),
            nanos,
            (nanos as u64) ^ (std::process::id() as u64),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    pub(super) fn path(&self) -> &Path {
        &self.dir
    }

    pub(super) fn local_db(&self) -> PathBuf {
        self.dir.join("phasegent-local.sqlite3")
    }
}

/// Spawn the compiled binary against an isolated local store and a
/// guaranteed-missing TOML overlay, so no developer environment leaks in.
/// Clears inherited role/session env so Operator (no role, no session) and
/// managed-missing paths stay deterministic.
pub(super) fn run_local(db: &Path, role: &str, args: &[&str]) -> Output {
    run_local_with_role(db, Some(role), args)
}

/// Role-optional spawn: `None` models an unmanaged Operator (no role and
/// no managed session marker). Always clears `PHASEGENT_SESSION_ID` and the
/// host `OPENCODE_SESSION_ID` so a developer's managed session never leaks
/// into the subprocess (issue #859 P1).
pub(super) fn run_local_with_role(db: &Path, role: Option<&str>, args: &[&str]) -> Output {
    run_local_with_session(db, role, None, args)
}

/// Fully injectable spawn: `role` models `PHASEGENT_ROLE` (`None` removes
/// it), `session` models `PHASEGENT_SESSION_ID` (`None` removes it, `Some`
/// sets it even when blank so managed-missing stays fail-closed).
/// `host_session` models `OPENCODE_SESSION_ID` the same way; `None` removes
/// it so ordinary Operator stays deterministic.
pub(super) fn run_local_with_session(
    db: &Path,
    role: Option<&str>,
    session: Option<&str>,
    args: &[&str],
) -> Output {
    run_local_with_both_sessions(db, role, session, None, args)
}

/// Fully injectable spawn with both managed markers: `role` models
/// `PHASEGENT_ROLE`, `session` models `PHASEGENT_SESSION_ID`, `host_session`
/// models `OPENCODE_SESSION_ID` (`None` removes, `Some` sets even when
/// blank so managed-missing stays fail-closed).
pub(super) fn run_local_with_both_sessions(
    db: &Path,
    role: Option<&str>,
    session: Option<&str>,
    host_session: Option<&str>,
    args: &[&str],
) -> Output {
    let missing_toml = db
        .parent()
        .map(|dir| dir.join("phasegent-missing.toml"))
        .unwrap_or_else(|| std::env::temp_dir().join("phasegent-missing.toml"));
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_LOCAL_DB_PATH", db.as_os_str())
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_DB_PATH")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env("PHASEGENT_CONFIG_PATH", missing_toml.as_os_str())
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    if let Some(session) = session {
        command.env("PHASEGENT_SESSION_ID", session);
    }
    if let Some(host_session) = host_session {
        command.env("OPENCODE_SESSION_ID", host_session);
    }
    command.output().expect("spawn phasegent binary")
}

/// Initialize the local store (schema + seeds) with a read that touches
/// no issue.
pub(super) fn init_local(db: &Path) {
    let out = run_local(
        db,
        "executor",
        &["--provider", "local", "record", "list", "1"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "local store init failed: {}",
        stderr_text(&out)
    );
}

/// Insert one local issue directly so the test never depends on the
/// provisioning-only `issue create` surface.
pub(super) fn seed_issue(db: &Path, title: &str) -> u64 {
    let conn = rusqlite::Connection::open(db).expect("open local db");
    conn.execute(
        "INSERT INTO local_issues \
         (title, body, status, project, tracker, author_role, created_at, updated_at) \
         VALUES (?1, 'body', 'New', 'default', 'Task', 'orchestrator', 1000, 1000)",
        rusqlite::params![title],
    )
    .expect("insert local issue");
    conn.last_insert_rowid() as u64
}

pub(super) fn json_stdout(output: &Output) -> serde_json::Value {
    serde_json::from_str(super::support::stdout_text(output).trim()).expect("JSON on stdout")
}

pub(super) fn error_envelope(output: &Output) -> serde_json::Value {
    serde_json::from_str(stderr_text(output).trim()).expect("structured error on stderr")
}

#![allow(dead_code)]
//! Shared scaffolding for the `snapshots_*` integration tests.
//!
//! Every scenario launches the compiled `phasegent` binary against a private
//! scratch root: the config/lease database, the local provider store, and the
//! lexical index all live inside it, and the ambient `PHASEGENT_*` overrides a
//! host may export are removed, so a snapshot can never depend on the
//! developer's machine state. Provider, git, and lease fixtures live in
//! [`fixtures`].

#[path = "fixtures.rs"]
pub mod fixtures;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Absolute path to the compiled `phasegent` binary, resolved by Cargo at
/// compile time so a test can never run a different binary.
pub fn phasegent_bin() -> &'static str {
    env!("CARGO_BIN_EXE_phasegent")
}

/// Per-test scratch directory; `Drop` removes the whole tree, including the
/// git worktrees a fixture created inside it.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "phasegent-snapshots-{label}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).expect("create scratch dir");
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Create (or reuse) a plain scratch subdirectory that is not a git
    /// checkout. Fixtures that must not trigger the create-time worktree
    /// auto-acquire run from here: outside a repository the identity probe
    /// fails first, so nothing is acquired and no cache directory is touched.
    pub fn dir(&self, name: &str) -> PathBuf {
        let dir = self.join(name);
        fs::create_dir_all(&dir).expect("create scratch subdir");
        dir
    }

    /// Canonical form of the scratch root: the snapshots filter the paths the
    /// binary prints back, and git resolves its own prefixes.
    pub fn canonical_root(&self) -> PathBuf {
        fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Run the compiled binary from `cwd` with a hermetic `PHASEGENT_*`
/// environment: the scratch databases are pinned and every host override a
/// developer may have exported is removed. `role` is passed explicitly
/// through `PHASEGENT_ROLE`; `None` removes it.
pub fn run(scratch: &Scratch, cwd: &Path, role: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .current_dir(cwd)
        .env("PHASEGENT_DB_PATH", scratch.join("phasegent.sqlite3"))
        .env(
            "PHASEGENT_LOCAL_DB_PATH",
            scratch.join("phasegent-local.sqlite3"),
        )
        .env(
            "PHASEGENT_INDEX_DB_PATH",
            scratch.join("phasegent-index.sqlite3"),
        )
        .env(
            "PHASEGENT_CONFIG_PATH",
            scratch.join("phasegent-missing.toml"),
        )
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env_remove("PHASEGENT_WORKTREE_NO_DISCOVER")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    command.output().expect("spawn phasegent binary")
}

pub fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run a command that must succeed and parse its JSON stdout document.
pub fn expect_json(
    scratch: &Scratch,
    cwd: &Path,
    role: Option<&str>,
    args: &[&str],
) -> serde_json::Value {
    let output = run(scratch, cwd, role, args);
    assert!(
        output.status.success(),
        "{args:?} exited with {}: stderr={}",
        output.status,
        stderr_text(&output),
    );
    let stdout = stdout_text(&output);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("{args:?} did not print one JSON document ({error}): {stdout}")
    })
}

/// Escape `value` so it can be used as a literal snapshot filter pattern:
/// insta's filters are regular expressions.
pub fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if !character.is_alphanumeric() && character != '_' {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

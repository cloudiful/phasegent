//! P2 foundation tests: isolated temp DB/repos only, never the live store.
//!
//! Shared fixtures (scratch repos, temp databases, env isolation) live
//! here for every child module; scope-selection tests sit at this level
//! because they gate all three flows (bind, import, hooks).

use std::path::{Path, PathBuf};
use std::process::Command;

mod identity;
mod import;
mod links;
mod reads;

#[cfg(test)]
mod cli;
mod compat;

pub(crate) fn open_memory_db() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory database must open");
    crate::branch_links::ensure_schema(&connection).expect("branch link schema must initialise");
    connection
}

pub(crate) fn unique_scratch(label: &str) -> PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-branch-links-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir must create");
    dir
}

pub(crate) struct TempRepo {
    pub(crate) dir: PathBuf,
}

impl TempRepo {
    pub(crate) fn init(label: &str) -> Self {
        let dir = unique_scratch(label);
        run_git(&dir, &["init", "-q", "-b", "main"]);
        run_git(
            &dir,
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        );
        Self { dir }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn set_origin(&self, url: &str) {
        run_git(&self.dir, &["remote", "add", "origin", url]);
    }

    pub(crate) fn set_binding(&self, branch: &str, issue: u64) {
        let key = crate::branch_context::config_key(branch);
        run_git(&self.dir, &["config", "--local", &key, &issue.to_string()]);
    }

    pub(crate) fn get_binding(&self, branch: &str) -> Option<String> {
        let key = crate::branch_context::config_key(branch);
        let output = Command::new("git")
            .args(["config", "--local", "--get", &key])
            .current_dir(&self.dir)
            .output()
            .expect("git config --get must run");
        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            None
        }
    }

    pub(crate) fn checkout_branch(&self, branch: &str) {
        run_git(&self.dir, &["checkout", "-q", "-B", branch]);
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Remove provider-selection env vars for scope tests, restoring them
/// on drop. Callers hold the workflow lock, like `EnvGuard`.
pub(crate) struct EnvClear {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvClear {
    fn clear(names: &[&'static str]) -> Self {
        let mut previous = Vec::new();
        for name in names {
            previous.push((*name, std::env::var_os(name)));
            // SAFETY: held under `lock_workflow_tests`, like `EnvGuard`.
            unsafe {
                std::env::remove_var(name);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvClear {
    fn drop(&mut self) {
        // SAFETY: symmetric to `clear` above.
        unsafe {
            for (name, value) in self.previous.drain(..) {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

pub(crate) fn clear_provider_env() -> EnvClear {
    EnvClear::clear(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"])
}

/// Pin the main database at an isolated temp file for scope tests.
pub(crate) fn pin_db(label: &str) -> (PathBuf, crate::infra::storage::test_support::EnvGuard) {
    let dir = unique_scratch(label);
    let db = dir.join("phasegent.sqlite3");
    let guard = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    (dir, guard)
}

// ---------------------------------------------------------------------------
// Link scope selection: explicit and stored scopes resolve locally,
// missing projects are BLOCKED, and nothing is ever guessed.
// ---------------------------------------------------------------------------

#[test]
fn explicit_scopes_resolve_without_network_or_storage() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::policy::Role;
    use crate::providers::ProviderKind;
    let _lock = lock_workflow_tests();
    let _env = clear_provider_env();
    let (_dir, _db) = pin_db("scope-explicit");

    let scope = crate::branch_links::resolve_link_scope(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        Some("tools-phasegent"),
    )
    .expect("redmine scope must resolve")
    .expect("redmine scope must be some");
    assert_eq!(scope.provider, "redmine");
    assert_eq!(scope.project, "tools-phasegent");

    let scope = crate::branch_links::resolve_link_scope(
        Some(Role::Orchestrator),
        Some(ProviderKind::Gitlab),
        None,
        Some("42"),
    )
    .expect("gitlab scope must resolve")
    .expect("gitlab scope must be some");
    assert_eq!(scope.provider, "gitlab");
    assert_eq!(scope.project, "42");

    let scope = crate::branch_links::resolve_link_scope(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
    )
    .expect("local scope must resolve")
    .expect("local scope must be some");
    assert_eq!(scope.provider, "local");
}

#[test]
fn missing_project_is_blocked_not_guessed() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::policy::Role;
    use crate::providers::ProviderKind;
    let _lock = lock_workflow_tests();
    let _env = clear_provider_env();
    let (_dir, _db) = pin_db("scope-blocked");

    let error = crate::branch_links::resolve_link_scope(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
    )
    .expect_err("redmine without project must be blocked");
    assert!(
        error.contains("--project-id"),
        "blocked message must name the missing scope; got: {error}"
    );

    let error = crate::branch_links::resolve_link_scope(
        Some(Role::Orchestrator),
        Some(ProviderKind::Gitlab),
        None,
        Some("nope"),
    )
    .expect_err("gitlab with non-numeric project must be blocked");
    assert!(error.contains("numeric"), "got: {error}");
}

#[test]
fn unselected_provider_stays_unresolved() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::policy::Role;
    let _lock = lock_workflow_tests();
    let _env = clear_provider_env();
    let (dir, _db) = pin_db("scope-none");
    // Empty temp database: nothing stored, nothing explicit, no env.
    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    assert!(
        storage
            .load_role_config(Role::Orchestrator)
            .expect("read must work")
            .is_none()
    );
    let scope = crate::branch_links::resolve_link_scope(Some(Role::Orchestrator), None, None, None)
        .expect("resolution must not fail");
    assert_eq!(
        scope, None,
        "no selection means no scope: no provider default is ever guessed for links"
    );
}

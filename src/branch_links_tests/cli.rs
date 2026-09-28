//! P3 scoped bind/status/unbind flows end to end: isolated temp repo
//! plus temp DB under the workflow lock; the live store is never
//! touched. Scope, compat, snapshot, and document-shape coverage lives
//! with the domain test modules; this file asserts flow behavior (exit
//! codes plus database/Git state).

use super::{TempRepo, clear_provider_env, pin_db};
use crate::branch_links;
use crate::command::IssueCommand;
use crate::infra::storage::test_support::EnvGuard;
use crate::policy::Role;
use crate::providers::ProviderKind;
use std::path::{Path, PathBuf};

mod bind;
mod branches;
mod status;
mod unbind;

const BRANCH: &str = "feat/628";

fn in_temp_repo<T>(repo: &TempRepo, action: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(&repo.dir).expect("set cwd to temp repo");
    let result = action();
    let _ = std::env::set_current_dir(&previous);
    result
}

fn db_links(db: &Path, repo_key: &str, branch: &str) -> Vec<branch_links::LinkedIssue> {
    let storage = crate::infra::storage::Storage::open_at(&db.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    branch_links::ensure_schema(&storage.connection).expect("schema");
    branch_links::issues_for_branch(
        &storage.connection,
        repo_key,
        branch,
        true,
        &branch_links::UnknownState,
    )
    .expect("read must work")
}

fn redmine_scope() -> (Option<ProviderKind>, Option<String>, Option<String>) {
    (
        Some(ProviderKind::Redmine),
        None,
        Some("tools-phasegent".to_owned()),
    )
}

fn scoped_env(label: &str) -> (PathBuf, EnvGuard, EnvGuard, super::EnvClear) {
    let (dir, db) = pin_db(label);
    let index = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("absent-index.sqlite3")
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    );
    (dir, db, index, clear_provider_env())
}

fn scoped_bind(repo: &TempRepo, issue_id: u64) -> i32 {
    let (provider, repository, project) = redmine_scope();
    in_temp_repo(repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            provider,
            repository.as_deref(),
            project.as_deref(),
            IssueCommand::Bind {
                issue_id,
                replace: false,
                session: None,
            },
        )
    })
}

fn scoped_status(repo: &TempRepo) -> i32 {
    let (provider, repository, project) = redmine_scope();
    in_temp_repo(repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            provider,
            repository.as_deref(),
            project.as_deref(),
            IssueCommand::StatusBranch,
        )
    })
}

fn scoped_unbind(repo: &TempRepo) -> i32 {
    let (provider, repository, project) = redmine_scope();
    in_temp_repo(repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            provider,
            repository.as_deref(),
            project.as_deref(),
            IssueCommand::Unbind,
        )
    })
}

fn local_key(repo: &TempRepo) -> String {
    branch_links::resolve_repo_key(None, &repo.dir)
        .expect("fallback key")
        .key
}

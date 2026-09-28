//! Durable branch-link hook resolution tests (issue 628 P3 and later).
//!
//! Moved verbatim from `hooks_tests.rs` so hook installation coverage
//! and durable-link coverage live in cohesive modules. Shared fixtures
//! (`TempRepo`, message runners, file helpers) stay in the parent.

use super::{TempRepo, checkout_main, read_file, run_prepare, write_file};
use crate::branch_context::{self, GitRunner};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Durable branch-link resolution (issue 628 P3).
// ---------------------------------------------------------------------------

fn pin_links_db(tag: &str) -> (PathBuf, EnvGuard) {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-hooks-links-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        dir.join("phasegent.sqlite3")
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    );
    (dir, guard)
}

fn repo_toplevel(repo: &TempRepo) -> PathBuf {
    let output = repo
        .runner()
        .run(&["rev-parse", "--show-toplevel"])
        .expect("toplevel resolves");
    assert_eq!(output.status, 0);
    PathBuf::from(output.stdout.trim())
}

fn seed_link(toplevel: &Path, branch: &str, issue: u64) {
    seed_scoped_link(toplevel, branch, "redmine", "tools-phasegent", issue);
}

fn seed_scoped_link(toplevel: &Path, branch: &str, provider: &str, project: &str, issue: u64) {
    let key = crate::branch_links::resolve_repo_key(None, toplevel)
        .expect("fallback key")
        .key;
    let storage = crate::infra::storage::Storage::open().expect("temp storage must open");
    crate::branch_links::ensure_schema(&storage.connection).expect("schema");
    let issue_key = crate::branch_links::IssueKey::new(provider, project, issue.to_string())
        .expect("issue key");
    crate::branch_links::store::link(
        &storage.connection,
        &crate::branch_links::LinkParams {
            repo_key: &key,
            branch,
            issue: &issue_key,
            issue_number: issue,
            source: "test",
            now: 1_700_000_001,
        },
    )
    .expect("link must insert");
}

fn checkout_branch(repo: &TempRepo, branch: &str) {
    let output = repo
        .runner()
        .run(&["checkout", "-q", "-B", branch])
        .expect("checkout works");
    assert_eq!(output.status, 0);
}

#[test]
fn prepare_prefers_durable_link_over_legacy_binding() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-db-first") else {
        return;
    };
    let (_dir, _db) = pin_links_db("db-first");
    checkout_branch(&repo, "feat/700");
    let toplevel = repo_toplevel(&repo);
    seed_link(&toplevel, "feat/700", 700);
    branch_context::bind(&repo.runner(), 701, false).unwrap();

    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Work on the linked issue\n");
    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(value["action"], "appended");
    assert_eq!(
        read_file(&file),
        b"Work on the linked issue\n\nRefs #700\n",
        "the durable link wins over the legacy binding"
    );
}

#[test]
fn prepare_falls_back_to_legacy_binding_without_links() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-legacy") else {
        return;
    };
    let (_dir, _db) = pin_links_db("legacy");
    checkout_branch(&repo, "feat/701");
    branch_context::bind(&repo.runner(), 701, false).unwrap();

    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Legacy-bound work\n");
    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(value["action"], "appended");
    assert_eq!(read_file(&file), b"Legacy-bound work\n\nRefs #701\n");
}

#[test]
fn prepare_stays_silent_on_ambiguous_links() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-ambiguous") else {
        return;
    };
    let (_dir, _db) = pin_links_db("ambiguous");
    checkout_branch(&repo, "feat/shared");
    let toplevel = repo_toplevel(&repo);
    seed_link(&toplevel, "feat/shared", 700);
    seed_link(&toplevel, "feat/shared", 701);

    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Shared branch work\n");
    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(value["action"], "noop", "ambiguous links are never guessed");
    assert_eq!(read_file(&file), b"Shared branch work\n");
}

#[test]
fn prepare_stays_silent_on_cross_scope_same_number_links() {
    // Two distinct provider/project identities sharing one number are
    // never collapsed into a single active issue (issue 628): the hook
    // stays silent instead of stamping the shared number.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-cross-scope") else {
        return;
    };
    let (_dir, _db) = pin_links_db("cross-scope");
    checkout_branch(&repo, "feat/shared");
    let toplevel = repo_toplevel(&repo);
    seed_scoped_link(&toplevel, "feat/shared", "redmine", "tools-phasegent", 700);
    seed_scoped_link(&toplevel, "feat/shared", "forgejo", "acme/widgets", 700);

    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Shared number work\n");
    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(
        value["action"], "noop",
        "cross-scope duplicates are never guessed"
    );
    assert_eq!(read_file(&file), b"Shared number work\n");
}

#[test]
fn prepare_never_resolves_the_detected_default_branch() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-default") else {
        return;
    };
    let (_dir, _db) = pin_links_db("default");
    checkout_main(&repo);
    let branch = current_branch_name(&repo);
    repo.runner()
        .run(&[
            "remote",
            "add",
            "origin",
            "https://forge.example.com/owner/repo.git",
        ])
        .expect("origin add works");
    let output = repo
        .runner()
        .run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{branch}"),
        ])
        .expect("origin HEAD works");
    assert_eq!(output.status, 0);
    branch_context::bind(&repo.runner(), 701, false).unwrap();

    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Work on default\n");
    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(
        value["action"], "noop",
        "the detected default branch never resolves an active issue"
    );
    assert_eq!(read_file(&file), b"Work on default\n");
}

fn current_branch_name(repo: &TempRepo) -> String {
    let output = repo
        .runner()
        .run(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .expect("branch resolves");
    assert_eq!(output.status, 0);
    output.stdout.trim().to_owned()
}

#[test]
fn close_retention_imports_legacy_keys_without_detaching() {
    use crate::providers::config::RedmineProvider;
    use crate::providers::{ProviderDispatcher, RedmineConfig};
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::new("hook-retain") else {
        return;
    };
    let (dir, _db) = pin_links_db("retain");
    checkout_branch(&repo, "feat/628");
    branch_context::bind(&repo.runner(), 628, false).unwrap();

    let provider = ProviderDispatcher::Redmine(
        RedmineProvider::new(
            RedmineConfig {
                api_base: "http://example.invalid".to_owned(),
                project_id: Some("tools-phasegent".to_owned()),
                close_status_id: None,
            },
            "dummy-key".to_owned(),
        )
        .expect("offline dispatcher must build"),
    );
    let previous = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(&repo.0).expect("set cwd to temp repo");
    let warning = crate::cli::issue::issue_branch::retain_closed_issue_links(&provider, 628);
    let _ = std::env::set_current_dir(&previous);
    assert!(warning.is_none(), "retention must succeed silently");

    let key = crate::branch_links::resolve_repo_key(None, &repo_toplevel(&repo))
        .expect("fallback key")
        .key;
    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    let rows = crate::branch_links::issues_for_branch(
        &storage.connection,
        &key,
        "feat/628",
        false,
        &crate::branch_links::UnknownState,
    )
    .expect("read must work");
    assert_eq!(rows.len(), 1, "closed legacy binding must be retained");
    assert_eq!(rows[0].issue_number, 628);
    assert_eq!(rows[0].status, "linked", "close must never detach");
}

//! Coverage for the shared candidate guard ([`super::candidate`]) that
//! `issue close`, `issue sync`, and `worktree prune` all drive.
//!
//! These drive the shared local and host guards directly against a real
//! temp repository and a scripted OpenCode host, so they pin the two things
//! the shared extraction must preserve: every local guard fires for every
//! caller, and [`CleanupMode::Sync`] never moves a session (it only ever keeps
//! a directory it cannot prove free).

use std::path::{Path, PathBuf};

use super::CleanupMode;
use super::candidate::{local_protected_reason, occupancy_reason};
use super::target::TargetResolver;
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::{
    DirectoryLegacyIssue, DirectoryRecord, ProcessWorktreeRunner, WorktreeRunner, repo_identity,
    worktree_add,
};

/// The complete local + host protection check for one candidate record, the
/// composition every caller performs.
#[allow(clippy::too_many_arguments)]
fn protected_reason(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    identity: &str,
    records: &[DirectoryRecord],
    closing: Option<&str>,
    record: &DirectoryRecord,
) -> Option<String> {
    let directory = Path::new(&record.worktree_path);
    local_protected_reason(runner, identity, directory).or_else(|| {
        occupancy_reason(
            api,
            mode,
            target,
            directory,
            &super::associated_sessions(records, directory),
            closing,
        )
    })
}

fn scratch(label: &str) -> PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-913-guard-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch create");
    dir
}

/// A temp repo plus a clean linked worktree, or `None` when git is
/// unavailable so the whole test body degrades to a no-op.
fn repo_and_worktree(label: &str) -> Option<(PathBuf, PathBuf)> {
    let runner = ProcessWorktreeRunner::new();
    let root = scratch(label);
    if runner.run(&["init", "-q", "-b", "main"], &root).is_err() {
        return None;
    }
    let _ = runner.run(
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
        &root,
    );
    let worktree = root.join("wt");
    worktree_add(&runner, &root, &worktree, "phasegent/913-guard").ok()?;
    Some((root, worktree))
}

fn record(worktree_path: &str, session: &str) -> DirectoryRecord {
    DirectoryRecord {
        directory_id: format!("dir-{worktree_path}"),
        repo_identity: String::new(),
        checkout_path: String::new(),
        worktree_path: worktree_path.to_owned(),
        branch: "phasegent/913-guard".to_owned(),
        created_at: 0,
        tasks: Vec::new(),
        legacy_issues: vec![DirectoryLegacyIssue {
            issue: 913,
            provider: String::new(),
            project: String::new(),
            external_id: 0,
        }],
        sessions: vec![session.to_owned()],
    }
}

#[test]
fn a_clean_cli_candidate_is_not_protected() {
    let Some((root, worktree)) = repo_and_worktree("clean") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let records = vec![record(&worktree.to_string_lossy(), "session-A")];
    assert_eq!(
        protected_reason(
            &runner,
            &api,
            CleanupMode::Sync,
            &target,
            &identity,
            &records,
            None,
            &records[0],
        ),
        None,
        "a clean, unmanaged, non-main candidate must be removable"
    );
    assert!(api.moves().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_main_checkout_is_always_protected() {
    let Some((root, _worktree)) = repo_and_worktree("main") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let records = vec![record(&root.to_string_lossy(), "session-A")];
    let reason = protected_reason(
        &runner,
        &api,
        CleanupMode::Sync,
        &target,
        &identity,
        &records,
        None,
        &records[0],
    )
    .expect("the main checkout must be kept");
    assert!(reason.contains("main checkout"), "got: {reason}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_mode_keeps_a_hosted_session_without_moving_it() {
    let Some((root, worktree)) = repo_and_worktree("hosted") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &worktree);
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let records = vec![record(&worktree.to_string_lossy(), "ses_a")];
    let reason = protected_reason(
        &runner,
        &api,
        CleanupMode::Sync,
        &target,
        &identity,
        &records,
        None,
        &records[0],
    )
    .expect("a hosted session must keep the directory");
    assert!(reason.contains("still hosted here"), "got: {reason}");
    assert!(api.moves().is_empty(), "Sync must never move a session");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_mode_releases_a_vacant_directory() {
    let Some((root, worktree)) = repo_and_worktree("vacant") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &root);
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let records = vec![record(&worktree.to_string_lossy(), "ses_a")];
    assert_eq!(
        protected_reason(
            &runner,
            &api,
            CleanupMode::Sync,
            &target,
            &identity,
            &records,
            None,
            &records[0],
        ),
        None,
        "a session proven elsewhere leaves a removable directory"
    );
    assert!(api.moves().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_incomplete_host_listing_keeps_the_candidate() {
    let Some((root, worktree)) = repo_and_worktree("incomplete") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &worktree)
        .failing_list();
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let records = vec![record(&worktree.to_string_lossy(), "ses_a")];
    let reason = protected_reason(
        &runner,
        &api,
        CleanupMode::Sync,
        &target,
        &identity,
        &records,
        None,
        &records[0],
    )
    .expect("an unusable listing must keep the directory");
    assert!(reason.contains("incomplete"), "got: {reason}");
    assert!(api.moves().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// A directory the guards cannot classify is kept, not removed: the
/// main-checkout probe failing closes the door.
#[test]
fn an_unverifiable_directory_is_kept() {
    let Some((root, _worktree)) = repo_and_worktree("unverifiable") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let identity = repo_identity(&runner, &root).expect("identity");
    let target = TargetResolver::new(&root, &identity);
    let missing = root.join("does-not-exist");
    let records = vec![record(&missing.to_string_lossy(), "session-A")];
    assert!(
        protected_reason(
            &runner,
            &api,
            CleanupMode::Sync,
            &target,
            &identity,
            &records,
            None,
            &records[0],
        )
        .is_some(),
        "a directory whose guards cannot run must be kept"
    );
    let _ = std::fs::remove_dir_all(&root);
}

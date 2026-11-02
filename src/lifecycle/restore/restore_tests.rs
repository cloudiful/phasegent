//! Close-time restoration evidence against real isolated repositories.

use std::path::Path;

use crate::git_runner::ProcessGitRunner;
use crate::infra::storage::Storage;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::{ProcessWorktreeRunner, WorktreeRunner};

use super::store::*;
use super::test_support::{RealRepo, TempDb, TempDir};
use super::{
    CloseRestoreParams, RestoreOutcome, UpstreamOutcome, resolve_close_target, restore_on_close,
};

const ISSUE: u64 = 1175;
const ISSUE_BRANCH: &str = "feature/1175";
const BASELINE_BRANCH: &str = "main";
const SESSION: &str = "ses_closing";

fn insert_baseline(
    storage: &Storage,
    repo: &RealRepo,
    state: CheckoutState,
    baseline_head: &str,
) -> BaselineScope {
    ensure_schema(&storage.connection).expect("baseline schema");
    let scope = repo.scope(ISSUE);
    insert_pending(
        &storage.connection,
        &NewBaseline {
            scope: &scope,
            issue_branch: ISSUE_BRANCH,
            baseline_branch: BASELINE_BRANCH,
            baseline_head,
            clean_evidence: CLEAN_EVIDENCE,
            default_branch: Some(BASELINE_BRANCH),
            recorded_at: 1,
        },
    )
    .expect("baseline insert");
    if state != CheckoutState::Pending {
        assert!(transition_pending(&storage.connection, &scope, state).unwrap());
    }
    scope
}

fn restore(
    repo: &RealRepo,
    db: &TempDb,
    api: &FakeOpenCodeApi,
    session: Option<&str>,
) -> RestoreOutcome {
    restore_with_scope(repo, db, api, session, Some("redmine"), Some("tools"))
}

fn restore_with_scope(
    repo: &RealRepo,
    db: &TempDb,
    api: &FakeOpenCodeApi,
    session: Option<&str>,
    provider: Option<&str>,
    project: Option<&str>,
) -> RestoreOutcome {
    let worktree = ProcessWorktreeRunner::new();
    let target = resolve_close_target(&worktree, &repo.work).expect("verified main checkout");
    let git = ProcessGitRunner::in_directory(&target.main_checkout);
    let storage = db.open();
    restore_on_close(&CloseRestoreParams {
        storage: &storage,
        git: &git,
        worktree: &worktree,
        api,
        target: &target,
        provider,
        project,
        issue_number: ISSUE,
        session,
    })
}

fn read_record(db: &TempDb, repo: &RealRepo) -> BaselineRecord {
    let storage = db.open();
    read_baseline(&storage.connection, &repo.scope(ISSUE))
        .expect("baseline read")
        .expect("baseline row")
}

fn prepare_feature(repo: &RealRepo) -> String {
    repo.checkout_new(ISSUE_BRANCH);
    repo.commit_empty(&repo.work, "feature work")
}

#[test]
fn restored_branch_fast_forwards_its_own_upstream_and_repeats_as_noop() {
    let repo = RealRepo::new("restore-ff", true);
    let db = TempDb::new("restore-ff-db");
    let baseline_head = repo.head(&repo.work);
    let feature_head = prepare_feature(&repo);
    let upstream_head = repo.advance_remote();
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let api = FakeOpenCodeApi::empty();
    let outcome = restore(&repo, &db, &api, Some(SESSION));
    assert_eq!(
        outcome,
        RestoreOutcome::Restored {
            branch: BASELINE_BRANCH.to_owned(),
            update: UpstreamOutcome::Updated,
        }
    );
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    assert_eq!(repo.head(&repo.work), upstream_head);
    let record = read_record(&db, &repo);
    assert_eq!(record.state, CheckoutState::Active);
    assert_eq!(record.update_state, UpdateState::Updated);
    assert!(record.restored_at.is_some());
    assert_eq!(
        output_git(
            &repo.work,
            &["rev-parse", &format!("{ISSUE_BRANCH}^{{commit}}")]
        ),
        feature_head,
        "the task branch and its commit are preserved"
    );

    let repeat = restore(&repo, &db, &api, Some(SESSION));
    assert_eq!(repeat, RestoreOutcome::Noop);
    assert_eq!(repo.head(&repo.work), upstream_head);
}

#[test]
fn no_upstream_restores_only_and_reports_the_skipped_update() {
    let repo = RealRepo::new("restore-no-upstream", false);
    let db = TempDb::new("restore-no-upstream-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert_eq!(
        outcome,
        RestoreOutcome::Restored {
            branch: BASELINE_BRANCH.to_owned(),
            update: UpstreamOutcome::Skipped("branch has no configured remote upstream".to_owned()),
        }
    );
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    assert_eq!(repo.head(&repo.work), baseline_head);
    let record = read_record(&db, &repo);
    assert_eq!(record.update_state, UpdateState::Skipped);
    assert_eq!(
        restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION)),
        RestoreOutcome::Noop
    );
}

#[test]
fn dirty_main_and_third_party_branch_skip_without_mutating() {
    let repo = RealRepo::new("restore-dirty", false);
    let db = TempDb::new("restore-dirty-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);
    std::fs::write(repo.work.join("untracked.txt"), "dirty").unwrap();

    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
    assert_eq!(read_record(&db, &repo).restored_at, None);

    std::fs::remove_file(repo.work.join("untracked.txt")).unwrap();
    output_git(&repo.work, &["checkout", "-q", "-b", "third-party"]);
    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), "third-party");
    assert_eq!(read_record(&db, &repo).restored_at, None);
}

#[test]
fn another_hosted_session_or_active_lease_blocks_restoration() {
    let repo = RealRepo::new("restore-occupancy", false);
    let db = TempDb::new("restore-occupancy-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let api = FakeOpenCodeApi::empty()
        .with_session(SESSION, &repo.work)
        .with_session("ses_peer", &repo.work);
    let outcome = restore(&repo, &db, &api, Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);

    crate::worktree::ensure_schema(&storage).unwrap();
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases \
             (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, \
              status, created_at, heartbeat_at) \
             VALUES ('lease-peer', ?1, 99, 'ses_peer', ?2, ?2, 'feature/99', 'active', 1, 1)",
            rusqlite::params![repo.identity(), repo.work.to_string_lossy()],
        )
        .unwrap();
    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
}

#[test]
fn sessionless_close_skips_restoration_with_a_warning() {
    let repo = RealRepo::new("restore-sessionless", false);
    let db = TempDb::new("restore-sessionless-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), None);
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
}

#[test]
fn local_ahead_preserves_branch_and_retries_a_failed_update() {
    let repo = RealRepo::new("restore-diverged", true);
    let db = TempDb::new("restore-diverged-db");
    let baseline_head = repo.commit_empty(&repo.work, "local ahead");
    let feature_head = prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let api = FakeOpenCodeApi::empty();
    let first = restore(&repo, &db, &api, Some(SESSION));
    assert_eq!(
        first,
        RestoreOutcome::Restored {
            branch: BASELINE_BRANCH.to_owned(),
            update: UpstreamOutcome::Failed(
                "local branch is ahead of or diverged from its upstream; nothing was overwritten"
                    .to_owned()
            ),
        }
    );
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    assert_eq!(repo.head(&repo.work), baseline_head);
    assert_eq!(read_record(&db, &repo).update_state, UpdateState::Failed);

    let second = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(second, RestoreOutcome::Restored { .. }));
    assert_eq!(repo.head(&repo.work), baseline_head);
    assert_eq!(
        output_git(&repo.work, &["rev-parse", ISSUE_BRANCH]),
        feature_head
    );
}

#[test]
fn failed_fetch_is_retried_by_the_next_close_without_recheckout() {
    let repo = RealRepo::new("restore-fetch-retry", true);
    let db = TempDb::new("restore-fetch-retry-db");
    let baseline_head = repo.head(&repo.work);
    let upstream_head = repo.advance_remote();
    let feature_head = prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);
    repo.set_remote_url("fixture", "/nonexistent/phasegent-remote.git");

    let api = FakeOpenCodeApi::empty();
    let first = restore(&repo, &db, &api, Some(SESSION));
    assert!(matches!(
        first,
        RestoreOutcome::Restored {
            update: UpstreamOutcome::Failed(_),
            ..
        }
    ));
    let restored_at = read_record(&db, &repo).restored_at;
    assert!(restored_at.is_some());
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    assert_eq!(repo.head(&repo.work), baseline_head);

    let bare_url = repo.bare.to_string_lossy().into_owned();
    repo.set_remote_url("fixture", &bare_url);
    let second = restore(&repo, &db, &api, Some(SESSION));
    assert_eq!(
        second,
        RestoreOutcome::Restored {
            branch: BASELINE_BRANCH.to_owned(),
            update: UpstreamOutcome::Updated,
        }
    );
    let record = read_record(&db, &repo);
    assert_eq!(record.update_state, UpdateState::Updated);
    assert_eq!(record.restored_at, restored_at);
    assert_eq!(repo.head(&repo.work), upstream_head);
    assert_eq!(
        output_git(&repo.work, &["rev-parse", ISSUE_BRANCH]),
        feature_head
    );
}

#[test]
fn pending_crash_recovery_infers_active_or_aborted_without_guessing() {
    let repo = RealRepo::new("restore-pending", false);
    let db = TempDb::new("restore-pending-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Pending, &baseline_head);

    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    let record = read_record(&db, &repo);
    assert_eq!(record.state, CheckoutState::Active);

    let repo = RealRepo::new("restore-pending-unchanged", false);
    let db = TempDb::new("restore-pending-unchanged-db");
    let baseline_head = repo.head(&repo.work);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Pending, &baseline_head);
    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert_eq!(outcome, RestoreOutcome::Noop);
    assert_eq!(read_record(&db, &repo).state, CheckoutState::Aborted);
}

#[test]
fn deleted_baseline_branch_is_not_recreated_from_a_remote_namesake() {
    let repo = RealRepo::new("restore-deleted-branch", true);
    let db = TempDb::new("restore-deleted-branch-db");
    let baseline_head = repo.head(&repo.work);
    let feature_head = prepare_feature(&repo);
    repo.update_ref("refs/remotes/fixture/main", &baseline_head);
    repo.delete_branch(BASELINE_BRANCH);
    assert!(!repo.local_branch_exists(BASELINE_BRANCH));
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert!(matches!(outcome, RestoreOutcome::Warning { .. }));
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
    assert!(!repo.local_branch_exists(BASELINE_BRANCH));
    assert_eq!(
        output_git(&repo.work, &["rev-parse", ISSUE_BRANCH]),
        feature_head
    );
}

#[test]
fn no_guess_switch_refuses_a_missing_branch_even_with_a_remote_namesake() {
    let repo = RealRepo::new("restore-no-guess", true);
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    repo.update_ref("refs/remotes/fixture/main", &baseline_head);
    repo.delete_branch(BASELINE_BRANCH);
    let error = super::git::checkout(&ProcessWorktreeRunner::new(), &repo.work, BASELINE_BRANCH)
        .expect_err("missing local branch must not be recreated");
    assert!(error.contains("switch"), "{error}");
    assert!(!repo.local_branch_exists(BASELINE_BRANCH));
}

#[test]
fn unresolvable_main_with_recorded_baseline_produces_a_warning() {
    let repo = RealRepo::new("restore-unavailable-main", false);
    let db = TempDb::new("restore-unavailable-main-db");
    let baseline_head = repo.head(&repo.work);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let warning = super::close_restore_unavailable_warning(
        &storage,
        &repo.work,
        Some("redmine"),
        Some("tools"),
        ISSUE,
        "no verified main checkout: test",
    )
    .expect("recorded baseline must warn");
    assert!(warning.contains("no verified main checkout"), "{warning}");
    assert!(
        super::close_restore_unavailable_warning(
            &storage,
            &repo.work,
            None,
            None,
            ISSUE,
            "no verified main checkout: test",
        )
        .is_none(),
        "an unknown scope must not search another scope"
    );

    let bare_root = TempDir::new("restore-unavailable-bare");
    let bare = bare_root.path().join("bare.git");
    let output = ProcessWorktreeRunner::new()
        .run(
            &["init", "--bare", "-q", &bare.to_string_lossy()],
            bare_root.path(),
        )
        .expect("bare init");
    assert_eq!(output.status, 0);
    let error = resolve_close_target(&ProcessWorktreeRunner::new(), &bare)
        .expect_err("bare repository has no verifiable main checkout");
    assert!(error.contains("main checkout"), "{error}");
}

#[test]
fn unavailable_main_warns_for_originless_linked_caller() {
    let repo = RealRepo::new("restore-local-warning", false);
    let db = TempDb::new("restore-local-warning-db");
    output_git(&repo.work, &["remote", "remove", "origin"]);
    let baseline_head = repo.head(&repo.work);
    let caller = repo.work.parent().unwrap().join("caller");
    repo.add_worktree(&caller, "caller-branch");
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    let scope = BaselineScope::new(
        &format!("local:{}", repo.work.display()),
        "redmine",
        "tools",
        ISSUE,
        &repo.work.to_string_lossy(),
    )
    .unwrap();
    insert_pending(
        &storage.connection,
        &NewBaseline {
            scope: &scope,
            issue_branch: ISSUE_BRANCH,
            baseline_branch: BASELINE_BRANCH,
            baseline_head: &baseline_head,
            clean_evidence: CLEAN_EVIDENCE,
            default_branch: None,
            recorded_at: 1,
        },
    )
    .unwrap();
    assert!(
        super::close_restore_unavailable_warning(
            &storage,
            &caller,
            Some("redmine"),
            Some("tools"),
            ISSUE,
            "no verified main checkout",
        )
        .is_some(),
        "recorded originless baseline must produce a warning"
    );
}

#[test]
fn prepared_main_is_usable_after_the_caller_worktree_is_removed() {
    let repo = RealRepo::new("restore-removed-caller", false);
    let db = TempDb::new("restore-removed-caller-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let caller = repo.work.parent().unwrap().join("caller");
    repo.add_worktree(&caller, "caller-branch");
    let worktree = ProcessWorktreeRunner::new();
    let target = resolve_close_target(&worktree, &caller).expect("main before removal");
    repo.remove_worktree(&caller);
    assert!(!caller.exists());

    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);
    let git = ProcessGitRunner::in_directory(&target.main_checkout);
    let outcome = restore_on_close(&CloseRestoreParams {
        storage: &storage,
        git: &git,
        worktree: &worktree,
        api: &FakeOpenCodeApi::empty(),
        target: &target,
        provider: Some("redmine"),
        project: Some("tools"),
        issue_number: ISSUE,
        session: Some(SESSION),
    });
    assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    assert_eq!(repo.branch(&repo.work), BASELINE_BRANCH);
    assert_eq!(repo.head(&repo.work), baseline_head);
}

#[test]
fn unknown_scope_is_a_quiet_noop_and_never_searches_another_scope() {
    let repo = RealRepo::new("restore-scope", false);
    let db = TempDb::new("restore-scope-db");
    let baseline_head = repo.head(&repo.work);
    prepare_feature(&repo);
    let storage = db.open();
    insert_baseline(&storage, &repo, CheckoutState::Active, &baseline_head);

    let outcome = restore_with_scope(
        &repo,
        &db,
        &FakeOpenCodeApi::empty(),
        Some(SESSION),
        None,
        None,
    );
    assert_eq!(outcome, RestoreOutcome::Noop);
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
}

#[test]
fn missing_baseline_is_a_silent_noop() {
    let repo = RealRepo::new("restore-missing", false);
    let db = TempDb::new("restore-missing-db");
    prepare_feature(&repo);
    let outcome = restore(&repo, &db, &FakeOpenCodeApi::empty(), Some(SESSION));
    assert_eq!(outcome, RestoreOutcome::Noop);
    assert_eq!(repo.branch(&repo.work), ISSUE_BRANCH);
}

fn output_git(cwd: &Path, args: &[&str]) -> String {
    let output = ProcessWorktreeRunner::new()
        .run(args, cwd)
        .expect("git invocation");
    assert_eq!(output.status, 0, "git {args:?} failed: {}", output.stdout);
    output.stdout.trim().to_owned()
}

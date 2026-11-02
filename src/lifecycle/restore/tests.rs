//! Restore-module evidence: additive schema, first-baseline preservation,
//! scope isolation, state transitions, and the real auto-switch order.

use std::cell::RefCell;
use std::rc::Rc;

use crate::branch_links::{IssueKey, LinkParams, ensure_schema as ensure_link_schema, link};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::lifecycle::{CreateSwitchOutcome, IssueSwitchParams, create_link_and_switch};
use crate::worktree::now_unix_secs;

use super::store::*;
use super::test_support::{SwitchState, TempDb, fake_switch};

fn scope(repo_key: &str, project: &str, issue: u64, path: &str) -> BaselineScope {
    BaselineScope::new(repo_key, "redmine", project, issue, path).expect("scope")
}

fn new_baseline<'a>(
    scope: &'a BaselineScope,
    issue_branch: &'a str,
    baseline_branch: &'a str,
    baseline_head: &'a str,
    recorded_at: i64,
) -> NewBaseline<'a> {
    NewBaseline {
        scope,
        issue_branch,
        baseline_branch,
        baseline_head,
        clean_evidence: CLEAN_EVIDENCE,
        default_branch: Some("main"),
        recorded_at,
    }
}

#[test]
fn schema_is_additive_and_scope_isolates_same_issue_number() {
    let db = TempDb::new("schema-scope");
    let storage = db.open();
    ensure_link_schema(&storage.connection).expect("legacy link schema");
    let issue = IssueKey::from_number("redmine", "tools", 1175).expect("issue key");
    link(
        &storage.connection,
        &LinkParams {
            repo_key: "github.com/owner/repo",
            branch: "issue/1175",
            issue: &issue,
            issue_number: 1175,
            source: "test",
            now: 1,
        },
    )
    .expect("legacy link row");
    ensure_schema(&storage.connection).expect("restore schema");

    let first = scope("github.com/owner/repo", "tools", 1175, "/checkout/one");
    let second = scope("github.com/owner/repo", "other", 1175, "/checkout/one");
    let third = scope("github.com/owner/repo", "tools", 1175, "/checkout/two");
    for (target, branch, head) in [
        (&first, "issue/1175", "aaa"),
        (&second, "issue/1175", "bbb"),
        (&third, "issue/1175", "ccc"),
    ] {
        insert_pending(
            &storage.connection,
            &new_baseline(target, branch, "main", head, 1),
        )
        .expect("insert");
    }

    assert_eq!(
        read_baseline(&storage.connection, &first)
            .unwrap()
            .unwrap()
            .baseline_head,
        "aaa"
    );
    assert_eq!(
        read_baseline(&storage.connection, &second)
            .unwrap()
            .unwrap()
            .baseline_head,
        "bbb"
    );
    assert_eq!(
        read_baseline(&storage.connection, &third)
            .unwrap()
            .unwrap()
            .baseline_head,
        "ccc"
    );
    let legacy: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM branch_issue_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(legacy, 1, "additive restore schema keeps legacy links");
}

#[test]
fn first_valid_baseline_is_never_overwritten() {
    let db = TempDb::new("first-baseline");
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    let target = scope("github.com/owner/repo", "tools", 1175, "/checkout/one");
    insert_pending(
        &storage.connection,
        &new_baseline(&target, "issue/1175", "dev", "aaa", 10),
    )
    .unwrap();
    let retried = insert_pending(
        &storage.connection,
        &new_baseline(&target, "issue/1175", "dev", "aaa", 10),
    )
    .unwrap();
    assert!(matches!(retried, InsertOutcome::Preserved(_)));
    let outcome = insert_pending(
        &storage.connection,
        &new_baseline(&target, "issue/1175", "main", "bbb", 20),
    )
    .unwrap();
    assert!(matches!(outcome, InsertOutcome::Preserved(_)));
    let row = read_baseline(&storage.connection, &target)
        .unwrap()
        .unwrap();
    assert_eq!(row.baseline_branch, "dev");
    assert_eq!(row.baseline_head, "aaa");
    assert_eq!(row.recorded_at, 10);
    assert_eq!(row.state, CheckoutState::Pending);
    assert_eq!(row.update_state, UpdateState::None);
    assert_eq!(row.restored_at, None);
}

#[test]
fn checkout_state_transitions_only_from_pending() {
    let db = TempDb::new("state-transition");
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    let first = scope("github.com/owner/repo", "tools", 1, "/checkout/one");
    insert_pending(
        &storage.connection,
        &new_baseline(&first, "issue/1", "main", "aaa", 1),
    )
    .unwrap();

    assert!(transition_pending(&storage.connection, &first, CheckoutState::Active).unwrap());
    assert!(!transition_pending(&storage.connection, &first, CheckoutState::Aborted).unwrap());
    assert_eq!(
        read_baseline(&storage.connection, &first)
            .unwrap()
            .unwrap()
            .state,
        CheckoutState::Active
    );

    let second = scope("github.com/owner/repo", "tools", 2, "/checkout/one");
    insert_pending(
        &storage.connection,
        &new_baseline(&second, "issue/2", "main", "bbb", 2),
    )
    .unwrap();
    assert!(transition_pending(&storage.connection, &second, CheckoutState::Aborted).unwrap());
    assert!(!transition_pending(&storage.connection, &second, CheckoutState::Active).unwrap());
    assert_eq!(
        read_baseline(&storage.connection, &second)
            .unwrap()
            .unwrap()
            .state,
        CheckoutState::Aborted
    );
}

#[test]
fn restoration_and_update_state_are_tracked_independently() {
    let db = TempDb::new("restore-state");
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    let target = scope("github.com/owner/repo", "tools", 1, "/checkout/one");
    insert_pending(
        &storage.connection,
        &new_baseline(&target, "issue/1", "main", "aaa", 1),
    )
    .unwrap();
    assert!(mark_restored(&storage.connection, &target, UpdateState::Pending, 20,).unwrap());
    assert!(set_update_state(&storage.connection, &target, UpdateState::Failed,).unwrap());
    let first = read_baseline(&storage.connection, &target)
        .unwrap()
        .unwrap();
    assert_eq!(first.restored_at, Some(20));
    assert_eq!(first.update_state, UpdateState::Failed);
    assert!(mark_restored(&storage.connection, &target, UpdateState::Updated, 30,).unwrap());
    let second = read_baseline(&storage.connection, &target)
        .unwrap()
        .unwrap();
    assert_eq!(second.restored_at, Some(20), "the first timestamp is kept");
    assert_eq!(second.update_state, UpdateState::Updated);
}

fn configured_state(db: &TempDb, branch: &str, head: &str) -> (SwitchState, BaselineScope) {
    let repo_path = db.dir.path();
    let repo_key =
        crate::branch_links::resolve_repo_key(Some("git@github.com:owner/repo.git"), repo_path)
            .expect("repo key")
            .key;
    let checkout_path = std::fs::canonicalize(repo_path)
        .expect("canonical repo")
        .to_string_lossy()
        .into_owned();
    let target =
        BaselineScope::new(&repo_key, "redmine", "tools", 1175, &checkout_path).expect("scope");
    let storage = db.open();
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    let mut state = SwitchState::new(repo_path, branch, head);
    state.db_path = Some(db.path.clone());
    state.scope = Some(target.clone());
    (state, target)
}

fn switch_once(
    _db: &TempDb,
    state: SwitchState,
) -> (CreateSwitchOutcome, Rc<RefCell<SwitchState>>) {
    let (git, worktree, shared) = fake_switch(state);
    let repo_path = shared.borrow().repo_path.clone();
    let outcome = create_link_and_switch(
        &git,
        &worktree,
        &IssueSwitchParams {
            repo_path: &repo_path,
            issue_id: 1175,
            branch: "issue/1175",
            base: None,
            scope_provider: "redmine",
            scope_project: Some("tools"),
            explicit_repository: None,
            session: None,
        },
    );
    (outcome, shared)
}

#[test]
fn automatic_switch_records_pending_before_checkout_and_active_after() {
    let _lock = lock_workflow_tests();
    let db = TempDb::new("auto-switch");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", &db.path_text());
    let (state, target) = configured_state(&db, "main", "abc123");
    let (outcome, shared) = switch_once(&db, state);

    assert_eq!(
        outcome,
        CreateSwitchOutcome::Switched {
            branch: "issue/1175".to_owned(),
            issue_id: 1175,
        }
    );
    assert_eq!(
        shared.borrow().checkout_observed_pending,
        Some(true),
        "the pending baseline must exist before git checkout runs"
    );
    let storage = db.open();
    let row = read_baseline(&storage.connection, &target)
        .unwrap()
        .expect("baseline row");
    assert_eq!(row.issue_branch, "issue/1175");
    assert_eq!(row.baseline_branch, "main");
    assert_eq!(row.baseline_head, "abc123");
    assert_eq!(row.clean_evidence, CLEAN_EVIDENCE);
    assert_eq!(row.default_branch.as_deref(), Some("main"));
    assert!(row.recorded_at > 0 && row.recorded_at <= now_unix_secs());
    assert_eq!(row.state, CheckoutState::Active);
    assert_eq!(row.update_state, UpdateState::None);
}

#[test]
fn dirty_and_already_on_issue_cases_do_not_capture() {
    let _lock = lock_workflow_tests();
    let db = TempDb::new("skip-capture");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", &db.path_text());
    let (mut dirty_state, dirty_scope) = configured_state(&db, "main", "abc123");
    dirty_state.clean = false;
    let (outcome, _) = switch_once(&db, dirty_state);
    assert!(matches!(outcome, CreateSwitchOutcome::LinkedOnly { .. }));
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    assert!(
        read_baseline(&storage.connection, &dirty_scope)
            .unwrap()
            .is_none()
    );

    let (same_branch_state, same_scope) = configured_state(&db, "issue/1175", "abc123");
    let (outcome, _) = switch_once(&db, same_branch_state);
    assert!(matches!(outcome, CreateSwitchOutcome::Switched { .. }));
    assert!(
        read_baseline(&storage.connection, &same_scope)
            .unwrap()
            .is_none()
    );
}

#[test]
fn linked_worktree_switch_proceeds_without_a_baseline() {
    let _lock = lock_workflow_tests();
    let db = TempDb::new("linked-worktree");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", &db.path_text());
    let (mut state, target) = configured_state(&db, "main", "abc123");
    state.git_dir = format!("{}/.git/worktrees/linked", state.common_dir);
    let (outcome, _) = switch_once(&db, state);
    assert!(matches!(outcome, CreateSwitchOutcome::Switched { .. }));
    let storage = db.open();
    ensure_schema(&storage.connection).unwrap();
    assert!(
        read_baseline(&storage.connection, &target)
            .unwrap()
            .is_none()
    );
}

#[test]
fn failed_checkout_with_unchanged_baseline_is_aborted() {
    let _lock = lock_workflow_tests();
    let db = TempDb::new("failed-checkout");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", &db.path_text());
    let (mut state, target) = configured_state(&db, "main", "abc123");
    state.checkout_status = 1;
    let (outcome, _) = switch_once(&db, state);
    assert!(matches!(outcome, CreateSwitchOutcome::Warning { .. }));
    let storage = db.open();
    let row = read_baseline(&storage.connection, &target)
        .unwrap()
        .expect("baseline row");
    assert_eq!(row.state, CheckoutState::Aborted);
}

#[test]
fn ambiguous_checkout_result_remains_pending() {
    let _lock = lock_workflow_tests();
    let db = TempDb::new("ambiguous-checkout");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", &db.path_text());
    let (mut state, target) = configured_state(&db, "main", "abc123");
    state.checkout_status = 1;
    state.after_checkout_branch = Some("feature/other".to_owned());
    let (outcome, _) = switch_once(&db, state);
    assert!(matches!(outcome, CreateSwitchOutcome::Warning { .. }));
    let storage = db.open();
    let row = read_baseline(&storage.connection, &target)
        .unwrap()
        .expect("baseline row");
    assert_eq!(row.state, CheckoutState::Pending);
}

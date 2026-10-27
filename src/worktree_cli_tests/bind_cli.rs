use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn auto_acquire_after_create_reuses_current_checkout_silently() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-create-reuse") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-create-reuse");
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("chdir temp repo");
    let warning = auto_acquire_after_create(310, Some("session-a"));
    let _ = std::env::set_current_dir(&previous_cwd);

    assert_eq!(
        warning, None,
        "a clean repo with no competing lease must reuse the checkout silently"
    );
    let storage = Storage::open().expect("storage");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let rows = list_for_repo(&storage, &identity).expect("list");
    assert_eq!(rows.len(), 1, "the reuse path records one active lease");
    assert_eq!(rows[0].session, "session-a");
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "reuse must never delete the checkout"
    );
}

#[test]
fn auto_acquire_after_create_warns_with_isolation_guidance_instead_of_creating() {
    // Issue 616 + 651 P3: the implicit create hook states the reuse
    // preference, so it never creates a worktree on its own. An
    // occupied checkout path surfaces the explicit isolation command
    // instead, and the create stays successful.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-create-opt-in") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-create-opt-in");
    // Seed an occupant on the checkout path itself so the hook refuses
    // creation. A lease on any other path would reuse instead.
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("schema");
    let mut occupant = fresh_lease_row(
        "lease-occupant",
        999,
        "other-session",
        &repo.dir.path().to_string_lossy(),
        "main",
        LEASE_STATUS_ACTIVE,
        now_unix_secs(),
    );
    occupant.repo_identity = identity.clone();
    insert_row(&storage, &occupant);
    drop(storage);

    let warning = in_temp_repo(&repo, || auto_acquire_after_create(311, Some("session-b")));
    let warning = warning.expect("an occupied checkout must surface the isolation guidance");
    assert!(
        warning.contains("isolation is opt-in")
            && warning.contains("--isolate")
            && warning.contains("issue 311")
            && warning.contains("other-session"),
        "unexpected warning: {warning}"
    );
    let storage = Storage::open().expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list");
    assert_eq!(
        rows.len(),
        1,
        "only the seeded occupant may exist; the refusal must not book the checkout: {rows:?}"
    );
    assert_eq!(rows[0].session, "other-session");
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "the implicit hook must not create a worktree directory"
    );
    assert!(
        repo.dir.path().exists(),
        "the refusal must never delete the current checkout"
    );
}

#[test]
fn auto_acquire_after_create_ignores_worktree_auto_and_reuses() {
    // Issue 651 P3: the removed `worktree-auto` setting is inert. With
    // it set, the same free checkout still reuses silently instead of
    // creating a worktree — the hook states the reuse preference and
    // never creates.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-create-auto-on") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-create-auto-on");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("schema");
    storage
        .save_global_setting("PHASEGENT_WORKTREE_AUTO", "true")
        .expect("leftover row must persist");
    // A foreign lease on another path never blocks the free primary.
    let mut foreign = fresh_lease_row(
        "lease-foreign",
        999,
        "other-session",
        "/tmp/phasegent-foreign",
        "main",
        LEASE_STATUS_ACTIVE,
        now_unix_secs(),
    );
    foreign.repo_identity = identity.clone();
    insert_row(&storage, &foreign);
    drop(storage);

    let warning = in_temp_repo(&repo, || auto_acquire_after_create(311, Some("session-b")));
    assert_eq!(
        warning, None,
        "a free checkout reuses silently despite the leftover setting: {warning:?}"
    );
    let rows = {
        let storage = Storage::open().expect("storage");
        list_for_repo(&storage, &identity).expect("list")
    };
    let booked = rows
        .iter()
        .find(|row| row.session == "session-b")
        .expect("reuse lease");
    assert_eq!(
        booked.worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "the hook books the current checkout, never a worktree"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "the implicit hook must not create a worktree directory"
    );
    assert!(
        repo.dir.path().exists(),
        "reuse must never delete the current checkout"
    );
}

#[test]
fn auto_acquire_after_create_is_silent_for_zero_issue_and_unknown_session() {
    let _lock = lock_workflow_tests();
    // `issue == 0` short-circuits before any session/storage work.
    assert_eq!(auto_acquire_after_create(0, Some("session-a")), None);
    // A blank explicit session is rejected by `resolve_session` and the
    // helper degrades to silence rather than failing the caller.
    assert_eq!(auto_acquire_after_create(312, Some("   ")), None);
}

// ---------------------------------------------------------------------------
// `issue bind` / `issue unbind`: durable links, role gates, no lease writes
// ---------------------------------------------------------------------------

fn scoped_env(label: &str) -> (TempDir, EnvGuard) {
    let temp = TempDir::new(&format!("{label}-db"));
    let db = temp.path().join("phasegent.sqlite3");
    let guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    (temp, guard)
}

fn repo_key(repo: &TempRepo) -> String {
    crate::branch_links::resolve_repo_key(None, repo.dir.path())
        .expect("fallback key")
        .key
}

/// Non-default branch the bind tests check out: the detected/conventional
/// default (`main`) is refused by the bind guards.
const BIND_BRANCH: &str = "feat/628";

fn checkout_bind_branch(repo: &TempRepo) {
    let runner = ProcessWorktreeRunner::new();
    let output = runner
        .run(&["checkout", "-q", "-B", BIND_BRANCH], repo.dir.path())
        .expect("checkout works");
    assert_eq!(output.status, 0);
}

fn linked_rows(
    db: &std::path::Path,
    repo_key: &str,
    branch: &str,
) -> Vec<crate::branch_links::LinkedIssue> {
    let storage = Storage::open_at(db).expect("temp storage must open");
    crate::branch_links::ensure_schema(&storage.connection).expect("schema");
    crate::branch_links::issues_for_branch(
        &storage.connection,
        repo_key,
        branch,
        true,
        &crate::branch_links::UnknownState,
    )
    .expect("read must work")
}

fn run_bind(role: Option<Role>, issue: u64) -> i32 {
    crate::cli::branch::execute_branch_context_scoped(
        role,
        Some(crate::providers::ProviderKind::Redmine),
        None,
        Some("tools-phasegent"),
        bind_issue_command(issue),
    )
}

fn run_unbind(role: Option<Role>) -> i32 {
    crate::cli::branch::execute_branch_context_scoped(
        role,
        Some(crate::providers::ProviderKind::Redmine),
        None,
        Some("tools-phasegent"),
        IssueCommand::Unbind { issue_ids: vec![] },
    )
}

fn run_unbind_ids(role: Option<Role>, issue_ids: &[u64]) -> i32 {
    crate::cli::branch::execute_branch_context_scoped(
        role,
        Some(crate::providers::ProviderKind::Redmine),
        None,
        Some("tools-phasegent"),
        IssueCommand::Unbind {
            issue_ids: issue_ids.to_vec(),
        },
    )
}

fn run_bind_ids(role: Option<Role>, issue_ids: &[u64]) -> i32 {
    crate::cli::branch::execute_branch_context_scoped(
        role,
        Some(crate::providers::ProviderKind::Redmine),
        None,
        Some("tools-phasegent"),
        IssueCommand::Bind {
            issue_ids: issue_ids.to_vec(),
        },
    )
}

#[test]
fn cli_bind_links_durably_and_never_runs_the_auto_acquire_hook() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p616-bind-refused") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p616-bind-refused");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    let exit = in_temp_repo(&repo, || run_bind(Some(Role::Orchestrator), 616));
    assert_eq!(exit, 0, "the scoped bind must succeed");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows.len(), 1, "exactly one durable link: {rows:?}");
    assert_eq!(rows[0].issue_number, 616);
    assert_eq!(rows[0].status, "linked");

    let storage = Storage::open_at(&db_path).expect("storage");
    ensure_schema(&storage).expect("lease schema");
    let leases = list_for_repo(&storage, &identity).expect("list");
    assert!(
        leases.is_empty(),
        "bind never runs the auto-acquire hook: {leases:?}"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "bind must not create a worktree directory"
    );
}

#[test]
fn cli_bind_with_executor_role_is_denied_before_any_write() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-denied") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p3-bind-denied");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    let exit = in_temp_repo(&repo, || run_bind(Some(Role::Executor), 541));
    assert_eq!(
        exit, 3,
        "an explicit non-orchestrator role must be denied `issue bind`"
    );
    assert!(
        linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH).is_empty(),
        "the denial must land before any link write"
    );
}

#[test]
fn cli_unbind_with_executor_role_is_denied_and_keeps_the_link() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-unbind-denied") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p3-unbind-denied");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);
    assert_eq!(
        in_temp_repo(&repo, || run_bind(Some(Role::Orchestrator), 542)),
        0
    );

    let exit = in_temp_repo(&repo, || run_unbind(Some(Role::Executor)));
    assert_eq!(
        exit, 3,
        "an explicit non-orchestrator role must be denied `issue unbind`"
    );
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows.len(), 1, "the denial must not detach the link");
    assert_eq!(rows[0].status, "linked");
}

#[test]
fn cli_bind_without_role_and_with_orchestrator_role_still_work() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-allowed") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p3-bind-allowed");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    let exit = in_temp_repo(&repo, || run_bind(None, 541));
    assert_eq!(exit, 0, "a role-less bind keeps the historical passthrough");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows[0].issue_number, 541);

    let exit = in_temp_repo(&repo, || run_unbind(Some(Role::Orchestrator)));
    assert_eq!(exit, 0, "orchestrator may unbind");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows[0].status, "detached");
}

#[test]
fn cli_status_branch_passes_the_executor_role_gate() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-status-branch") else {
        return;
    };
    let (_db_temp, _db_env) = scoped_env("p3-status-branch");
    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Executor),
            None,
            None,
            None,
            IssueCommand::StatusBranch,
        )
    });
    assert_eq!(exit, 0, "read-only status-branch is unrestricted");
}

#[test]
fn cli_bind_links_a_batch_and_unbind_detaches_only_named_ids() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p117-batch-bind") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p117-batch-bind");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    let exit = in_temp_repo(&repo, || {
        run_bind_ids(Some(Role::Orchestrator), &[628, 629, 630])
    });
    assert_eq!(exit, 0, "the multi-id bind must succeed");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows.len(), 3, "every id lands in one batch: {rows:?}");
    assert!(rows.iter().all(|row| row.status == "linked"));

    let exit = in_temp_repo(&repo, || run_unbind_ids(Some(Role::Orchestrator), &[629]));
    assert_eq!(exit, 0, "the named unbind must succeed");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    let detached: Vec<u64> = rows
        .iter()
        .filter(|row| row.status == "detached")
        .map(|row| row.issue_number)
        .collect();
    assert_eq!(detached, vec![629], "only the named link is detached");
    assert_eq!(
        rows.iter().filter(|row| row.status == "linked").count(),
        2,
        "the other links stay active: {rows:?}"
    );

    let exit = in_temp_repo(&repo, || run_unbind(Some(Role::Orchestrator)));
    assert_eq!(exit, 0, "an empty id list keeps the detach-all behavior");
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert!(
        rows.iter().all(|row| row.status == "detached"),
        "detach-all retires the remaining links: {rows:?}"
    );
}

#[test]
fn cli_bind_batch_is_idempotent_and_ignores_duplicate_ids() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p117-batch-idempotent") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p117-batch-idempotent");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    assert_eq!(
        in_temp_repo(&repo, || {
            run_bind_ids(Some(Role::Orchestrator), &[701, 701, 702])
        }),
        0
    );
    assert_eq!(
        in_temp_repo(&repo, || {
            run_bind_ids(Some(Role::Orchestrator), &[702, 701])
        }),
        0
    );
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(
        rows.len(),
        2,
        "a repeated bind must not duplicate: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.status == "linked"),
        "repeated ids stay linked: {rows:?}"
    );
}

#[test]
fn cli_bind_is_idempotent_for_the_same_issue() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-repeat") else {
        return;
    };
    let (db_temp, _db_env) = scoped_env("p3-bind-repeat");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    checkout_bind_branch(&repo);

    assert_eq!(
        in_temp_repo(&repo, || run_bind(Some(Role::Orchestrator), 245)),
        0
    );
    assert_eq!(
        in_temp_repo(&repo, || run_bind(Some(Role::Orchestrator), 245)),
        0
    );
    let rows = linked_rows(&db_path, &repo_key(&repo), BIND_BRANCH);
    assert_eq!(rows.len(), 1, "a repeat bind must not duplicate: {rows:?}");
    assert_eq!(rows[0].status, "linked");
}

#[test]
fn batch_link_validates_every_parameter_before_any_write() {
    let _lock = lock_workflow_tests();
    let (db_temp, _db_env) = scoped_env("p117-batch-atomic");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    let storage = Storage::open_at(&db_path).expect("storage");
    crate::branch_links::ensure_schema(&storage.connection).expect("schema");
    let first = crate::branch_links::IssueKey::from_number("local", "default", 1).expect("key");
    let second = crate::branch_links::IssueKey::from_number("local", "default", 2).expect("key");
    let repo_key = "local:/tmp/phasegent-batch-atomic";
    let params = [
        crate::branch_links::LinkParams {
            repo_key,
            branch: BIND_BRANCH,
            issue: &first,
            issue_number: 1,
            source: "test",
            now: 1,
        },
        // An empty branch fails validation; the whole batch must land nothing.
        crate::branch_links::LinkParams {
            repo_key,
            branch: "",
            issue: &second,
            issue_number: 2,
            source: "test",
            now: 1,
        },
    ];
    assert!(
        crate::branch_links::store::link_many(&storage.connection, &params).is_err(),
        "an invalid member fails the batch"
    );
    let rows = crate::branch_links::issues_for_branch(
        &storage.connection,
        repo_key,
        BIND_BRANCH,
        true,
        &crate::branch_links::UnknownState,
    )
    .expect("read");
    assert!(
        rows.is_empty(),
        "the valid member must not be written when the batch fails: {rows:?}"
    );
}

#[test]
fn batch_detach_validates_every_member_before_any_write() {
    let _lock = lock_workflow_tests();
    let (db_temp, _db_env) = scoped_env("p117-batch-detach-atomic");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    let storage = Storage::open_at(&db_path).expect("storage");
    crate::branch_links::ensure_schema(&storage.connection).expect("schema");
    let first = crate::branch_links::IssueKey::from_number("local", "default", 1).expect("key");
    let second = crate::branch_links::IssueKey::from_number("local", "default", 2).expect("key");
    let repo_key = "local:/tmp/phasegent-batch-detach-atomic";
    let linked = [
        crate::branch_links::LinkParams {
            repo_key,
            branch: BIND_BRANCH,
            issue: &first,
            issue_number: 1,
            source: "test",
            now: 1,
        },
        crate::branch_links::LinkParams {
            repo_key,
            branch: BIND_BRANCH,
            issue: &second,
            issue_number: 2,
            source: "test",
            now: 1,
        },
    ];
    crate::branch_links::store::link_many(&storage.connection, &linked).expect("seed links");

    // An invalid member fails the whole batch before the transaction opens.
    let targets = [
        (first.clone(), 1u64),
        (
            crate::branch_links::IssueKey::from_number("local", "default", 2).expect("key"),
            0,
        ),
    ];
    assert!(
        crate::branch_links::store::detach_many(
            &storage.connection,
            repo_key,
            BIND_BRANCH,
            &targets,
            "test",
            2,
        )
        .is_err(),
        "an invalid member fails the batch"
    );
    let rows = crate::branch_links::issues_for_branch(
        &storage.connection,
        repo_key,
        BIND_BRANCH,
        true,
        &crate::branch_links::UnknownState,
    )
    .expect("read");
    assert_eq!(rows.len(), 2, "both seeded links remain: {rows:?}");
    assert!(
        rows.iter().all(|row| row.status == "linked"),
        "the valid member must not be detached when the batch fails: {rows:?}"
    );
}

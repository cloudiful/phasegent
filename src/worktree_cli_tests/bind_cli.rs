use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn auto_acquire_after_bind_reuses_current_checkout_silently() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-bind-reuse") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-bind-reuse");
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("chdir temp repo");
    let warning = auto_acquire_after_bind(310, Some("session-a"));
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
fn auto_acquire_after_bind_warns_with_isolation_guidance_instead_of_creating() {
    // Issue 616: the implicit create/bind hook never creates a worktree on
    // its own. A conflict surfaces the explicit isolation command instead,
    // and the create/bind stays successful.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-bind-opt-in") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-bind-opt-in");
    // Seed a foreign active lease for the repo so rule 4 refuses creation.
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("schema");
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

    let warning = in_temp_repo(&repo, || auto_acquire_after_bind(311, Some("session-b")));
    let warning = warning.expect("a refused conflict must surface the isolation guidance");
    assert!(
        warning.contains("isolation is opt-in")
            && warning.contains("--isolate")
            && warning.contains("issue 311")
            && warning.contains("another active lease exists for this repository"),
        "unexpected warning: {warning}"
    );
    let storage = Storage::open().expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list");
    assert_eq!(
        rows.len(),
        1,
        "only the seeded foreign lease may exist; the refusal must not book the checkout: {rows:?}"
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
fn auto_acquire_after_bind_creates_when_worktree_auto_is_enabled() {
    // `worktree-auto` remains the global opt-in: with it on, the same
    // conflict creates the dedicated worktree and reports the redirect.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-bind-auto-on") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("auto-bind-auto-on");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("schema");
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

    let warning = in_temp_repo(&repo, || auto_acquire_after_bind(311, Some("session-b")));
    let warning = warning.expect("a created worktree must surface a bounded warning");
    assert!(
        warning.contains("phasegent: acquired worktree")
            && warning.contains("issue 311")
            && warning.contains("reason=new_worktree"),
        "unexpected warning: {warning}"
    );
    let rows = {
        let storage = Storage::open().expect("storage");
        list_for_repo(&storage, &identity).expect("list")
    };
    let created = rows
        .iter()
        .find(|row| row.session == "session-b")
        .expect("created lease");
    assert!(
        created
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "the opted-in worktree must land under the temp cache"
    );
    assert!(
        std::path::Path::new(&created.worktree_path).exists(),
        "the helper must create the worktree directory on disk"
    );
    assert!(
        repo.dir.path().exists(),
        "isolation must never delete the current checkout"
    );
}

#[test]
fn auto_acquire_after_bind_is_silent_for_zero_issue_and_unknown_session() {
    let _lock = lock_workflow_tests();
    // `issue == 0` short-circuits before any session/storage work.
    assert_eq!(auto_acquire_after_bind(0, Some("session-a")), None);
    // A blank explicit session is rejected by `resolve_session` and the
    // helper degrades to silence rather than failing the caller.
    assert_eq!(auto_acquire_after_bind(312, Some("   ")), None);
}

#[test]
fn cli_bind_stays_successful_when_implicit_isolation_is_unavailable() {
    // Issue 616 constraint: a rejected isolation is a stderr warning, never a
    // bind failure. The binding is written, the process exits 0, and no
    // worktree or lease is created by the implicit hook.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p616-bind-refused") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p616-bind-refused");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("schema");
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

    let exit = in_temp_repo(&repo, || {
        execute_branch_context(
            Some(Role::Orchestrator),
            bind_issue_command(616, Some("session-B")),
        )
    });
    assert_eq!(
        exit, 0,
        "the refused implicit isolation must not fail the bind"
    );
    assert_eq!(
        read_branch_binding(&repo).as_deref(),
        Some("616"),
        "the binding must be written before the hook runs"
    );
    let storage = Storage::open().expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list");
    assert_eq!(
        rows.len(),
        1,
        "the refused hook must not book the checkout: {rows:?}"
    );
    assert_eq!(rows[0].issue, 999);
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "the refused hook must not create a worktree directory"
    );
}

// ---------------------------------------------------------------------------
// Issue #541 Phase 3: `issue bind` role gate and repeat-bind skip, end to end
// ---------------------------------------------------------------------------
//
// The role gate lives in `cli::branch::execute_branch_context`, which resolves
// its checkout through the process cwd, so these tests chdir into a temp repo
// under `lock_workflow_tests` exactly like the CLI-surface acquire tests above.
// The temp DB and cache are pinned so an allowed bind never touches the
// operator's database or `~/.cache`.

#[test]
fn cli_bind_with_executor_role_is_denied_before_any_git_write() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-denied") else {
        return;
    };
    let exit = in_temp_repo(&repo, || {
        execute_branch_context(Some(Role::Executor), bind_issue_command(541, None))
    });
    assert_eq!(
        exit, 3,
        "an explicit non-orchestrator role must be denied `issue bind`"
    );
    assert_eq!(
        read_branch_binding(&repo),
        None,
        "the denial must land before any Git config write"
    );
}

#[test]
fn cli_unbind_with_executor_role_is_denied_and_keeps_the_binding() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-unbind-denied") else {
        return;
    };
    bind_current_branch(&repo, 542);
    let exit = in_temp_repo(&repo, || {
        execute_branch_context(Some(Role::Executor), IssueCommand::Unbind)
    });
    assert_eq!(
        exit, 3,
        "an explicit non-orchestrator role must be denied `issue unbind`"
    );
    assert_eq!(
        read_branch_binding(&repo).as_deref(),
        Some("542"),
        "the denial must not unset the branch binding"
    );
}

#[test]
fn cli_bind_without_role_and_with_orchestrator_role_still_work() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-allowed") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p3-bind-allowed");

    let exit = in_temp_repo(&repo, || {
        execute_branch_context(None, bind_issue_command(541, Some("session-A")))
    });
    assert_eq!(exit, 0, "a role-less bind keeps the historical passthrough");
    assert_eq!(read_branch_binding(&repo).as_deref(), Some("541"));

    let exit = in_temp_repo(&repo, || {
        execute_branch_context(Some(Role::Orchestrator), IssueCommand::Unbind)
    });
    assert_eq!(exit, 0, "orchestrator may unbind");
    assert_eq!(read_branch_binding(&repo), None);

    let exit = in_temp_repo(&repo, || {
        execute_branch_context(
            Some(Role::Orchestrator),
            bind_issue_command(542, Some("session-A")),
        )
    });
    assert_eq!(exit, 0, "orchestrator may bind");
    assert_eq!(read_branch_binding(&repo).as_deref(), Some("542"));
}

#[test]
fn cli_status_branch_passes_the_executor_role_gate() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-status-branch") else {
        return;
    };
    bind_current_branch(&repo, 541);
    let exit = in_temp_repo(&repo, || {
        execute_branch_context(Some(Role::Executor), IssueCommand::StatusBranch)
    });
    assert_eq!(exit, 0, "read-only status-branch is unrestricted");
}

#[test]
fn cli_repeated_bind_skips_auto_acquire_and_keeps_the_lease_count() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-bind-repeat") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p3-bind-repeat");
    let db_path = db_temp.path().join("phasegent.sqlite3");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    // The branch is already bound, so `issue bind` returns `already_bound`.
    bind_current_branch(&repo, 245);

    let exit = in_temp_repo(&repo, || {
        execute_branch_context(
            Some(Role::Orchestrator),
            bind_issue_command(245, Some("session-A")),
        )
    });
    assert_eq!(exit, 0, "a repeated bind is still a successful no-op");

    {
        let storage = Storage::open_at(&db_path).expect("storage");
        ensure_schema(&storage).expect("ensure_schema");
        let rows = list_for_repo(&storage, &identity).expect("list temp db");
        assert!(
            rows.is_empty(),
            "an already_bound bind must not run the auto-acquire hook: {rows:?}"
        );
    }

    // Control: the same repo/database/session through the acquire hook does
    // record a lease, so the empty table above is attributable to the skip.
    let warning = in_temp_repo(&repo, || auto_acquire_after_bind(245, Some("session-A")));
    assert_eq!(warning, None, "the reuse path stays silent on a clean tree");
    let storage = Storage::open_at(&db_path).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        1,
        "the control acquire records exactly one active lease"
    );
}

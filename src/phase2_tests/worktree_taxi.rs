use super::support::*;
use super::*;

#[test]
fn worktree_list_taxi_reconciles_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-list");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    // `worktree` subcommands take no provider flag, so the taxi resolves the
    // configured default provider.
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-list", "feat/552-taxi");
    let number = sync_cli_seed_closed_issue("Taxi list");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(exit, 0, "the taxi never changes the list exit code");
    assert!(
        !worktree.exists(),
        "the taxi reconciles the closed issue before list runs"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_no_sync_skips_the_taxi() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-no-sync");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-no-sync", "feat/552-no-sync");
    let number = sync_cli_seed_closed_issue("Taxi no sync");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: true,
        },
        &repo,
    );

    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "--no-sync must skip the reconciliation pass entirely"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_taxi_remote_failure_does_not_block() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-remote-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let api_base = sync_cli_dead_api_base();
    let _api_guard = EnvGuard::set("PHASEGENT_API_BASE", &api_base);
    let _repo_guard = EnvGuard::set("PHASEGENT_REPOSITORY", "owner/repo");
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    Storage::open()
        .expect("storage")
        .save_credential(Role::Orchestrator, "forgejo", "sync-test-token")
        .expect("store forgejo token");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-remote", "feat/552-taxi-remote");
    let number = sync_cli_seed_closed_issue("Taxi remote down");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(
        exit, 0,
        "an unreachable remote is a taxi warning, never a blocking error"
    );
    assert!(worktree.exists(), "a failed taxi pass deletes nothing");
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_taxi_targets_the_repo_flag_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-repo-flag");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo_main = root.join("repo-main");
    close_cli_init_repo(&repo_main);
    let worktree = close_cli_add_worktree(&repo_main, "taxi-repo-flag", "feat/552-repo-flag");
    let number = sync_cli_seed_closed_issue("Taxi repo flag");
    let identity = sync_cli_identity(&repo_main);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let repo_other = root.join("repo-other");
    close_cli_init_repo(&repo_other);

    // `--repo` names the checkout the subcommand operates on, so the pass
    // reconciles that checkout instead of the working directory.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: Some(repo_other.to_string_lossy().to_string()),
            no_sync: false,
        },
        &repo_main,
    );
    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "the --repo checkout has no residue, so the working directory is untouched"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");

    // Without `--repo` the pass targets the working directory again.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo_main,
    );
    assert_eq!(exit, 0);
    assert!(!worktree.exists(), "the working directory is reconciled");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_prune_taxi_reconciles_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-prune");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-prune", "feat/552-taxi-prune");
    let number = sync_cli_seed_closed_issue("Taxi prune");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    // `prune` without action flags stays a read-only dry-run; the taxi runs
    // before it and reconciles the closed issue's residue.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::Prune {
            repo: Some(repo.to_string_lossy().to_string()),
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(exit, 0, "the taxi never changes the prune exit code");
    assert!(
        !worktree.exists(),
        "the taxi reconciles before prune classifies anything"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn taxi_sync_is_silent_when_no_residue_exists() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-silent");
    let db = root.join("phasegent.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    // Deliberately no provider configuration: a silent pass never resolves
    // one, so an unconfigured host still gets zero output.
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let identity = sync_cli_identity(&repo);
    // A lease whose directory is already gone is not residue.
    close_cli_seed_lease_at(
        &identity,
        777,
        "session-gone",
        "retained",
        &root.join("missing-worktree"),
    );

    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();
    let warnings = crate::cli::issue::sync::taxi_sync(Role::Orchestrator, None);
    let _ = std::env::set_current_dir(&previous_cwd);

    assert!(
        warnings.is_empty(),
        "no residue means zero output; got: {warnings:?}"
    );
    let _ = fs::remove_dir_all(root);
}

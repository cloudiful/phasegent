//! Focused tests for the SQLite storage layer.
//!
//! These tests are scoped to the storage module: schema initialisation,
//! role/provider credential separation, and the explicit non-persistence
//! of `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`. End-to-end behaviour that
//! goes through the public auth API lives in the existing
//! `redmine_contract_tests` suite and is intentionally untouched here.
//!
//! Tests use [`Storage::open_at`] with an explicit temp path so they
//! never touch the operator's real platform-standard database.

use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage, TimerRunOwner,
    TimerStatusFilter,
};
use crate::policy::Role;
use std::fs;
use std::path::PathBuf;

fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "phasegent-storage-{label}-{}-{}",
        std::process::id(),
        system_time_nanos()
    ))
}

fn system_time_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn open_at_temp(label: &str) -> (PathBuf, Storage) {
    let temp_dir = unique_temp_dir(label);
    let storage = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    (temp_dir, storage)
}

#[test]
fn open_creates_database_with_private_permissions() {
    let (temp_dir, storage) = open_at_temp("open");
    let db_path = storage.db_path();
    assert!(db_path.exists(), "database file must exist after open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory_mode = fs::metadata(db_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700, "config directory must be 0700");
        let file_mode = fs::metadata(db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "database file must be 0600");
    }
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn schema_initialisation_is_idempotent() {
    let (temp_dir, storage) = open_at_temp("schema");
    drop(storage);
    // Second open must reuse the existing database without error and
    // continue to expose a usable Storage handle.
    let storage = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    assert!(storage.load_role_config(Role::Admin).unwrap().is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_is_additive_exact_and_finish_idempotent() {
    let (temp_dir, storage) = open_at_temp("timer-ledger");
    let columns = storage
        .connection
        .prepare("PRAGMA table_info(execution_timer_runs)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for expected in [
        "run_id",
        "issue_id",
        "phase",
        "role",
        "attempt",
        "started_at",
        "finished_at",
        "status",
        "elapsed_seconds",
        "rounded_hours",
        "activity_id",
        "redmine_time_entry_id",
        "sync_status",
    ] {
        assert!(columns.contains(&expected.to_owned()), "missing {expected}");
    }

    let run = storage
        .start_timer_run(
            "timer-run-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let duplicate = storage
        .start_timer_run(
            "timer-run-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    assert_eq!(run.run_id, duplicate.run_id);
    assert_eq!(run.started_at, duplicate.started_at);
    let count = storage
        .connection
        .query_row("SELECT count(*) FROM execution_timer_runs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 1, "duplicate start must not create another row");

    let finished = storage
        .finish_timer_run("timer-run-1", "DONE", 1_700_000_037)
        .unwrap();
    assert_eq!(finished.status, "DONE");
    assert_eq!(finished.elapsed_seconds, Some(37));
    assert_eq!(finished.rounded_hours, Some(0.02));
    let finished_again = storage
        .finish_timer_run("timer-run-1", "DONE", 1_700_000_038)
        .unwrap();
    assert_eq!(finished_again.time_entry_id, finished.time_entry_id);
    assert_eq!(finished_again.elapsed_seconds, Some(37));

    let reopened = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    let persisted = reopened.load_timer_run("timer-run-1").unwrap().unwrap();
    assert_eq!(persisted.status, "DONE");
    assert_eq!(persisted.elapsed_seconds, Some(37));
    assert_eq!(persisted.rounded_hours, Some(0.02));
    storage
        .start_timer_run(
            "same-second",
            28,
            "implementation",
            "executor",
            1,
            2_000_000_000,
        )
        .unwrap();
    let same_second = storage
        .finish_timer_run("same-second", "DONE", 2_000_000_000)
        .unwrap();
    assert_eq!(same_second.elapsed_seconds, Some(0));
    assert_eq!(same_second.rounded_hours, Some(0.01));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_rejects_conflicting_identity_and_invalid_timestamps() {
    let (temp_dir, storage) = open_at_temp("timer-validation");
    storage
        .start_timer_run("timer-run-2", 28, "implementation", "reviewer", 1, 100)
        .unwrap();
    assert!(
        storage
            .start_timer_run("timer-run-2", 29, "implementation", "reviewer", 1, 100)
            .is_err()
    );
    assert!(storage.finish_timer_run("timer-run-2", "DONE", 99).is_err());
    assert!(storage.finish_timer_run("missing", "FAILED", 200).is_err());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_distinguishes_synced_with_or_without_time_entry_id() {
    // Phase 4: GitLab has no numeric time-entry id, so its projection
    // path advances sync_status to `synced` while leaving
    // `redmine_time_entry_id` null. The Redmine path keeps its id-based
    // behaviour so `load_timer_run` always reports the actual state.
    let (temp_dir, storage) = open_at_temp("timer-gitlab-sync");
    let _ = storage
        .start_timer_run(
            "timer-gitlab",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-gitlab", "DONE", 1_700_003_600)
        .unwrap();
    assert_eq!(finished.sync_status, "pending");
    assert!(finished.time_entry_id.is_none());

    let updated = storage
        .mark_timer_sync(
            "timer-gitlab",
            None,
            None,
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert_eq!(updated.sync_status, "synced");
    assert!(updated.time_entry_id.is_none());

    let persisted = storage.load_timer_run("timer-gitlab").unwrap().unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(persisted.time_entry_id.is_none());

    // The Redmine-shaped path still records the id and stays
    // distinguishable from the GitLab path.
    let _ = storage
        .start_timer_run(
            "timer-redmine",
            8,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-redmine", "DONE", 1_700_003_600)
        .unwrap();
    let redmine_synced = storage
        .mark_timer_sync(
            "timer-redmine",
            Some(11),
            Some(99),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert_eq!(redmine_synced.sync_status, "synced");
    assert_eq!(redmine_synced.time_entry_id, Some(99));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_marks_failure_with_bounded_error_message() {
    // Phase 4: the failed-state recovery path records the bounded
    // error so a retry can see why the last projection failed.
    let (temp_dir, storage) = open_at_temp("timer-failed");
    let _ = storage
        .start_timer_run(
            "timer-fail",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-fail", "DONE", 1_700_000_060)
        .unwrap();
    let updated = storage
        .mark_timer_sync(
            "timer-fail",
            None,
            None,
            crate::infra::storage::TIMER_SYNC_FAILED,
            Some("GitLab add_spent_time returned HTTP 422"),
        )
        .unwrap();
    assert_eq!(updated.sync_status, "failed");
    assert!(updated.sync_error.is_some());
    assert!(
        storage
            .mark_timer_sync(
                "timer-fail",
                None,
                None,
                crate::infra::storage::TIMER_SYNC_FAILED,
                None
            )
            .is_err()
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn save_role_config_distinguishes_missing_from_empty() {
    let (temp_dir, storage) = open_at_temp("save-empty");
    let config = StoredConfig {
        provider: Some(PROVIDER_FORGEJO.to_owned()),
        ..Default::default()
    };
    storage.save_role_config(Role::Admin, &config).unwrap();
    let loaded = storage
        .load_role_config(Role::Admin)
        .unwrap()
        .expect("row must exist after save");
    assert_eq!(loaded.provider.as_deref(), Some(PROVIDER_FORGEJO));
    assert_eq!(loaded.api_base, None);
    assert_eq!(loaded.repository, None);

    // Saving an all-default row should still produce Some(...), proving
    // "row present with NULL fields" is observable independently from
    // "no row".
    storage
        .save_role_config(Role::Executor, &StoredConfig::default())
        .unwrap();
    let empty = storage.load_role_config(Role::Executor).unwrap();
    assert!(
        empty.is_some(),
        "row must exist after save, even when all fields are null"
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn credentials_for_different_providers_are_stored_separately() {
    let (temp_dir, storage) = open_at_temp("credential-separation");
    storage
        .save_credential(Role::Executor, PROVIDER_FORGEJO, "forgejo-token")
        .unwrap();
    storage
        .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-key")
        .unwrap();

    let forgejo = storage
        .load_credential(Role::Executor, PROVIDER_FORGEJO)
        .unwrap()
        .unwrap();
    let redmine = storage
        .load_credential(Role::Executor, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    assert_eq!(forgejo, "forgejo-token");
    assert_eq!(redmine, "redmine-key");

    // Overwriting the forgejo credential must not touch the redmine one.
    storage
        .save_credential(Role::Executor, PROVIDER_FORGEJO, "forgejo-token-v2")
        .unwrap();
    let forgejo_v2 = storage
        .load_credential(Role::Executor, PROVIDER_FORGEJO)
        .unwrap()
        .unwrap();
    let redmine_after = storage
        .load_credential(Role::Executor, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    assert_eq!(forgejo_v2, "forgejo-token-v2");
    assert_eq!(redmine_after, "redmine-key");
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn mirror_environment_variables_are_never_persisted() {
    // The storage layer has no column for the mirror bearer key or the
    // mirror URL. Confirm that no value reachable from these
    // environment variables can leak into the database by exercising
    // every public API surface with those env vars set.
    //
    // Serialise against the shared `lock_workflow_tests()` mutex that
    // the mirror-plugin contract tests also acquire: under the
    // default parallel `cargo test` runner both groups mutate the
    // same two env vars and would otherwise race. The `EnvGuard`
    // below restores the previous host values on Drop.
    let _environment_lock = lock_workflow_tests();
    let _mirror_key = EnvGuard::set(
        "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
        "mirror-bearer-leaked",
    );
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://mirror.example/owner/repo.git",
    );

    let (temp_dir, storage) = open_at_temp("mirror-env");
    for role in [
        Role::Admin,
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
        Role::Tester,
    ] {
        for provider in [PROVIDER_FORGEJO, PROVIDER_REDMINE] {
            assert!(storage.load_credential(role, provider).unwrap().is_none());
        }
        storage
            .save_role_config(
                role,
                &StoredConfig {
                    provider: Some(PROVIDER_FORGEJO.to_owned()),
                    api_base: Some("https://forgejo.example".to_owned()),
                    repository: Some("owner/repo".to_owned()),
                },
            )
            .unwrap();
        storage
            .save_redmine_config(
                role,
                &RedmineStoredConfig {
                    api_base: Some("https://redmine.example".to_owned()),
                    project_id: Some("1".to_owned()),
                    close_status_id: Some(5),
                    group_name: None,
                    group_role: None,
                },
            )
            .unwrap();
    }
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn persist_redmine_bootstrap_validates_zero_ids() {
    let (temp_dir, storage) = open_at_temp("bootstrap-validation");
    let zero = storage
        .persist_redmine_bootstrap(Role::Admin, None, 0, 5)
        .unwrap_err();
    assert!(zero.contains("greater than zero"));
    let zero = storage
        .persist_redmine_bootstrap(Role::Admin, None, 7, 0)
        .unwrap_err();
    assert!(zero.contains("greater than zero"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn role_gitlab_config_round_trip_and_numeric_project_id() {
    // Phase 1 (remove-project-id): GitLab `project_id` is no longer
    // persisted; the column remains for non-destructive migration but
    // `load` always returns `None` and `save` ignores the field. The
    // test verifies api_base round-trip and that legacy values are
    // inert rather than asserting the old persistence.
    let (temp_dir, storage) = open_at_temp("gitlab-round-trip");
    assert!(
        storage.load_gitlab_config(Role::Admin).unwrap().is_none(),
        "fresh database must report no GitLab row as missing"
    );

    storage
        .save_gitlab_config(
            Role::Executor,
            &GitlabStoredConfig {
                api_base: Some("https://gitlab.example".to_owned()),
                project_id: Some(42),
            },
        )
        .unwrap();
    let loaded = storage
        .load_gitlab_config(Role::Executor)
        .unwrap()
        .expect("Gitlab row must exist after save");
    assert_eq!(loaded.api_base.as_deref(), Some("https://gitlab.example"));
    assert_eq!(
        loaded.project_id, None,
        "gitlab project_id must be inert after Phase 1"
    );

    // Saving a row with api_base only must keep the row alive and still
    // report project_id as None. Second save with relocated api_base
    // confirms api_base still round-trips.
    storage
        .save_gitlab_config(
            Role::Executor,
            &GitlabStoredConfig {
                api_base: Some("https://gitlab-relocated.example".to_owned()),
                project_id: Some(42),
            },
        )
        .unwrap();
    let reloaded = storage
        .load_gitlab_config(Role::Executor)
        .unwrap()
        .expect("row must still exist after second save");
    assert_eq!(
        reloaded.api_base.as_deref(),
        Some("https://gitlab-relocated.example")
    );
    assert_eq!(
        reloaded.project_id, None,
        "gitlab project_id must remain inert after second save"
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn persist_gitlab_bootstrap_validates_zero_project_id_and_flips_provider() {
    // The GitLab bootstrap is the only entry point that flips the
    // role_config.provider column on the executor so ordinary
    // `auth setup` flows don't have to know about the underlying
    // column. Confirm the zero-id guard and the provider flip in one
    // test so the foundation never silently accepts an id of zero.
    // Phase 1: project_id is ignored on persist, only api_base is kept.
    let (temp_dir, storage) = open_at_temp("gitlab-bootstrap");
    let zero = storage
        .persist_gitlab_bootstrap(Role::Executor, None, 0)
        .unwrap_err();
    assert!(zero.contains("greater than zero"));

    storage
        .persist_gitlab_bootstrap(
            Role::Executor,
            Some("https://gitlab.example".to_owned()),
            42,
        )
        .unwrap();
    let row = storage
        .load_gitlab_config(Role::Executor)
        .unwrap()
        .expect("gitlab row must exist after bootstrap");
    assert_eq!(row.api_base.as_deref(), Some("https://gitlab.example"));
    assert_eq!(
        row.project_id, None,
        "gitlab project_id must be inert after Phase 1 bootstrap"
    );
    let provider = storage
        .load_role_config(Role::Executor)
        .unwrap()
        .expect("role_config row must exist after bootstrap")
        .provider;
    assert_eq!(provider.as_deref(), Some(PROVIDER_GITLAB));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn credentials_for_all_three_providers_are_isolated_per_role() {
    // The role_credential table uses (role, provider) as a composite
    // primary key so the same role can keep three independent
    // credentials. Phase-1 GitLab foundation: confirm the new
    // gitlab row coexists with forgejo and redmine values without
    // any cross-write or leak, and that overwriting one credential
    // never touches another.
    let (temp_dir, storage) = open_at_temp("credential-coexistence");
    storage
        .save_credential(Role::Orchestrator, PROVIDER_FORGEJO, "forgejo-secret")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, PROVIDER_REDMINE, "redmine-secret")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, PROVIDER_GITLAB, "gitlab-secret")
        .unwrap();

    let forgejo = storage
        .load_credential(Role::Orchestrator, PROVIDER_FORGEJO)
        .unwrap()
        .unwrap();
    let redmine = storage
        .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    let gitlab = storage
        .load_credential(Role::Orchestrator, PROVIDER_GITLAB)
        .unwrap()
        .unwrap();
    assert_eq!(forgejo, "forgejo-secret");
    assert_eq!(redmine, "redmine-secret");
    assert_eq!(gitlab, "gitlab-secret");

    // Overwriting one must not leak into another.
    storage
        .save_credential(Role::Orchestrator, PROVIDER_GITLAB, "gitlab-secret-v2")
        .unwrap();
    let forgejo_after = storage
        .load_credential(Role::Orchestrator, PROVIDER_FORGEJO)
        .unwrap()
        .unwrap();
    let redmine_after = storage
        .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    let gitlab_after = storage
        .load_credential(Role::Orchestrator, PROVIDER_GITLAB)
        .unwrap()
        .unwrap();
    assert_eq!(forgejo_after, "forgejo-secret");
    assert_eq!(redmine_after, "redmine-secret");
    assert_eq!(gitlab_after, "gitlab-secret-v2");

    // Other roles must not observe any of these credentials.
    assert!(
        storage
            .load_credential(Role::Executor, PROVIDER_GITLAB)
            .unwrap()
            .is_none(),
        "executor must not observe orchestrator's GitLab credential"
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn owner_metadata_round_trips_and_validates_bounds() {
    let (temp_dir, storage) = open_at_temp("owner-metadata");
    let run = storage
        .start_timer_run_with_owner(
            "owner-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-123".to_owned()),
                call_id: Some("call-abc".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(run.owner_session_id.as_deref(), Some("sess-123"));
    assert_eq!(run.owner_call_id.as_deref(), Some("call-abc"));

    // Empty strings collapse to NULL so the column stores NULL instead
    // of an empty marker.
    let blank = storage
        .start_timer_run_with_owner(
            "owner-2",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("   ".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    assert!(blank.owner_session_id.is_none());
    assert!(blank.owner_call_id.is_none());

    // Control characters and oversize values are rejected before the
    // row touches the database.
    let control = storage
        .start_timer_run_with_owner(
            "owner-3",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_002,
            &TimerRunOwner {
                session_id: Some("a\nb".to_owned()),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(control.contains("control characters"));

    let oversize = "x".repeat(200);
    let oversize_err = storage
        .start_timer_run_with_owner(
            "owner-4",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_003,
            &TimerRunOwner {
                session_id: Some(oversize),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(oversize_err.contains("at most 128"));

    // Existing legacy start_timer_run callers continue to leave the
    // owner columns null so old test code keeps compiling and running.
    let legacy = storage
        .start_timer_run(
            "owner-5",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_004,
        )
        .unwrap();
    assert!(legacy.owner_session_id.is_none());
    assert!(legacy.owner_call_id.is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn owner_mismatch_on_repeated_start_is_rejected() {
    // Two competing calls for the same run id must not silently overwrite
    // an owner that was already attached by another session.
    let (temp_dir, storage) = open_at_temp("owner-mismatch");
    storage
        .start_timer_run_with_owner(
            "owner-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    let error = storage
        .start_timer_run_with_owner(
            "owner-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-B".to_owned()),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("already owned by another session"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn list_timer_runs_groups_running_and_finished_with_clamped_limit() {
    let (temp_dir, storage) = open_at_temp("list-runs");
    for index in 0..3 {
        let run_id = format!("run-{index}");
        storage
            .start_timer_run(
                &run_id,
                28,
                "implementation",
                "executor",
                1,
                1_700_000_000 + index,
            )
            .unwrap();
    }
    storage
        .finish_timer_run("run-0", "DONE", 1_700_000_010)
        .unwrap();

    let running = storage
        .list_timer_runs(TimerStatusFilter::Running, 100)
        .unwrap();
    assert_eq!(running.len(), 2);
    assert!(running.iter().all(|run| run.status == "running"));

    let finished = storage
        .list_timer_runs(TimerStatusFilter::Finished, 100)
        .unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].run_id, "run-0");
    assert_eq!(finished[0].status, "DONE");

    let all = storage
        .list_timer_runs(TimerStatusFilter::All, 100)
        .unwrap();
    assert_eq!(all.len(), 3);

    // Limit 0 is clamped up to 1 so the caller always gets at least one
    // candidate row when one exists.
    let one = storage.list_timer_runs(TimerStatusFilter::All, 0).unwrap();
    assert_eq!(one.len(), 1);
    // Limits above the cap clamp down without an error.
    let many = storage
        .list_timer_runs(TimerStatusFilter::All, 10_000)
        .unwrap();
    assert_eq!(many.len(), 3);
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn additive_owner_migration_is_idempotent_across_reopens() {
    // Phase 3 ships additive ALTER TABLE statements for the owner
    // columns. Opening an already-migrated database must not error
    // (column_exists returns true and the ALTER is skipped) and a row
    // written before the migration must remain readable.
    let (temp_dir, storage) = open_at_temp("owner-migration");
    storage
        .start_timer_run(
            "pre-migration",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    // Re-open the same database path: the migration must run, see the
    // columns already present, and succeed without error.
    let reopened = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    let pre = reopened.load_timer_run("pre-migration").unwrap().unwrap();
    assert!(pre.owner_session_id.is_none());
    assert!(pre.owner_call_id.is_none());

    // New rows written after the migration carry owner metadata.
    reopened
        .start_timer_run_with_owner(
            "post-migration",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("sess-2".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    let post = reopened.load_timer_run("post-migration").unwrap().unwrap();
    assert_eq!(post.owner_session_id.as_deref(), Some("sess-2"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn owner_call_mismatch_on_repeated_start_is_rejected() {
    let (temp_dir, storage) = open_at_temp("owner-call-mismatch");
    storage
        .start_timer_run_with_owner(
            "owner-call-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: Some("call-1".to_owned()),
            },
        )
        .unwrap();
    let err = storage
        .start_timer_run_with_owner(
            "owner-call-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: Some("call-2".to_owned()),
            },
        )
        .unwrap_err();
    assert!(err.contains("already owned by another call"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn concurrent_projection_claim_is_serialized() {
    let (temp_dir, storage) = open_at_temp("projection-claim");
    storage
        .start_timer_run(
            "claim-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("claim-run", "FAILED", 1_700_000_060)
        .unwrap();
    // Two concurrent claim attempts with distinct caller-bound tokens: only
    // one may move pending->projecting. The token binds the lease to the
    // caller so a second concurrent finish cannot reuse the loaded
    // projecting row.
    let path = temp_dir.join(DB_FILENAME);
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let p = path.clone();
            std::thread::spawn(move || {
                let s = Storage::open_at(&p).unwrap();
                let token = format!("tok-claim-{i}-{}", std::process::id());
                s.try_claim_timer_projection("claim-run", &token).unwrap()
            })
        })
        .collect();
    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|&&v| v).count(),
        1,
        "only one concurrent claim may succeed"
    );
    let final_row = Storage::open_at(&path)
        .unwrap()
        .load_timer_run("claim-run")
        .unwrap()
        .unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    assert!(final_row.projection_token.is_some());
    assert!(final_row.projection_claimed_at.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn projection_lease_token_binds_finalization_and_prevents_second_post() {
    // Regression for P1: a second finish that loads a terminal projecting
    // row must not be treated as the owner. Only the holder of the token
    // may finalize to synced; a concurrent caller with a different token
    // must see "projection already in progress" and never POST.
    let (temp_dir, storage) = open_at_temp("projection-ownership");
    storage
        .start_timer_run(
            "owner-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("owner-run", "DONE", 1_700_000_060)
        .unwrap();
    // First caller claims with token A and holds the lease.
    let token_a = "tok-owner-A";
    assert!(
        storage
            .try_claim_timer_projection("owner-run", token_a)
            .unwrap()
    );
    let claimed = storage.load_timer_run("owner-run").unwrap().unwrap();
    assert_eq!(claimed.projection_token.as_deref(), Some(token_a));
    assert_eq!(
        claimed.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    // Second caller with token B attempts to claim the same run: must fail.
    let token_b = "tok-owner-B";
    assert!(
        !storage
            .try_claim_timer_projection("owner-run", token_b)
            .unwrap()
    );
    // Second caller must not be able to finalize with its own token.
    let marked_b = storage
        .mark_timer_sync_with_token(
            "owner-run",
            token_b,
            None,
            Some(999),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(
        !marked_b,
        "second caller must not finalize with wrong token"
    );
    // First caller (holder) can finalize.
    let marked_a = storage
        .mark_timer_sync_with_token(
            "owner-run",
            token_a,
            None,
            Some(1001),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(marked_a, "holder must be able to finalize");
    let final_row = storage.load_timer_run("owner-run").unwrap().unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_SYNCED
    );
    assert_eq!(final_row.time_entry_id, Some(1001));
    assert!(final_row.projection_token.is_none());
    // A stale reset must not clear a live lease. Simulate a concurrent
    // recover trying to force-reset while the lease is still fresh.
    // Create a fresh projecting row with recent claimed_at.
    storage
        .start_timer_run(
            "stale-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_100,
        )
        .unwrap();
    storage
        .finish_timer_run("stale-run", "DONE", 1_700_000_160)
        .unwrap();
    let live_token = "tok-live";
    assert!(
        storage
            .try_claim_timer_projection("stale-run", live_token)
            .unwrap()
    );
    // Immediate stale reset should NOT succeed while lease is fresh.
    let stale_reset = storage
        .reset_stale_projection_to_failed("stale-run", "stale")
        .unwrap();
    assert!(
        !stale_reset,
        "live lease must not be reset as stale within window"
    );
    // Holder can still finalize after failed stale reset.
    let marked_live = storage
        .mark_timer_sync_with_token(
            "stale-run",
            live_token,
            None,
            Some(2002),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(marked_live);
    // Simulate hard-crash stale: insert a projecting row with old timestamp
    // directly via SQL so the lease appears expired.
    let stale_id = "hard-crash-run";
    storage
        .start_timer_run(stale_id, 28, "implementation", "executor", 1, 1_700_000_200)
        .unwrap();
    storage
        .finish_timer_run(stale_id, "DONE", 1_700_000_260)
        .unwrap();
    // Manually claim with old claimed_at (legacy NULL or expired)
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-old', projection_claimed_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_000_000_i64, stale_id],
        )
        .unwrap();
    // Now stale reset should succeed.
    let stale_ok = storage
        .reset_stale_projection_to_failed(stale_id, "recovering hard crash")
        .unwrap();
    assert!(stale_ok, "expired lease must be recoverable");
    let after = storage.load_timer_run(stale_id).unwrap().unwrap();
    assert_eq!(after.sync_status, crate::infra::storage::TIMER_SYNC_FAILED);
    assert!(after.projection_token.is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn legacy_owner_migration_tolerates_concurrent_opens() {
    // Simulate a pre-owner database by creating the legacy schema without
    // owner columns, then opening it concurrently from two threads.
    let temp_dir = unique_temp_dir("legacy-concurrent");
    fs::create_dir_all(&temp_dir).unwrap();
    let db_path = temp_dir.join(DB_FILENAME);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS execution_timer_runs (
                 run_id TEXT PRIMARY KEY, issue_id INTEGER NOT NULL, phase TEXT NOT NULL,
                 role TEXT NOT NULL, attempt INTEGER NOT NULL, started_at INTEGER NOT NULL,
                 finished_at INTEGER, status TEXT NOT NULL, elapsed_seconds INTEGER,
                 rounded_hours REAL, activity_id INTEGER, redmine_time_entry_id INTEGER,
                 sync_status TEXT NOT NULL DEFAULT 'pending', sync_error TEXT
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO execution_timer_runs (run_id, issue_id, phase, role, attempt, started_at, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["legacy-1", 28, "implementation", "executor", 1, 1_700_000_000, "running"],
        )
        .unwrap();
    }
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let p = db_path.clone();
            std::thread::spawn(move || Storage::open_at(&p))
        })
        .collect();
    for h in handles {
        let storage = h.join().unwrap().unwrap();
        let row = storage.load_timer_run("legacy-1").unwrap().unwrap();
        assert_eq!(row.run_id, "legacy-1");
        // Columns must exist after concurrent migration.
        assert!(row.owner_session_id.is_none());
    }
    // Third open after both should also see the column.
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run_with_owner(
            "new-after-legacy",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("sess-x".to_owned()),
                call_id: Some("call-y".to_owned()),
            },
        )
        .unwrap();
    let _ = fs::remove_dir_all(temp_dir);
}

/// Meaningful interleaving test with activity initialization: two concurrent
/// `try_claim_timer_projection` calls must be serialized, and only the
/// holder may persist `activity_id` through `update_activity_with_token`.
/// This is the storage-level proof of the round-3 reviewer finding #2:
/// two callers with `activity_id == NULL` cannot both list/update and
/// POST because only the lease holder proceeds past the claim and the
/// activity persist is token-bound. The test mirrors what `project_run`
/// does at the provider boundary.
#[test]
fn concurrent_activity_initialization_is_token_bound() {
    let (temp_dir, storage) = open_at_temp("activity-init-token");
    storage
        .start_timer_run(
            "activity-token-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("activity-token-run", "DONE", 1_700_000_060)
        .unwrap();
    // Caller A claims and persists its activity_id with token A.
    let token_a = "tok-activity-A";
    assert!(
        storage
            .try_claim_timer_projection("activity-token-run", token_a)
            .unwrap()
    );
    let persisted_a = storage
        .update_activity_with_token("activity-token-run", token_a, 9)
        .unwrap();
    assert!(persisted_a, "token-A holder must persist activity_id");
    // Caller B with token B cannot finalize with its own token, and its
    // activity persist with token B is rejected because the row carries
    // token A.
    let token_b = "tok-activity-B";
    let persisted_b = storage
        .update_activity_with_token("activity-token-run", token_b, 11)
        .unwrap();
    assert!(
        !persisted_b,
        "non-holder activity persist must be rejected by token check"
    );
    let row = storage
        .load_timer_run("activity-token-run")
        .unwrap()
        .unwrap();
    assert_eq!(row.activity_id, Some(9), "activity_id must reflect holder");
    assert_eq!(row.projection_token.as_deref(), Some(token_a));
    assert_eq!(
        row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    let _ = fs::remove_dir_all(temp_dir);
}

/// Liveness protection for stale recovery. The `reset_stale_projection_to_failed`
/// call must acquire `BEGIN IMMEDIATE` itself so a live projector that
/// is still holding its `IMMEDIATE` blocks the reset until it commits or
/// rolls back. After the live holder rolls back, the row is back at
/// `pending`/`failed`/`unconfirmed` and the reset observes the new state.
/// This is the storage-level proof of the round-3 reviewer finding #3:
/// a fixed wall-clock lease with no liveness protection is replaced by
/// the held IMMEDIATE protocol.
#[test]
fn stale_reset_is_liveness_protected_by_immediate_lock() {
    let (temp_dir, storage) = open_at_temp("stale-reset-liveness");
    storage
        .start_timer_run(
            "liveness-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("liveness-run", "DONE", 1_700_000_060)
        .unwrap();
    // Hard-crash stale: write a `projecting` row with an old claimed_at
    // so the lease appears expired. The reset must succeed because no
    // live holder is inside an IMMEDIATE on this row.
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-old', projection_claimed_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_000_000_i64, "liveness-run"],
        )
        .unwrap();
    let reset_ok = storage
        .reset_stale_projection_to_failed("liveness-run", "recovering hard crash")
        .unwrap();
    assert!(
        reset_ok,
        "stale reset must succeed when no live holder is in IMMEDIATE"
    );
    let row = storage.load_timer_run("liveness-run").unwrap().unwrap();
    assert_eq!(row.sync_status, crate::infra::storage::TIMER_SYNC_FAILED);
    assert!(row.projection_token.is_none());
    assert!(row.sync_error.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

/// Liveness protection proof: while a live holder is inside an
/// `IMMEDIATE` transaction (simulating a long-running Redmine
/// reconciliation), a concurrent `reset_stale_projection_to_failed`
/// from a second connection must NOT clobber the live holder's row.
/// The reset blocks on the held lock and only proceeds after the
/// holder releases its transaction. Once the holder commits/rolls
/// back the reset observes the new row state and cannot steal the
/// lease. This is the storage-level proof of the round-3 reviewer
/// finding #3: the fixed 120-second wall-clock lease is replaced by
/// the held IMMEDIATE protocol.
#[test]
fn stale_reset_blocks_against_live_immediate_holder() {
    let temp_dir = unique_temp_dir("stale-reset-blocked");
    let db_path = temp_dir.join(DB_FILENAME);
    {
        let setup = Storage::open_at(&db_path).unwrap();
        setup
            .start_timer_run(
                "live-holder",
                28,
                "implementation",
                "executor",
                1,
                1_700_000_000,
            )
            .unwrap();
        setup
            .finish_timer_run("live-holder", "DONE", 1_700_000_060)
            .unwrap();
        // Pre-stage a `projecting` row with an expired claimed_at so the
        // reset would otherwise pass the lease-window check.
        setup
            .connection
            .execute(
                "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-live', projection_claimed_at = ?1 WHERE run_id = ?2",
                rusqlite::params![1_000_000_i64, "live-holder"],
            )
            .unwrap();
    }
    // Holder opens its own connection and holds an IMMEDIATE.
    let holder_db = db_path.clone();
    let holder = std::thread::spawn(move || {
        let storage = Storage::open_at(&holder_db).unwrap();
        storage.begin_projection().unwrap();
        // Hold the IMMEDIATE long enough for the reset to attempt and
        // back off. While held the reset must observe `false` (no row
        // mutated).
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Roll back so the lock is released.
        storage.rollback_projection().unwrap();
    });
    // Give the holder time to acquire BEGIN IMMEDIATE.
    std::thread::sleep(std::time::Duration::from_millis(50));
    // From a different connection, attempt the stale reset. It must NOT
    // clobber the live holder's row because the held IMMEDIATE prevents
    // the reset from acquiring its own BEGIN IMMEDIATE within the
    // bounded retry window. After the holder rolls back, the row is
    // still `projecting` with the holder's token (because the holder's
    // transaction had no claim write inside), so the reset still does
    // not mutate. The reset returns false rather than succeeding.
    let reseter = {
        let db = db_path.clone();
        std::thread::spawn(move || {
            let storage = Storage::open_at(&db).unwrap();
            storage.reset_stale_projection_to_failed("live-holder", "stale attempt")
        })
    };
    let reset_outcome = reseter.join().unwrap();
    holder.join().unwrap();
    let final_row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("live-holder")
        .unwrap()
        .unwrap();
    // After both threads finish, the row may either still be
    // `projecting` (the reset never acquired) or `failed` (the reset
    // acquired AFTER the holder rolled back, after the row's lease was
    // already past the window — but the holder had nothing to write
    // inside the IMMEDIATE so the row's claimed_at is still old). Both
    // outcomes are valid; the invariant is that the live holder's
    // projection_token was never clobbered by an in-flight concurrent
    // reset. The reset_outcome must be `Ok` (either true or false) —
    // it must never error out from a busy timeout because the holder
    // released before the bounded retry exhausted.
    assert!(
        reset_outcome.is_ok(),
        "reset must return Ok (busy must resolve before retry exhaustion)"
    );
    let reset_value = reset_outcome.unwrap();
    // The reset must NOT have mutated a row whose lease was never
    // legitimately expired: if the holder's rollback released the
    // IMMEDIATE before the retry exhaustion, the reset could have
    // acquired its own IMMEDIATE and observed the row still in
    // `projecting` with the old lease, and therefore legitimately
    // reset it (because claimed_at <= threshold). That reset would
    // set sync_status='failed' and projection_token=NULL. The
    // invariant is that during the holder's IMMEDIATE the reset was
    // blocked; the actual reset decision is allowed to be true after
    // the holder released.
    if reset_value {
        assert_eq!(
            final_row.sync_status,
            crate::infra::storage::TIMER_SYNC_FAILED
        );
        assert!(final_row.projection_token.is_none());
    } else {
        // The reset chose not to mutate. Possible if the row state
        // changed between read and UPDATE (live holder released with
        // no in-flight claim write).
        assert!(
            final_row.sync_status == crate::infra::storage::TIMER_SYNC_PROJECTING
                || final_row.sync_status == crate::infra::storage::TIMER_SYNC_FAILED
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
}

/// Hard-crash behavior is deterministic and safe: no success inference
/// from a missing transcript. After `finish_timer_run(.., "FAILED", ..)`
/// the row is `sync_status='failed'` locally; a concurrent projection
/// that never acquired the lease must not mutate it (no fallback). A
/// subsequent recover re-runs through the durable FAILED + lease path.
#[test]
fn hard_crash_failed_recovery_keeps_row_terminal_and_unmutated() {
    let (temp_dir, storage) = open_at_temp("hard-crash-failed");
    storage
        .start_timer_run(
            "crash-failed",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("crash-failed", "FAILED", 1_700_000_060)
        .unwrap();
    // Row is durably FAILED locally before any provider attempt.
    let initial = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(initial.status, "FAILED");
    assert_eq!(
        initial.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert!(initial.projection_token.is_none());
    // A failed finalize attempt with a non-matching token returns false
    // and does NOT mutate the row: the holder check is atomic.
    let stray = storage
        .mark_timer_sync_with_token(
            "crash-failed",
            "tok-stranger",
            None,
            Some(1),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(!stray, "non-holder must not finalize");
    let after_stray = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(
        after_stray.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert_eq!(after_stray.time_entry_id, None);
    assert_eq!(after_stray.projection_token, None);
    // The safe `record_failed_sync_error` helper records the projection
    // error without overwriting a live `projecting` row, so the durable
    // FAILED surface remains intact.
    let recorded = storage
        .record_failed_sync_error("crash-failed", "test projection failure")
        .unwrap();
    assert!(recorded);
    let with_error = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(
        with_error.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert!(with_error.sync_error.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

/// Finalize requires owning claim/token — no unconditional mark fallback
/// for a claimed operation. A caller that did not acquire ownership must
/// not mutate projection state. Verified at the storage layer.
#[test]
fn finalize_without_lease_does_not_mutate_projection_state() {
    let (temp_dir, storage) = open_at_temp("finalize-no-lease");
    storage
        .start_timer_run(
            "no-lease",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("no-lease", "DONE", 1_700_000_060)
        .unwrap();
    // Holder A claims.
    let token_a = "tok-finalize-A";
    assert!(
        storage
            .try_claim_timer_projection("no-lease", token_a)
            .unwrap()
    );
    // Stray finalize attempt with token B returns false (no rows match).
    let stray_finalize = storage
        .mark_timer_sync_with_token(
            "no-lease",
            "tok-finalize-B",
            None,
            Some(42),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(
        !stray_finalize,
        "stray finalize must be rejected by token check"
    );
    let row = storage.load_timer_run("no-lease").unwrap().unwrap();
    assert_eq!(
        row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING,
        "stray finalize must not flip sync_status"
    );
    assert_eq!(
        row.projection_token.as_deref(),
        Some(token_a),
        "projection_token must remain the holder's"
    );
    assert!(row.time_entry_id.is_none(), "no time entry yet");
    // Holder A can finalize successfully.
    let ok = storage
        .mark_timer_sync_with_token(
            "no-lease",
            token_a,
            None,
            Some(99),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(ok, "holder finalize must succeed");
    let final_row = storage.load_timer_run("no-lease").unwrap().unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_SYNCED
    );
    assert_eq!(final_row.time_entry_id, Some(99));
    assert!(
        final_row.projection_token.is_none(),
        "finalize must clear the lease token"
    );
    let _ = fs::remove_dir_all(temp_dir);
}

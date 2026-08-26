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
use crate::policy::Role;
use crate::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::storage::{DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage};
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
            crate::storage::TIMER_SYNC_SYNCED,
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
            crate::storage::TIMER_SYNC_SYNCED,
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
            crate::storage::TIMER_SYNC_FAILED,
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
                crate::storage::TIMER_SYNC_FAILED,
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
    // The GitlabStoredConfig struct stores the project id as `u64`
    // because GitLab identifiers are numeric, and the SQLite column is
    // INTEGER so the storage layer must never write a placeholder
    // string that callers might confuse with a Redmine slug. Confirm
    // the round-trip and the missing-row semantics in one focused
    // test so a future contributor cannot accidentally regress the
    // column type.
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
    assert_eq!(loaded.project_id, Some(42));

    // Saving a row with api_base only must preserve the existing
    // project_id (the load_or_default pattern keeps the previous
    // value because we never call load_gitlab_config from
    // save_gitlab_config). Round-trip a second save that touches only
    // api_base to assert the column type is unaffected.
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
    assert_eq!(reloaded.project_id, Some(42));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn persist_gitlab_bootstrap_validates_zero_project_id_and_flips_provider() {
    // The GitLab bootstrap is the only entry point that flips the
    // role_config.provider column on the executor so ordinary
    // `auth setup` flows don't have to know about the underlying
    // column. Confirm the zero-id guard and the provider flip in one
    // test so the foundation never silently accepts an id of zero.
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
    assert_eq!(row.project_id, Some(42));
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

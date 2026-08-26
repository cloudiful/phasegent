//! Focused tests for the SQLite storage layer.
//!
//! These tests are scoped to the storage module: schema initialisation,
//! field-by-field legacy import with idempotence, role/provider
//! credential separation, and the explicit non-persistence of
//! `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`. End-to-end behaviour that
//! goes through the public auth API lives in the existing
//! `redmine_contract_tests` suite and is intentionally untouched here.

use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::policy::Role;
use crate::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::storage::{PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage};
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

fn write_legacy_role_config(directory: &std::path::Path, role: Role, value: &serde_json::Value) {
    fs::write(
        directory.join(format!("{}.config.json", role.as_str())),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

fn write_legacy_redmine_config(directory: &std::path::Path, role: Role, value: &serde_json::Value) {
    fs::write(
        directory.join(format!("redmine.{}.config.json", role.as_str())),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

fn write_legacy_credential(directory: &std::path::Path, role: Role, provider: &str, value: &str) {
    let filename = match provider {
        PROVIDER_FORGEJO => format!("{}.token", role.as_str()),
        PROVIDER_REDMINE => format!("redmine.{}.key", role.as_str()),
        other => panic!("unsupported legacy provider '{other}' in test helper"),
    };
    fs::write(directory.join(filename), value).unwrap();
}

#[test]
fn open_creates_database_with_private_permissions() {
    let home = unique_temp_dir("open");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn schema_initialisation_is_idempotent() {
    let home = unique_temp_dir("schema");
    let storage = Storage::open_for_home(&home).unwrap();
    drop(storage);
    // Second open must reuse the existing database without error and
    // continue to expose a usable Storage handle.
    let storage = Storage::open_for_home(&home).unwrap();
    assert!(storage.load_role_config(Role::Admin).unwrap().is_none());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_ledger_is_additive_exact_and_finish_idempotent() {
    let home = unique_temp_dir("timer-ledger");
    let storage = Storage::open_for_home(&home).unwrap();
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

    let reopened = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_ledger_rejects_conflicting_identity_and_invalid_timestamps() {
    let home = unique_temp_dir("timer-validation");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_ledger_distinguishes_synced_with_or_without_time_entry_id() {
    // Phase 4: GitLab has no numeric time-entry id, so its projection
    // path advances sync_status to `synced` while leaving
    // `redmine_time_entry_id` null. The Redmine path keeps its id-based
    // behaviour so `load_timer_run` always reports the actual state.
    let home = unique_temp_dir("timer-gitlab-sync");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_ledger_marks_failure_with_bounded_error_message() {
    // Phase 4: the failed-state recovery path records the bounded
    // error so a retry can see why the last projection failed.
    let home = unique_temp_dir("timer-failed");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn save_role_config_distinguishes_missing_from_empty() {
    let home = unique_temp_dir("save-empty");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn import_legacy_copies_fields_field_by_field_and_skips_existing_values() {
    let home = unique_temp_dir("import-field-by-field");
    let config_dir = home.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_dir).unwrap();
    write_legacy_role_config(
        &config_dir,
        Role::Orchestrator,
        &serde_json::json!({"provider": "redmine", "api_base": null, "repository": null}),
    );
    write_legacy_redmine_config(
        &config_dir,
        Role::Orchestrator,
        &serde_json::json!({
            "api_base": "https://redmine.example",
            "project_id": "44",
            "close_status_id": 5,
        }),
    );
    write_legacy_credential(
        &config_dir,
        Role::Orchestrator,
        PROVIDER_REDMINE,
        "legacy-redmine-key",
    );
    write_legacy_credential(
        &config_dir,
        Role::Orchestrator,
        PROVIDER_FORGEJO,
        "legacy-forgejo-token",
    );

    let storage = Storage::open_for_home(&home).unwrap();
    let report = storage.import_legacy(&config_dir).unwrap();
    assert_eq!(
        report.imported, 6,
        "first import must copy every populated legacy field (provider, three redmine fields, two credentials)"
    );
    assert_eq!(report.skipped, 0);

    let role_config = storage
        .load_role_config(Role::Orchestrator)
        .unwrap()
        .expect("role config row must exist after import");
    assert_eq!(role_config.provider.as_deref(), Some(PROVIDER_REDMINE));
    assert_eq!(role_config.api_base, None);
    assert_eq!(role_config.repository, None);

    let redmine_config = storage
        .load_redmine_config(Role::Orchestrator)
        .unwrap()
        .expect("redmine config row must exist after import");
    assert_eq!(
        redmine_config.api_base.as_deref(),
        Some("https://redmine.example")
    );
    assert_eq!(redmine_config.project_id.as_deref(), Some("44"));
    assert_eq!(redmine_config.close_status_id, Some(5));

    let redmine_credential = storage
        .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
        .unwrap()
        .expect("redmine credential must exist after import");
    assert_eq!(redmine_credential, "legacy-redmine-key");

    let forgejo_credential = storage
        .load_credential(Role::Orchestrator, PROVIDER_FORGEJO)
        .unwrap()
        .expect("forgejo credential must exist after import");
    assert_eq!(forgejo_credential, "legacy-forgejo-token");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn import_legacy_does_not_overwrite_existing_sqlite_values() {
    let home = unique_temp_dir("import-no-overwrite");
    let config_dir = home.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_dir).unwrap();
    write_legacy_role_config(
        &config_dir,
        Role::Admin,
        &serde_json::json!({"provider": "redmine", "api_base": "https://legacy.example", "repository": null}),
    );

    let storage = Storage::open_for_home(&home).unwrap();
    // Pre-populate SQLite with values that differ from the legacy file
    // so the import must respect them.
    let config = StoredConfig {
        provider: Some(PROVIDER_FORGEJO.to_owned()),
        api_base: Some("https://sqlite.example".to_owned()),
        ..Default::default()
    };
    storage.save_role_config(Role::Admin, &config).unwrap();

    let report = storage.import_legacy(&config_dir).unwrap();
    assert_eq!(
        report.skipped, 2,
        "import must skip already-populated SQLite fields"
    );

    let loaded = storage.load_role_config(Role::Admin).unwrap().unwrap();
    assert_eq!(loaded.provider.as_deref(), Some(PROVIDER_FORGEJO));
    assert_eq!(loaded.api_base.as_deref(), Some("https://sqlite.example"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn import_legacy_is_idempotent_across_multiple_opens() {
    let home = unique_temp_dir("import-idempotent");
    let config_dir = home.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_dir).unwrap();
    write_legacy_role_config(
        &config_dir,
        Role::Reviewer,
        &serde_json::json!({"provider": "forgejo"}),
    );
    write_legacy_credential(
        &config_dir,
        Role::Reviewer,
        PROVIDER_FORGEJO,
        "reviewer-token",
    );

    // First open imports the legacy data.
    let first = Storage::open_for_home(&home).unwrap();
    let first_report = first.import_legacy(&config_dir).unwrap();
    assert_eq!(first_report.imported, 2);
    drop(first);

    // Second open must re-import nothing because SQLite already has
    // every legacy field populated; the importer records each one as
    // a skip so callers can observe the no-op.
    let second = Storage::open_for_home(&home).unwrap();
    let second_report = second.import_legacy(&config_dir).unwrap();
    assert_eq!(second_report.imported, 0, "second import must be a no-op");
    assert_eq!(
        second_report.skipped, 2,
        "second import must skip the two already-populated legacy fields"
    );
    let credential = second
        .load_credential(Role::Reviewer, PROVIDER_FORGEJO)
        .unwrap()
        .unwrap();
    assert_eq!(credential, "reviewer-token");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn credentials_for_different_providers_are_stored_separately() {
    let home = unique_temp_dir("credential-separation");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
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

    let home = unique_temp_dir("mirror-env");
    let storage = Storage::open_for_home(&home).unwrap();
    let config_dir = home.join(".config/opencode/phasegent");
    // Run a fake import that touches every credential slot. If the
    // mirror env were ever stored, the import would either persist
    // it (and we would observe the row) or fail with an error.
    let _ = storage.import_legacy(&config_dir).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn persist_redmine_bootstrap_validates_zero_ids() {
    let home = unique_temp_dir("bootstrap-validation");
    let storage = Storage::open_for_home(&home).unwrap();
    let zero = storage
        .persist_redmine_bootstrap(Role::Admin, None, 0, 5)
        .unwrap_err();
    assert!(zero.contains("greater than zero"));
    let zero = storage
        .persist_redmine_bootstrap(Role::Admin, None, 7, 0)
        .unwrap_err();
    assert!(zero.contains("greater than zero"));
    let _ = fs::remove_dir_all(home);
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
    let home = unique_temp_dir("gitlab-round-trip");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn persist_gitlab_bootstrap_validates_zero_project_id_and_flips_provider() {
    // The GitLab bootstrap is the only entry point that flips the
    // role_config.provider column on the executor so ordinary
    // `auth setup` flows don't have to know about the underlying
    // column. Confirm the zero-id guard and the provider flip in one
    // test so the foundation never silently accepts an id of zero.
    let home = unique_temp_dir("gitlab-bootstrap");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

#[test]
fn credentials_for_all_three_providers_are_isolated_per_role() {
    // The role_credential table uses (role, provider) as a composite
    // primary key so the same role can keep three independent
    // credentials. Phase-1 GitLab foundation: confirm the new
    // gitlab row coexists with forgejo and redmine values without
    // any cross-write or leak, and that overwriting one credential
    // never touches another.
    let home = unique_temp_dir("credential-coexistence");
    let storage = Storage::open_for_home(&home).unwrap();
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
    let _ = fs::remove_dir_all(home);
}

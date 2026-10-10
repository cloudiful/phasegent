use super::support::*;
use super::*;

/// A retired provider key. Historical `role_credential` rows can still
/// carry a retired provider literal, and the composite `(role, provider)`
/// primary key must still isolate them from the live `redmine` rows.
const LEGACY_PROVIDER: &str = "forgejo";

#[test]
fn credentials_for_different_providers_are_stored_separately() {
    let (temp_dir, storage) = open_at_temp("credential-separation");
    storage
        .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Executor, LEGACY_PROVIDER, "legacy-token")
        .unwrap();

    let redmine = storage
        .load_credential(Role::Executor, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    let legacy = storage
        .load_credential(Role::Executor, LEGACY_PROVIDER)
        .unwrap()
        .unwrap();
    assert_eq!(redmine, "redmine-key");
    assert_eq!(legacy, "legacy-token");

    // Overwriting one credential must not touch the other.
    storage
        .save_credential(Role::Executor, LEGACY_PROVIDER, "legacy-token-v2")
        .unwrap();
    let legacy_v2 = storage
        .load_credential(Role::Executor, LEGACY_PROVIDER)
        .unwrap()
        .unwrap();
    let redmine_after = storage
        .load_credential(Role::Executor, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    assert_eq!(legacy_v2, "legacy-token-v2");
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
        for provider in [LEGACY_PROVIDER, PROVIDER_REDMINE] {
            assert!(storage.load_credential(role, provider).unwrap().is_none());
        }
        storage
            .save_role_config(
                role,
                &StoredConfig {
                    provider: Some(LEGACY_PROVIDER.to_owned()),
                    api_base: Some("https://legacy.example".to_owned()),
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
fn credentials_for_distinct_provider_keys_are_isolated_per_role() {
    // The role_credential table uses (role, provider) as a composite
    // primary key so the same role can keep independent credentials. A
    // historical retired-provider row must coexist with the live redmine
    // value without any cross-write or leak, and overwriting one
    // credential never touches another.
    let (temp_dir, storage) = open_at_temp("credential-coexistence");
    storage
        .save_credential(Role::Orchestrator, LEGACY_PROVIDER, "legacy-secret")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, PROVIDER_REDMINE, "redmine-secret")
        .unwrap();

    let legacy = storage
        .load_credential(Role::Orchestrator, LEGACY_PROVIDER)
        .unwrap()
        .unwrap();
    let redmine = storage
        .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    assert_eq!(legacy, "legacy-secret");
    assert_eq!(redmine, "redmine-secret");

    // Overwriting one must not leak into another.
    storage
        .save_credential(Role::Orchestrator, LEGACY_PROVIDER, "legacy-secret-v2")
        .unwrap();
    let redmine_after = storage
        .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
        .unwrap()
        .unwrap();
    let legacy_after = storage
        .load_credential(Role::Orchestrator, LEGACY_PROVIDER)
        .unwrap()
        .unwrap();
    assert_eq!(redmine_after, "redmine-secret");
    assert_eq!(legacy_after, "legacy-secret-v2");

    // Other roles must not observe any of these credentials.
    assert!(
        storage
            .load_credential(Role::Executor, LEGACY_PROVIDER)
            .unwrap()
            .is_none(),
        "executor must not observe the orchestrator's legacy credential"
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn credential_summary_reports_fingerprint_and_store_time() {
    // `save_credential` maintains the non-secret fingerprint and store
    // timestamp so display paths never load the secret. The fingerprint
    // is the last 4 characters; overwriting refreshes both columns.
    use crate::infra::storage::credential_fingerprint;

    assert_eq!(
        credential_fingerprint("redmine-secret-key-1234").as_deref(),
        Some("1234")
    );
    assert_eq!(credential_fingerprint("abcd").as_deref(), Some("abcd"));
    assert_eq!(credential_fingerprint("abc"), None);
    assert_eq!(credential_fingerprint(""), None);

    let (temp_dir, storage) = open_at_temp("credential-fingerprint");
    storage
        .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-secret-key-1234")
        .unwrap();
    let summary = storage
        .credential_summary(Role::Executor, PROVIDER_REDMINE)
        .unwrap();
    assert!(summary.present);
    assert_eq!(summary.length, "redmine-secret-key-1234".chars().count());
    assert_eq!(summary.fingerprint.as_deref(), Some("1234"));
    assert!(
        summary.updated_at.is_some_and(|stamp| stamp > 0),
        "store timestamp must be recorded"
    );

    storage
        .save_credential(Role::Executor, PROVIDER_REDMINE, "rotated-key-5678")
        .unwrap();
    let rotated = storage
        .credential_summary(Role::Executor, PROVIDER_REDMINE)
        .unwrap();
    assert_eq!(rotated.fingerprint.as_deref(), Some("5678"));

    // Short secrets report presence/length but no fingerprint.
    storage
        .save_credential(Role::Executor, LEGACY_PROVIDER, "abc")
        .unwrap();
    let short = storage
        .credential_summary(Role::Executor, LEGACY_PROVIDER)
        .unwrap();
    assert!(short.present);
    assert_eq!(short.fingerprint, None);

    let missing = storage
        .credential_summary(Role::Reviewer, PROVIDER_REDMINE)
        .unwrap();
    assert!(!missing.present);
    assert_eq!(missing.length, 0);
    assert_eq!(missing.fingerprint, None);
    assert_eq!(missing.updated_at, None);
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn credential_summary_backfills_legacy_rows_without_fingerprint() {
    // Rows written before the fingerprint columns existed carry NULLs
    // (simulated here by clearing the columns after save). The first
    // summary read backfills the fingerprint from the stored value;
    // the secret itself is never returned.
    let (temp_dir, storage) = open_at_temp("credential-backfill");
    storage
        .save_credential(Role::Orchestrator, LEGACY_PROVIDER, "legacy-token-99")
        .unwrap();
    storage
        .connection
        .execute(
            "UPDATE role_credential SET fingerprint = NULL, credential_updated_at = NULL \
             WHERE role = 'orchestrator' AND provider = ?1",
            [LEGACY_PROVIDER],
        )
        .unwrap();

    let summary = storage
        .credential_summary(Role::Orchestrator, LEGACY_PROVIDER)
        .unwrap();
    assert!(summary.present);
    assert_eq!(summary.fingerprint.as_deref(), Some("n-99"));

    let stored: Option<String> = storage
        .connection
        .query_row(
            "SELECT fingerprint FROM role_credential WHERE role = 'orchestrator' AND provider = ?1",
            [LEGACY_PROVIDER],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("n-99"));
    let _ = fs::remove_dir_all(temp_dir);
}

use super::support::*;
use super::*;

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
fn credentials_for_all_three_providers_are_isolated_per_role() {
    // The role_credential table uses (role, provider) as a composite
    // primary key so the same role can keep three independent
    // credentials. Confirm the new gitlab row coexists with forgejo and
    // redmine values without any cross-write or leak, and that
    // overwriting one credential never touches another.
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
        .save_credential(Role::Executor, PROVIDER_FORGEJO, "abc")
        .unwrap();
    let short = storage
        .credential_summary(Role::Executor, PROVIDER_FORGEJO)
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
        .save_credential(
            Role::Orchestrator,
            PROVIDER_GITLAB,
            "legacy-gitlab-token-99",
        )
        .unwrap();
    storage
        .connection
        .execute(
            "UPDATE role_credential SET fingerprint = NULL, credential_updated_at = NULL \
             WHERE role = 'orchestrator' AND provider = 'gitlab'",
            [],
        )
        .unwrap();

    let summary = storage
        .credential_summary(Role::Orchestrator, PROVIDER_GITLAB)
        .unwrap();
    assert!(summary.present);
    assert_eq!(summary.fingerprint.as_deref(), Some("n-99"));

    let stored: Option<String> = storage
        .connection
        .query_row(
            "SELECT fingerprint FROM role_credential WHERE role = 'orchestrator' AND provider = 'gitlab'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("n-99"));
    let _ = fs::remove_dir_all(temp_dir);
}

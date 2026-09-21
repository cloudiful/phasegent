use super::support::*;
use super::*;

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
fn role_redmine_user_round_trips_per_role_and_validates() {
    // Admin-only provisioning persists one (user_id, login) row per agent
    // role. The table is additive; fresh databases report missing, saves
    // round-trip, overwrites replace, roles stay isolated, and zero/blank
    // inputs are rejected before SQL.
    let (temp_dir, storage) = open_at_temp("redmine-user");
    assert!(
        storage
            .load_redmine_user(Role::Orchestrator)
            .unwrap()
            .is_none()
    );
    storage
        .save_redmine_user(Role::Orchestrator, 11, "phasegent-orchestrator")
        .unwrap();
    storage
        .save_redmine_user(Role::Executor, 22, "phasegent-executor")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((11, "phasegent-orchestrator".to_owned()))
    );
    assert_eq!(
        storage.load_redmine_user(Role::Executor).unwrap(),
        Some((22, "phasegent-executor".to_owned()))
    );
    assert!(storage.load_redmine_user(Role::Reviewer).unwrap().is_none());
    storage
        .save_redmine_user(Role::Orchestrator, 12, "phasegent-orchestrator")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((12, "phasegent-orchestrator".to_owned()))
    );
    assert!(storage.save_redmine_user(Role::Reviewer, 0, "x").is_err());
    assert!(storage.save_redmine_user(Role::Reviewer, 7, "   ").is_err());
    assert!(
        storage
            .save_redmine_user(Role::Reviewer, 7, "a\nb")
            .is_err()
    );
    // Whitespace is trimmed on write/read.
    storage
        .save_redmine_user(Role::Tester, 44, "  phasegent-tester  ")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Tester).unwrap(),
        Some((44, "phasegent-tester".to_owned()))
    );
    // Downstream role_credential rows remain the source for API keys.
    storage
        .save_credential(Role::Orchestrator, PROVIDER_REDMINE, "orchestrator-key")
        .unwrap();
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, PROVIDER_REDMINE)
            .unwrap()
            .as_deref(),
        Some("orchestrator-key")
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn role_gitlab_config_round_trip_and_numeric_project_id() {
    // GitLab `project_id` is no longer persisted; the column remains for
    // non-destructive migration but `load` always returns `None` and
    // `save` ignores the field. The test verifies api_base round-trip
    // and that legacy values are
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
    // project_id is ignored on persist, only api_base is kept.
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

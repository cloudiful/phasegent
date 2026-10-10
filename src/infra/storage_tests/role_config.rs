use super::support::*;
use super::*;

#[test]
fn save_role_config_distinguishes_missing_from_empty() {
    let (temp_dir, storage) = open_at_temp("save-empty");
    let config = StoredConfig {
        provider: Some(PROVIDER_REDMINE.to_owned()),
        ..Default::default()
    };
    storage.save_role_config(Role::Admin, &config).unwrap();
    let loaded = storage
        .load_role_config(Role::Admin)
        .unwrap()
        .expect("row must exist after save");
    assert_eq!(loaded.provider.as_deref(), Some(PROVIDER_REDMINE));
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
        .save_redmine_user(Role::Reviewer, 44, "  phasegent-reviewer  ")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Reviewer).unwrap(),
        Some((44, "phasegent-reviewer".to_owned()))
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
fn legacy_provider_literals_remain_inert_in_role_config() {
    // Historical provider rows can still exist in an operator database. The
    // storage layer keeps them decodable and inert: the resolver rejects a
    // retired literal before any network access, and nothing here rewrites
    // or deletes the row.
    let (temp_dir, storage) = open_at_temp("legacy-provider");
    let config = StoredConfig {
        provider: Some("forgejo".to_owned()),
        ..Default::default()
    };
    storage.save_role_config(Role::Executor, &config).unwrap();
    let loaded = storage.load_role_config(Role::Executor).unwrap().unwrap();
    assert_eq!(loaded.provider.as_deref(), Some("forgejo"));
    let _ = fs::remove_dir_all(temp_dir);
}

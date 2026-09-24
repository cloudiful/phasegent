use super::support::*;
use super::*;

#[test]
fn config_provider_get_parses_without_role() {
    let args = ["config", "provider", "get"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config provider get without a role must parse");
    match invocation.command {
        Command::ConfigProviderGet => {}
        other => panic!("expected ConfigProviderGet, got {other:?}"),
    }
}

#[test]
fn config_provider_get_parses_with_role() {
    let args = ["config", "provider", "get"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("executor"))
        .expect("config provider get with a role must parse");
    match invocation.command {
        Command::ConfigProviderGet => {}
        other => panic!("expected ConfigProviderGet, got {other:?}"),
    }
}

#[test]
fn config_provider_set_parses_valid_values() {
    for (raw, expected) in [
        ("forgejo", crate::providers::ProviderKind::Forgejo),
        ("redmine", crate::providers::ProviderKind::Redmine),
        ("gitlab", crate::providers::ProviderKind::Gitlab),
        ("local", crate::providers::ProviderKind::Local),
    ] {
        let args = ["admin", "config", "provider", "set", raw]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let invocation = command::parse(&args)
            .unwrap_or_else(|error| panic!("config provider set {raw} must parse: {error}"));
        match invocation.command {
            Command::ConfigProviderSet { value } => assert_eq!(value, expected),
            other => panic!("expected ConfigProviderSet, got {other:?}"),
        }
    }
}

#[test]
fn config_provider_set_rejects_unknown_value() {
    let args = ["admin", "config", "provider", "set", "wrong"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown provider value must error");
    assert!(
        error.contains("config provider set"),
        "error must name the operation: {error}"
    );
    assert!(
        error.contains("wrong"),
        "error must echo the offending value: {error}"
    );
}

#[test]
fn config_provider_set_rejects_missing_value() {
    let args = ["admin", "config", "provider", "set"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("missing value must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_set_rejects_extra_arguments() {
    let args = ["admin", "config", "provider", "set", "redmine", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("extra arguments must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_clear_parses_without_role() {
    let args = ["admin", "config", "provider", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation =
        command::parse(&args).expect("config provider clear without a role must parse");
    match invocation.command {
        Command::ConfigProviderClear => {}
        other => panic!("expected ConfigProviderClear, got {other:?}"),
    }
}

#[test]
fn config_provider_clear_rejects_extra_arguments() {
    let args = ["admin", "config", "provider", "clear", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("extra arguments must error");
    assert!(error.contains("no arguments"), "got: {error}");
}

#[test]
fn config_provider_unknown_subcommand_is_rejected() {
    let args = ["config", "provider", "purge"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown config provider subcommand must error");
    assert!(
        error.contains("unknown config provider command"),
        "got: {error}"
    );
}

#[test]
fn storage_global_default_provider_save_load_and_delete_round_trip() {
    with_isolated_storage("global-default-crud", |_db_path, storage| {
        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "fresh storage must report an absent default"
        );
        assert!(
            !storage
                .delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap(),
            "delete on an absent row must return false"
        );

        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE)
            .unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .as_deref(),
            Some(PROVIDER_REDMINE)
        );

        assert!(
            storage
                .delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap(),
            "delete on an existing row must return true"
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "delete must leave the row absent"
        );
    });
}

#[test]
fn config_provider_set_get_and_clear_round_trip_through_helpers() {
    with_isolated_storage("global-default-helpers", |_db_path, storage| {
        let initial = config::provider_get(storage).unwrap();
        assert!(
            initial.provider.is_none(),
            "fresh storage must report null default: {initial:?}"
        );

        for literal in [PROVIDER_FORGEJO, PROVIDER_REDMINE, PROVIDER_GITLAB] {
            let outcome = config::provider_set(literal, storage).unwrap();
            assert_eq!(outcome.provider, Some(literal));
            let stored = config::provider_get(storage).unwrap();
            assert_eq!(stored.provider, Some(literal));
        }

        let cleared = config::provider_clear(storage).unwrap();
        assert!(cleared.cleared, "first clear must remove the row");
        let cleared_again = config::provider_clear(storage).unwrap();
        assert!(!cleared_again.cleared, "second clear must be a no-op");
        assert!(
            config::provider_get(storage).unwrap().provider.is_none(),
            "clear must leave the default unset"
        );
    });
}

#[test]
fn config_provider_set_helper_rejects_unknown_value() {
    with_isolated_storage("global-default-invalid", |_db_path, storage| {
        let error = config::provider_set("wrong", storage).unwrap_err();
        assert!(
            error.contains("invalid provider"),
            "error must name the offending value: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the value: {error}"
        );

        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "rejected set must not persist anything"
        );
    });
}

#[test]
fn config_provider_get_rejects_stale_invalid_row() {
    with_isolated_storage("global-default-stale", |_db_path, storage| {
        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "wrong")
            .unwrap();

        let error = config::provider_get(storage).unwrap_err();
        assert!(
            error.contains("persisted PHASEGENT_DEFAULT_PROVIDER is invalid"),
            "error must identify the persisted row: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the offending value: {error}"
        );
    });
}

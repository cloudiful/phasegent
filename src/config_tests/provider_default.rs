use crate::command::{self, Command};

#[test]
fn config_provider_get_parses_without_role() {
    let args = ["config", "provider", "get"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("config provider get without a role must parse");
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
        ("redmine", crate::providers::ProviderKind::Redmine),
        ("local", crate::providers::ProviderKind::Local),
        ("forgejo", crate::providers::ProviderKind::Forgejo),
        ("gitlab", crate::providers::ProviderKind::Gitlab),
    ] {
        let args = ["admin", "config", "provider", "set", raw]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let invocation = command::parse_with_role_env(&args, None)
            .unwrap_or_else(|error| panic!("config provider set {raw} must parse: {error}"));
        match invocation.command {
            Command::ConfigProviderSet { value } => assert_eq!(value, expected),
            other => panic!("expected ConfigProviderSet, got {other:?}"),
        }
    }
}

#[test]
fn config_provider_set_accepts_gitlab_as_the_fourth_provider() {
    let args = ["admin", "config", "provider", "set", "gitlab"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("admin"))
        .expect("gitlab provider must be accepted");
    match invocation.command {
        Command::ConfigProviderSet { value } => {
            assert_eq!(value, crate::providers::ProviderKind::Gitlab)
        }
        other => panic!("expected ConfigProviderSet, got {other:?}"),
    }
}

#[test]
fn config_provider_set_rejects_unknown_value() {
    let args = ["admin", "config", "provider", "set", "wrong"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error =
        command::parse_with_role_env(&args, None).expect_err("unknown provider value must error");
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
    let error = command::parse_with_role_env(&args, None).expect_err("missing value must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_set_rejects_extra_arguments() {
    let args = ["admin", "config", "provider", "set", "redmine", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, None).expect_err("extra arguments must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_clear_parses_without_role() {
    let args = ["admin", "config", "provider", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("config provider clear without a role must parse");
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
    let error = command::parse_with_role_env(&args, None).expect_err("extra arguments must error");
    assert!(error.contains("no arguments"), "got: {error}");
}

#[test]
fn config_provider_unknown_subcommand_is_rejected() {
    let args = ["config", "provider", "purge"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, None)
        .expect_err("unknown config provider subcommand must error");
    assert!(
        error.contains("unknown config provider command"),
        "got: {error}"
    );
}

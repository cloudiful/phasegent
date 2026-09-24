use super::support::*;
use super::*;

#[test]
fn config_set_parses_canonical_and_kebab_alias() {
    // Canonical and kebab-case alias must both be accepted and resolve to same canonical.
    // Project-id aliases were removed; they are asserted as rejected in
    // the dedicated regression test below.
    let cases = [
        ("PHASEGENT_API_BASE", "api-base"),
        (
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "redmine-git-mirror-api-key",
        ),
        ("PHASEGENT_DEFAULT_PROVIDER", "default-provider"),
        ("PHASEGENT_GITLAB_API_BASE", "gitlab-api-base"),
    ];
    for (canonical, alias) in cases {
        for name in [canonical, alias] {
            let is_secret = config_write::is_secret_setting(canonical);
            let mut args = vec![
                "admin".to_owned(),
                "config".to_owned(),
                "set".to_owned(),
                name.to_owned(),
            ];
            if is_secret {
                args.push("--stdin".to_owned());
            } else {
                args.push("test-value".to_owned());
            }
            let invocation = command::parse_with_role_env(&args, Some("executor"))
                .unwrap_or_else(|e| panic!("set {name} must parse: {e}"));
            match invocation.command {
                Command::ConfigSet { setting, .. } => assert_eq!(setting, canonical),
                other => panic!("expected ConfigSet for {name}, got {other:?}"),
            }
        }
    }
}

#[test]
fn config_set_rejects_legacy_project_id_aliases() {
    // Project-id persistence removed. The canonical names and the
    // ambiguous alias must be rejected as unknown settings at parse
    // time and via the config_write dispatch.
    for alias in [
        "PHASEGENT_REDMINE_PROJECT_ID",
        "redmine-project-id",
        "PHASEGENT_GITLAB_PROJECT_ID",
        "gitlab-project-id",
        "PHASEGENT_PROJECT_ID",
        "project-id",
        "project_id",
    ] {
        assert!(
            config_write::canonical_setting_name(alias).is_none(),
            "alias '{alias}' must be unknown after Phase 1"
        );
        let args = ["admin", "config", "set", alias, "42"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let error = command::parse_with_role_env(&args, Some("executor"))
            .expect_err("project-id alias must be rejected");
        assert!(error.contains("unknown config setting"), "got: {error}");
        let clear_args = ["admin", "config", "clear", alias]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let clear_error = command::parse_with_role_env(&clear_args, Some("executor"))
            .expect_err("clear project-id must be rejected");
        assert!(
            clear_error.contains("unknown config setting"),
            "got: {clear_error}"
        );
    }
    // Direct dispatch must also reject unknown canonicals.
    with_isolated_storage("project-id-rejected-dispatch", |_db_path, storage| {
        for canonical in [
            "PHASEGENT_REDMINE_PROJECT_ID",
            "PHASEGENT_GITLAB_PROJECT_ID",
            "PHASEGENT_PROJECT_ID",
        ] {
            let err =
                config_write::set_setting_value(Some(Role::Executor), canonical, "42", storage)
                    .unwrap_err();
            assert!(err.contains("unknown setting"), "got: {err}");
            let clear_err =
                config_write::clear_setting(Some(Role::Executor), canonical, storage).unwrap_err();
            assert!(clear_err.contains("unknown setting"), "got: {clear_err}");
        }
    });
}

#[test]
fn config_set_global_without_role_parses() {
    // Global settings must be usable without a role.
    let args = [
        "admin",
        "config",
        "set",
        "redmine-git-mirror-api-key",
        "--stdin",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation =
        command::parse_with_role_env(&args, None).expect("global set without a role must parse");
    match invocation.command {
        Command::ConfigSet { setting, stdin, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
            assert!(stdin);
        }
        other => panic!("expected ConfigSet global without role, got {other:?}"),
    }
    let args = [
        "admin",
        "config",
        "set",
        "redmine-repository-url",
        "https://example.com",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("global set url without a role must parse");
    match invocation.command {
        Command::ConfigSet { setting, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_REPOSITORY_URL")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn config_set_role_scoped_requires_role() {
    let args = ["admin", "config", "set", "api-base", "https://example.com"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, None)
        .expect_err("role-scoped set without a role must error");
    assert!(error.contains("a role is required"), "got: {error}");
}

#[test]
fn config_set_rejects_secret_direct_value() {
    let args = [
        "config",
        "set",
        "redmine-git-mirror-api-key",
        "direct-secret-value",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("secret direct value must be rejected");
    assert!(
        error.contains("does not accept a direct value"),
        "got: {error}"
    );
    assert!(
        !error.contains("direct-secret-value"),
        "error must not echo secret: {error}"
    );
}

#[test]
fn config_set_rejects_unknown_setting() {
    let args = ["config", "set", "unknown-setting", "value"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("unknown setting must error");
    assert!(error.contains("unknown config setting"), "got: {error}");
    assert!(error.contains("unknown-setting"), "got: {error}");
}

#[test]
fn config_set_rejects_missing_value_for_non_secret() {
    let args = ["admin", "config", "set", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("missing value must error");
    assert!(error.contains("requires a value"), "got: {error}");
}

#[test]
fn config_set_rejects_empty_value() {
    with_isolated_storage("set-empty", |_db_path, storage| {
        let err = config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_API_BASE",
            "   ",
            storage,
        )
        .unwrap_err();
        assert!(err.contains("cannot be empty"), "got: {err}");
        let err = config_write::set_setting_stdin_content(
            None,
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "   ",
            storage,
        )
        .unwrap_err();
        assert!(err.contains("cannot be empty"), "got: {err}");
        assert!(!err.contains("shhh"), "secret leaked");
    });
}

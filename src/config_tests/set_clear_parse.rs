use crate::command::{self, Command};
use crate::config_write;

#[test]
fn config_set_parses_canonical_and_kebab_alias() {
    // Canonical and kebab-case alias must both be accepted and resolve to same canonical.
    // Project-id aliases were removed; they are asserted as rejected in
    // the dedicated regression test below.
    let cases = [
        (
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "redmine-git-mirror-api-key",
        ),
        ("PHASEGENT_DEFAULT_PROVIDER", "default-provider"),
        ("PHASEGENT_REDMINE_API_BASE", "redmine-api-base"),
        (
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
            "redmine-close-status-id",
        ),
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
            let invocation = command::parse_with_role_env(&args, Some("admin"))
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
    // The canonical global Redmine address is machine-wide too.
    let args = [
        "admin",
        "config",
        "set",
        "redmine-api-base",
        "https://redmine.example",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("global redmine api base without a role must parse");
    match invocation.command {
        Command::ConfigSet { setting, .. } => assert_eq!(setting, "PHASEGENT_REDMINE_API_BASE"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn config_set_role_scoped_requires_role() {
    // Unmanaged Operator parses role-scoped writes (execution uses the admin
    // scope centrally); a managed session without a role fails closed.
    let args = ["admin", "config", "set", "redmine-close-status-id", "5"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation =
        command::parse_with_role_env(&args, None).expect("operator role-scoped set must parse");
    assert_eq!(invocation.caller, crate::policy::CallerContext::Operator);
    let error = command::parse_with_caller_env(&args, None, true)
        .expect_err("managed role-scoped set without a role must error");
    assert!(
        error.contains("managed session requires a role"),
        "got: {error}"
    );
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
    let args = ["admin", "config", "set", "redmine-close-status-id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("missing value must error");
    assert!(error.contains("requires a value"), "got: {error}");
}

#[test]
fn config_clear_command_parsing() {
    let args = ["admin", "config", "clear", "redmine-git-mirror-api-key"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, None).expect("clear global without role must parse");
    match inv.command {
        Command::ConfigClear { setting, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        }
        other => panic!("got {other:?}"),
    }
    let args = ["admin", "config", "clear", "redmine-close-status-id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, None).expect("operator role-scoped clear must parse");
    match inv.command {
        Command::ConfigClear { setting, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        }
        other => panic!("got {other:?}"),
    }
    let err = command::parse_with_caller_env(&args, None, true)
        .expect_err("managed role-scoped clear without role must error");
    assert!(
        err.contains("managed session requires a role"),
        "got: {err}"
    );

    let args = ["admin", "config", "clear", "redmine-close-status-id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, Some("admin")).expect("clear with role must parse");
    match inv.command {
        Command::ConfigClear { setting, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        }
        other => panic!("got {other:?}"),
    }

    let args = ["admin", "config", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("clear without setting must error");
    assert!(err.contains("requires a setting"), "got: {err}");

    let args = ["admin", "config", "clear", "unknown"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("unknown clear setting must error");
    assert!(err.contains("unknown config setting"), "got: {err}");
}

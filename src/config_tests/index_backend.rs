use crate::command::{self, Command};

#[test]
fn index_backend_alias_and_validation() {
    for alias in [
        "PHASEGENT_INDEX_BACKEND",
        "index-backend",
        "INDEX_BACKEND",
        "index_backend",
        "PHASEGENT_index_backend",
    ] {
        let canonical = crate::config_write::canonical_setting_name(alias)
            .unwrap_or_else(|| panic!("alias {alias} must resolve"));
        assert_eq!(canonical, "PHASEGENT_INDEX_BACKEND");
    }
    for alias in [
        "PHASEGENT_INDEX_PG_URL",
        "index-pg-url",
        "INDEX_PG_URL",
        "index_pg_url",
    ] {
        let canonical = crate::config_write::canonical_setting_name(alias)
            .unwrap_or_else(|| panic!("alias {alias} must resolve"));
        assert_eq!(canonical, "PHASEGENT_INDEX_PG_URL");
    }
    assert!(crate::config_write::is_global_setting(
        "PHASEGENT_INDEX_BACKEND"
    ));
    assert!(crate::config_write::is_global_setting(
        "PHASEGENT_INDEX_PG_URL"
    ));
    assert!(!crate::config_write::is_secret_setting(
        "PHASEGENT_INDEX_BACKEND"
    ));
    assert!(crate::config_write::is_secret_setting(
        "PHASEGENT_INDEX_PG_URL"
    ));

    let args = ["admin", "config", "set", "index-backend", "postgres"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse_with_role_env(&args, None)
        .expect("index-backend set without role must parse");
    match inv.command {
        Command::ConfigSet { setting, .. } => assert_eq!(setting, "PHASEGENT_INDEX_BACKEND"),
        other => panic!("unexpected {other:?}"),
    }
    let args = ["admin", "config", "set", "index-pg-url", "--stdin"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse_with_role_env(&args, None)
        .expect("index-pg-url --stdin without role must parse");
    match inv.command {
        Command::ConfigSet { setting, stdin, .. } => {
            assert_eq!(setting, "PHASEGENT_INDEX_PG_URL");
            assert!(stdin);
        }
        other => panic!("unexpected {other:?}"),
    }
    let args = [
        "admin",
        "config",
        "set",
        "index-pg-url",
        "postgres://example",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, None)
        .expect_err("direct secret value must be rejected");
    assert!(err.contains("does not accept a direct value"), "got: {err}");
    assert!(
        !err.contains("postgres://example"),
        "error must not echo secret"
    );
}

#[test]
fn removed_index_commands_and_help_topics_are_rejected() {
    // `issue index sync/search` were removed; ordinary `issue search` is the
    // only documented workflow with automatic warm/fallback. The removed
    // command surface must fail at the CLI and under `--help`, and the
    // overview must not advertise a topic whose name starts with `index`.
    for args in [
        vec!["issue", "index", "sync", "--query", "bug"],
        vec!["issue", "index", "search", "--query", "hello"],
        vec!["issue", "index"],
    ] {
        let parsed: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let error = command::parse_with_role_env(&parsed, Some("executor"))
            .expect_err("removed index command must be rejected");
        assert!(
            error.contains("unknown issue command") && error.contains("index"),
            "got: {error}"
        );
    }
    for topic in ["index", "index sync", "index search"] {
        let mut parts = vec!["--help".to_owned(), "issue".to_owned()];
        parts.extend(topic.split_whitespace().map(str::to_owned));
        let error = command::parse_with_role_env(&parts, Some("executor"))
            .err()
            .unwrap_or_else(|| panic!("help {topic} must be rejected"));
        assert!(
            error.contains("unknown issue help topic"),
            "help {topic} must be rejected as unknown help topic, got: {error}"
        );
    }
    let args = ["issue", "search", "--query", "phase"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("ordinary search must parse");
    match invocation.command {
        Command::Issue(crate::command::IssueCommand::Search { query, .. }) => {
            assert_eq!(query.as_deref(), Some("phase"));
        }
        other => panic!("expected Search, got {other:?}"),
    }
}

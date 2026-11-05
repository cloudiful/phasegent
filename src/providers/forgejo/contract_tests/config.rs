use super::super::{ForgejoProvider, config::ForgejoConfig, http::ForgejoHttp};
use super::support::server;
use crate::infra::storage::Storage;
use crate::policy::Role;
use reqwest::Method;

#[test]
fn forgejo_index_scope_uses_validated_repository_without_network() {
    use crate::providers::index_store::{explicit_scope, provider_scope};
    use crate::providers::{ProviderDispatcher, ProviderKind};

    let config = ForgejoConfig::new("https://example.test", "owner/repo").unwrap();
    let provider = ProviderDispatcher::Forgejo(ForgejoProvider::new(config, "fixture").unwrap());
    let scope = provider_scope(&provider).unwrap();
    assert_eq!(scope.source, "forgejo");
    assert_eq!(scope.project, "owner/repo");
    let explicit = explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo"), None).unwrap();
    assert_eq!(explicit.project, "owner/repo");
    assert!(explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo/extra"), None).is_none());
    assert!(explicit_scope(Some(ProviderKind::Forgejo), None, None).is_none());
}

#[test]
fn forgejo_planning_and_relations_reject_before_network() {
    use crate::command::{AssigneeOption, PlanningOptions, RelationCommand};
    use crate::providers::{
        ProviderDispatcher,
        redmine::{planning, relations},
    };

    let config = ForgejoConfig::new("https://example.test", "owner/repo").unwrap();
    let provider = ProviderDispatcher::Forgejo(ForgejoProvider::new(config, "fixture").unwrap());
    let planning_options = PlanningOptions {
        parent_issue: Some("3".into()),
        ..Default::default()
    };
    assert!(
        planning::create_issue(
            &provider,
            "title",
            "body",
            None,
            &planning_options,
            &AssigneeOption::Unset
        )
        .is_err()
    );
    assert!(
        planning::update_body(
            &provider,
            7,
            "body",
            Some("Bug"),
            &PlanningOptions::default()
        )
        .is_err()
    );
    assert!(
        relations::execute(
            &provider,
            &RelationCommand::List {
                issue: 7,
                page: None
            }
        )
        .is_err()
    );
}

#[test]
fn forgejo_timer_commands_and_hooks_skip_ledger() {
    use crate::command::TimerCommand;
    use crate::lifecycle_auto::{
        AutoCloseOutcome, AutoTimerOutcome, auto_close_issue_timer, auto_transition_timer,
    };
    use crate::providers::ProviderKind;

    let commands = [
        TimerCommand::Start {
            issue: 7,
            phase: "P1".into(),
            agent_role: "executor".into(),
            attempt: 1,
            run_id: None,
            owner_session_id: None,
            owner_call_id: None,
        },
        TimerCommand::Finish {
            run_id: "missing".into(),
            result: "DONE".into(),
        },
    ];
    for command in commands {
        let error = crate::time_tracking::execute(
            Some(Role::Orchestrator),
            Some(ProviderKind::Forgejo),
            None,
            None,
            None,
            command,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not_supported"), "{error}");
    }
    let finish = crate::time_tracking::finish::execute_finish(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        "missing",
        "DONE",
    )
    .unwrap_err();
    assert!(finish.to_string().contains("not_supported"));
    let recover = crate::time_tracking::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        TimerCommand::Recover {
            run_id: "missing".into(),
        },
    )
    .unwrap_err();
    assert!(recover.to_string().contains("not_supported"));
    assert!(matches!(
        auto_transition_timer(7, ProviderKind::Forgejo, "In Progress"),
        AutoTimerOutcome::Skipped
    ));
    assert!(matches!(
        auto_close_issue_timer(7, ProviderKind::Forgejo),
        AutoCloseOutcome::Noop { .. }
    ));
}

#[test]
fn requires_explicit_scope_and_validates_it() {
    for repo in [
        " owner/repo",
        "owner/repo ",
        "owner/ repo",
        "owner/repo/extra",
        "https://host/repo",
        "owner/..",
        "owner/a\nb",
    ] {
        assert!(
            ForgejoConfig::new("https://example.test", repo).is_err(),
            "{repo:?}"
        );
    }
    for base in [
        "ftp://example.test",
        "https://u:p@example.test",
        "https://example.test/?q=1",
        "https://example.test/#fragment",
    ] {
        assert!(ForgejoConfig::new(base, "owner/repo").is_err(), "{base:?}");
    }
    let config = ForgejoConfig::new("https://example.test/prefix/", "owner/repo").unwrap();
    assert_eq!(config.api_base, "https://example.test/prefix");
    assert_eq!(config.owner(), "owner");
    assert_eq!(config.repo(), "repo");
}

#[test]
fn role_credential_is_provider_and_role_scoped() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = crate::test_scratch::root().join(format!(
        "forgejo-credential-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    let storage = Storage::open_at(&dir.join("fixture.sqlite3")).unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "redmine-fixture")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "forgejo", "other-role-fixture")
        .unwrap();
    assert!(ForgejoConfig::resolve_with_storage(&storage, Role::Executor, None, None).is_err());
    storage
        .save_role_config(
            Role::Executor,
            &crate::auth::StoredConfig {
                provider: Some("redmine".into()),
                api_base: Some("https://legacy.test".into()),
                repository: Some("legacy/repo".into()),
            },
        )
        .unwrap();
    assert!(ForgejoConfig::resolve_with_storage(&storage, Role::Executor, None, None).is_err());
    storage
        .save_forgejo_auth(
            Role::Reviewer,
            "https://forgejo.test",
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let owned = ForgejoConfig::resolve_with_storage(
        &storage,
        Role::Reviewer,
        Some("https://override.test"),
        None,
    )
    .unwrap();
    assert_eq!(owned.api_base(), "https://override.test");
    assert_eq!(owned.repository(), "owner/repo");
    storage.connection.execute_batch("CREATE TRIGGER reject_forgejo BEFORE INSERT ON role_credential WHEN NEW.provider = 'forgejo' AND NEW.role = 'reviewer' BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;").unwrap();
    assert!(
        storage
            .save_forgejo_auth(
                Role::Reviewer,
                "https://other.test",
                "other/repo",
                "other-fixture"
            )
            .is_err()
    );
    assert_eq!(
        storage
            .load_forgejo_config(Role::Reviewer)
            .unwrap()
            .unwrap()
            .repository,
        "owner/repo"
    );
    assert_eq!(
        storage
            .load_role_config(Role::Executor)
            .unwrap()
            .unwrap()
            .provider
            .as_deref(),
        Some("redmine")
    );
    let config = ForgejoConfig::new("https://example.test", "owner/repo").unwrap();
    let error = ForgejoProvider::with_storage(config.clone(), Role::Executor, &storage)
        .err()
        .unwrap();
    assert!(!error.to_string().contains("fixture"));
    storage
        .save_credential(Role::Executor, "forgejo", "correct-fixture")
        .unwrap();
    assert!(ForgejoProvider::with_storage(config, Role::Executor, &storage).is_ok());
    drop(storage);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn transport_authenticates_and_encodes_scoped_paths_without_leaking_errors() {
    let (base, server) = server::respond("200 OK", "{\"ok\":true}");
    let config = ForgejoConfig::new(base, "owner/repo").unwrap();
    let http = ForgejoHttp::new(&config, "fixture-token").unwrap();
    let request = http
        .request(Method::GET, &["issues", "a b"])
        .unwrap()
        .query(&[("q", "a&b")]);
    let result: serde_json::Value = http.get(request, "fixture").unwrap();
    assert_eq!(result["ok"], true);
    let request = server.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /api/v1/repos/owner/repo/issues/a%20b?q=a%26b "));
    assert!(request.contains("authorization: token fixture-token\r\n"));

    let (base, server) = server::respond("403 Forbidden", "fixture-token secret");
    let http = ForgejoHttp::new(
        &ForgejoConfig::new(base, "owner/repo").unwrap(),
        "fixture-token",
    )
    .unwrap();
    let error = http
        .write::<serde_json::Value>(http.request(Method::POST, &["issues"]).unwrap(), "create")
        .unwrap_err();
    assert!(!error.to_string().contains("fixture-token"));
    assert!(!error.to_string().contains("secret"));
    assert!(
        server
            .join()
            .unwrap()
            .starts_with("POST /api/v1/repos/owner/repo/issues ")
    );
}

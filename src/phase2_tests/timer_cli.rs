use super::*;

#[test]
fn timer_parser_accepts_valid_foundation_syntax_and_rejects_malformed_values() {
    let args = [
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "2",
        "--run-id",
        "run-28",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Timer(command::TimerCommand::Start {
            issue,
            phase,
            agent_role,
            attempt,
            run_id,
            owner_session_id,
            owner_call_id,
        }) => {
            assert_eq!(issue, 28);
            assert_eq!(phase, "implementation");
            assert_eq!(agent_role, "executor");
            assert_eq!(attempt, 2);
            assert_eq!(run_id.as_deref(), Some("run-28"));
            assert_eq!(owner_session_id, None);
            assert_eq!(owner_call_id, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for malformed in [
        vec![
            "timer",
            "start",
            "28",
            "--phase",
            "implementation",
            "--agent-role",
            "executor",
            "--attempt",
            "0",
        ],
        vec!["timer", "finish", "run-28", "--result", "SUCCESS"],
        vec![
            "timer",
            "start",
            "0",
            "--phase",
            "implementation",
            "--agent-role",
            "reviewer",
            "--attempt",
            "1",
        ],
    ] {
        let malformed = malformed.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(command::parse_with_role_env(&malformed, Some("orchestrator")).is_err());
    }
}

#[test]
fn timer_execution_is_orchestrator_and_redmine_only() {
    let home = crate::test_scratch::root().join(format!(
        "phasegent-timer-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .start_timer_run(
            "boundary-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    let executor = crate::time_tracking_cli::execute(
        Some(Role::Executor),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Finish {
            run_id: "boundary-run".to_owned(),
            result: "DONE".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(executor.json()["kind"], "config");
    assert!(
        storage
            .load_timer_run("boundary-run")
            .unwrap()
            .unwrap()
            .status
            == "running"
    );

    let forgejo = crate::time_tracking_cli::execute(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::Finish {
            run_id: "boundary-run".to_owned(),
            result: "DONE".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(forgejo.json()["kind"], "not_supported");
    assert!(
        storage
            .load_timer_run("boundary-run")
            .unwrap()
            .unwrap()
            .status
            == "running"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_parser_handles_owner_args_and_recovery_subcommands() {
    // Owner metadata flows through the parser as plain strings so the
    // plugin can attach its session/call identifiers without a special
    // encoding.
    let start_with_owner = [
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "2",
        "--run-id",
        "run-28",
        "--owner-session-id",
        "sess-123",
        "--owner-call-id",
        "call-abc",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&start_with_owner, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Timer(command::TimerCommand::Start {
            issue,
            run_id,
            owner_session_id,
            owner_call_id,
            ..
        }) => {
            assert_eq!(issue, 28);
            assert_eq!(run_id.as_deref(), Some("run-28"));
            assert_eq!(owner_session_id.as_deref(), Some("sess-123"));
            assert_eq!(owner_call_id.as_deref(), Some("call-abc"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Empty owner args are rejected with the same shape as the other
    // bounded timer inputs.
    let empty_owner = [
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
        "--owner-session-id",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&empty_owner, Some("orchestrator"))
        .expect_err("empty owner must error");
    assert!(
        error.contains("owner-session-id cannot be empty"),
        "expected empty owner error, got: {error}"
    );

    // `list` accepts the status filter and the limit cap.
    for (args, expected_status, expected_limit) in [
        (
            vec!["--provider", "redmine", "timer", "list"],
            "all".to_owned(),
            100_u32,
        ),
        (
            vec![
                "--provider",
                "redmine",
                "timer",
                "list",
                "--status",
                "running",
                "--limit",
                "7",
            ],
            "running".to_owned(),
            7_u32,
        ),
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        match command::parse_with_role_env(&args, Some("orchestrator"))
            .unwrap()
            .command
        {
            command::Command::Timer(command::TimerCommand::List { status, limit }) => {
                assert_eq!(status, expected_status);
                assert_eq!(limit, expected_limit);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    // Invalid status values keep the parser strict so a typo surfaces
    // before any storage call.
    let bad_status = ["--provider", "redmine", "timer", "list", "--status", "open"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let bad = command::parse_with_role_env(&bad_status, Some("orchestrator"))
        .expect_err("invalid --status must error");
    assert!(bad.contains("running, finished, or all"));

    // `get` and `recover` need a non-empty positional run id.
    let get = ["--provider", "redmine", "timer", "get", "phase-51"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&get, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Timer(command::TimerCommand::Get { run_id }) => {
            assert_eq!(run_id, "phase-51");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let recover = ["--provider", "redmine", "timer", "recover", "phase-51"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&recover, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Timer(command::TimerCommand::Recover { run_id }) => {
            assert_eq!(run_id, "phase-51");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for missing in [
        vec!["--provider", "redmine", "timer", "get"],
        vec!["--provider", "redmine", "timer", "recover"],
    ] {
        let missing = missing.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let error = command::parse_with_role_env(&missing, Some("orchestrator"))
            .expect_err("missing positional must error");
        assert!(
            error.contains("missing arguments") || error.contains("requires a run id"),
            "expected run-id error, got: {error}"
        );
    }
}

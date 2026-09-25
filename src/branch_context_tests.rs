use crate::branch_context::{
    self, BindOutcome, BranchContextError, GitOutput, GitRunner, UnbindOutcome,
};
use crate::cli::branch::{permission_denial, should_auto_acquire};
use crate::command::{self, Command, IssueCommand};
use crate::policy::Role;
use std::cell::RefCell;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Fake Git runner for detached-HEAD and overwrite behavior tests.
// ---------------------------------------------------------------------------

struct FakeResponse {
    /// Matched as a prefix of the actual argv (after `git`).
    args: &'static [&'static str],
    status: i32,
    stdout: String,
}

struct FakeGitRunner {
    responses: Vec<FakeResponse>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeGitRunner {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl GitRunner for FakeGitRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|value| value.to_string()).collect());
        for response in &self.responses {
            if args.starts_with(response.args) {
                return Ok(GitOutput {
                    status: response.status,
                    stdout: response.stdout.to_owned(),
                });
            }
        }
        Err(BranchContextError::new(
            "git",
            format!("unexpected git invocation {args:?}"),
        ))
    }
}

fn branch_runner(branch: &str, stored: Option<&str>) -> FakeGitRunner {
    let mut responses = vec![FakeResponse {
        args: &["symbolic-ref"],
        status: 0,
        stdout: branch.to_owned(),
    }];
    if let Some(stored) = stored {
        responses.push(FakeResponse {
            args: &["config", "--local", "--get"],
            status: 0,
            stdout: stored.to_owned(),
        });
    } else {
        responses.push(FakeResponse {
            args: &["config", "--local", "--get"],
            status: 1,
            stdout: String::new(),
        });
    }
    // Catch-all for set/unset writes; matched only after the more specific
    // --get prefix because responses are evaluated in order.
    responses.push(FakeResponse {
        args: &["config", "--local"],
        status: 0,
        stdout: String::new(),
    });
    FakeGitRunner::new(responses)
}

fn detached_runner() -> FakeGitRunner {
    FakeGitRunner::new(vec![FakeResponse {
        args: &["symbolic-ref"],
        status: 1,
        stdout: String::new(),
    }])
}

// ---------------------------------------------------------------------------
// Parser shapes.
// ---------------------------------------------------------------------------

fn parse_args(values: &[&str]) -> Result<command::Invocation, String> {
    parse_with_role(values, None)
}

#[test]
fn issue_bind_parses_positive_id_and_optional_replace() {
    let invocation =
        parse_with_role(&["issue", "bind", "23"], Some("orchestrator")).expect("bind parses");
    match invocation.command {
        Command::Issue(IssueCommand::Bind {
            issue_id,
            replace,
            session,
        }) => {
            assert_eq!(issue_id, 23);
            assert!(!replace);
            assert_eq!(session, None, "--session is optional");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let invocation = parse_with_role(&["issue", "bind", "24", "--replace"], Some("orchestrator"))
        .expect("bind --replace parses");
    match invocation.command {
        Command::Issue(IssueCommand::Bind {
            issue_id,
            replace,
            session,
        }) => {
            assert_eq!(issue_id, 24);
            assert!(replace);
            assert_eq!(session, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_bind_accepts_optional_session() {
    let invocation = parse_with_role(
        &["issue", "bind", "23", "--session", "s1"],
        Some("orchestrator"),
    )
    .expect("bind --session parses");
    match invocation.command {
        Command::Issue(IssueCommand::Bind { session, .. }) => {
            assert_eq!(session.as_deref(), Some("s1"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_bind_rejects_blank_and_overlong_session() {
    for raw in ["", "   "] {
        assert!(
            parse_with_role(
                &["issue", "bind", "23", "--session", raw],
                Some("orchestrator")
            )
            .is_err(),
            "issue bind accepted blank session {raw:?}"
        );
    }
    let overlong = "s".repeat(129);
    let error = parse_with_role(
        &["issue", "bind", "23", "--session", &overlong],
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        error.contains("session") && error.contains("128"),
        "unexpected error: {error}"
    );
}

#[test]
fn issue_bind_rejects_zero_negative_and_nonnumeric_ids() {
    for raw in ["0", "-1", "abc", "12abc"] {
        assert!(
            parse_with_role(&["issue", "bind", raw], Some("orchestrator")).is_err(),
            "issue bind accepted invalid id {raw:?}"
        );
    }
}

#[test]
fn issue_unbind_and_status_parse_without_arguments_or_options() {
    for operation in ["unbind", "status"] {
        let invocation = parse_args(&["issue", operation]).expect("local command parses");
        assert!(invocation.role.is_none());
        if operation == "unbind" {
            assert!(matches!(
                invocation.command,
                Command::Issue(IssueCommand::Unbind)
            ));
        } else {
            assert!(matches!(
                invocation.command,
                Command::Issue(IssueCommand::StatusBranch)
            ));
        }
        assert!(
            parse_with_role(&["issue", operation, "extra"], Some("executor")).is_err(),
            "issue {operation} must reject extra arguments"
        );
        assert!(
            parse_with_role(&["issue", operation, "--unknown"], Some("executor")).is_err(),
            "issue {operation} must reject unknown options"
        );
    }
}

#[test]
fn hooks_install_parses_as_placeholder_command() {
    let invocation =
        parse_with_role(&["hooks", "install"], Some("orchestrator")).expect("hooks install parses");
    match invocation.command {
        Command::Hooks(crate::hooks::HooksCommand::Install) => {}
        other => panic!("unexpected command: {other:?}"),
    }

    assert!(parse_args(&["hooks"]).is_ok());
    assert!(parse_args(&["hooks", "uninstall"]).is_err());
    assert!(parse_args(&["hooks", "install", "extra"]).is_err());
    assert!(parse_args(&["--help", "hooks"]).is_ok());
    assert!(parse_args(&["--help", "hooks", "install"]).is_ok());
}

#[test]
fn existing_issue_commands_still_parse() {
    // Compatibility guard: adding bind/unbind/status must not disturb the
    // pre-existing provider-backed subcommands.
    let invocation =
        parse_with_role(&["issue", "close", "5"], Some("orchestrator")).expect("close parses");
    match invocation.command {
        Command::Issue(IssueCommand::Close { number, .. }) => assert_eq!(number, 5),
        other => panic!("unexpected command: {other:?}"),
    }
    assert!(parse_with_role(&["issue", "search"], Some("orchestrator")).is_ok());
}

// ---------------------------------------------------------------------------
// `PHASEGENT_ROLE` resolution.
//
// The process-global `PHASEGENT_ROLE` is never set here: the parser exposes
// an injectable variant so parallel parser assertions in other test modules
// stay independent of this environment.
// ---------------------------------------------------------------------------

fn parse_with_role(values: &[&str], role_env: Option<&str>) -> Result<command::Invocation, String> {
    let args = values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    command::parse_with_role_env(&args, role_env)
}

#[test]
fn phasegent_role_env_supplies_role() {
    for raw in ["executor", " executor ", "\texecutor\n"] {
        let invocation = parse_with_role(&["issue", "get", "1"], Some(raw))
            .unwrap_or_else(|error| panic!("env role {raw:?} must parse: {error}"));
        assert_eq!(invocation.role, Some(Role::Executor), "env role {raw:?}");
        assert!(matches!(
            invocation.command,
            Command::Issue(IssueCommand::Get { number: 1 })
        ));
    }
}

#[test]
fn removed_role_flag_is_rejected() {
    for token in ["--role", "--role=reviewer"] {
        let error = parse_with_role(&[token, "reviewer", "issue", "get", "1"], Some("executor"))
            .expect_err("the removed --role flag must be rejected");
        assert!(
            error.starts_with("unknown option '--role"),
            "token {token}: {error}"
        );
    }
}

#[test]
fn invalid_phasegent_role_env_is_rejected() {
    let error = parse_with_role(&["issue", "get", "1"], Some("bogus"))
        .expect_err("invalid env role must error");
    assert!(
        error.contains("PHASEGENT_ROLE is invalid"),
        "unexpected error: {error}"
    );
    assert!(error.contains("invalid role 'bogus'"), "got: {error}");
}

#[test]
fn blank_or_absent_phasegent_role_env_keeps_the_previous_requirement() {
    for role_env in [None, Some(""), Some("   ")] {
        let error = parse_with_role(&["issue", "get", "1"], role_env).unwrap_err();
        assert!(
            error.contains("a role is required"),
            "env {role_env:?} must behave as unset, got: {error}"
        );
    }
}

#[test]
fn no_role_whitelist_commands_still_parse_with_role_env_present() {
    // The env fallback lands before the `no_role_allowed` gate, so a
    // role-less whitelist command is accepted and carries whatever role the
    // environment supplied. `issue bind` keeps its role-less passthrough, but
    // an explicit child role is now rejected by the shared registry gate.
    let invocation =
        parse_with_role(&["issue", "bind", "23"], Some("orchestrator")).expect("bind parses");
    assert_eq!(invocation.role, Some(Role::Orchestrator));
    assert!(matches!(
        invocation.command,
        Command::Issue(IssueCommand::Bind { issue_id: 23, .. })
    ));

    let denied = parse_with_role(&["issue", "bind", "23"], Some("executor")).unwrap_err();
    assert!(
        denied.contains("role 'executor' is not allowed to perform issue bind"),
        "got: {denied}"
    );

    let invocation =
        parse_with_role(&["issue", "status"], Some("executor")).expect("status parses");
    assert_eq!(invocation.role, Some(Role::Executor));

    let invocation =
        parse_with_role(&["issue", "unbind"], Some("orchestrator")).expect("unbind parses");
    assert_eq!(invocation.role, Some(Role::Orchestrator));
}

// ---------------------------------------------------------------------------
// Branch context behavior against the fake runner.
// ---------------------------------------------------------------------------

#[test]
fn config_key_uses_native_branch_section_naming() {
    assert_eq!(
        branch_context::config_key("feature/x"),
        "branch.feature/x.redmine-issue-id"
    );
    assert_eq!(
        branch_context::config_key("main"),
        "branch.main.redmine-issue-id"
    );
}

#[test]
fn issue_ids_must_be_strictly_positive_integers() {
    assert_eq!(branch_context::parse_issue_id("42").unwrap(), 42);
    assert_eq!(
        branch_context::parse_issue_id("18446744073709551615").unwrap(),
        u64::MAX
    );
    for raw in ["0", "-1", "", "abc", "1.5", " 7"] {
        let error = branch_context::parse_issue_id(raw).unwrap_err();
        assert_eq!(error.kind, "argument", "id {raw:?}");
    }
}

#[test]
fn detached_head_is_a_structured_actionable_error_for_every_operation() {
    let runner = detached_runner();
    let error = branch_context::current_branch(&runner).unwrap_err();
    assert_eq!(error.kind, "branch");
    assert!(error.message.contains("detached"));

    for result in [
        branch_context::bind(&runner, 23, false)
            .err()
            .map(|e| e.kind),
        branch_context::unbind(&runner).err().map(|e| e.kind),
        branch_context::status(&runner).err().map(|e| e.kind),
    ] {
        assert_eq!(result, Some("branch"), "{result:?}");
    }
}

#[test]
fn bind_writes_branch_scoped_local_config_key() {
    let runner = branch_runner("feature/ctx", None);
    let outcome = branch_context::bind(&runner, 23, false).unwrap();
    assert_eq!(
        outcome,
        BindOutcome {
            branch: "feature/ctx".to_owned(),
            issue_id: 23,
            replaced_existing: false,
            already_bound: false,
        }
    );
    let calls = runner.recorded();
    assert!(calls.contains(&vec![
        "config".to_owned(),
        "--local".to_owned(),
        "branch.feature/ctx.redmine-issue-id".to_owned(),
        "23".to_owned(),
    ]));
}

#[test]
fn bind_rejects_overwrite_without_explicit_replace() {
    let runner = branch_runner("feature/ctx", Some("7"));
    let error = branch_context::bind(&runner, 9, false).unwrap_err();
    assert_eq!(error.kind, "conflict");
    assert_eq!(error.branch.as_deref(), Some("feature/ctx"));
    assert!(error.message.contains('7'));
    // Only reads happened: every config invocation was a --get.
    assert!(
        runner
            .recorded()
            .iter()
            .filter(|call| call.first().map(String::as_str) == Some("config"))
            .all(|call| call.contains(&"--get".to_owned()))
    );

    let outcome = branch_context::bind(&runner, 9, true).unwrap();
    assert!(outcome.replaced_existing);
}

#[test]
fn rebinding_same_issue_is_idempotent() {
    let runner = branch_runner("main", Some("23"));
    let outcome = branch_context::bind(&runner, 23, false).unwrap();
    assert!(outcome.already_bound);
    assert!(!outcome.replaced_existing);
}

#[test]
fn unbound_branch_reports_not_bound_without_unset_call() {
    let runner = branch_runner("main", None);
    assert_eq!(
        branch_context::unbind(&runner).unwrap(),
        UnbindOutcome::NotBound {
            branch: "main".to_owned(),
        }
    );
    assert!(
        runner
            .recorded()
            .iter()
            .all(|call| !call.contains(&"--unset".to_owned()))
    );
}

#[test]
fn bound_branch_unsets_the_branch_config_key() {
    let runner = branch_runner("main", Some("23"));
    assert_eq!(
        branch_context::unbind(&runner).unwrap(),
        UnbindOutcome::Unbound {
            branch: "main".to_owned(),
        }
    );
    assert!(
        runner
            .recorded()
            .iter()
            .any(|call| call.contains(&"--unset".to_owned())
                && call.contains(&"branch.main.redmine-issue-id".to_owned()))
    );
}

#[test]
fn status_reports_branch_with_optional_issue() {
    let runner = branch_runner("main", None);
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.branch, "main");
    assert_eq!(status.issue_id, None);
    assert_eq!(status.source, branch_context::BranchIssueSource::None);

    let runner = branch_runner("feature/x", Some("44"));
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, Some(44));
    assert_eq!(status.source, branch_context::BranchIssueSource::Bound);
}

#[test]
fn branch_name_fallback_prefers_type_id_then_trailing_then_bare() {
    // Rule 1: type/id[-slug] first.
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/452"),
        Some(452)
    );
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/452-slug"),
        Some(452)
    );
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("fix/452_extra"),
        Some(452)
    );
    // Rule 2: legacy trailing -/_id.
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/help-unify-447"),
        Some(447)
    );
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/help-unify_447"),
        Some(447)
    );
    // Rule 3: bare digits.
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("452"),
        Some(452)
    );
    // Rule 1 wins over rule 2 when both match.
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/452-help-447"),
        Some(452)
    );
    // Non-positive and non-numeric names have no fallback.
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("main"),
        None
    );
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feature/x"),
        None
    );
    assert_eq!(branch_context::parse_issue_id_from_branch_name("0"), None);
    assert_eq!(
        branch_context::parse_issue_id_from_branch_name("feat/0"),
        None
    );
}

#[test]
fn status_marks_key_over_name_fallback_source() {
    // Key wins over a parseable branch name.
    let runner = branch_runner("feat/452", Some("99"));
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, Some(99));
    assert_eq!(status.source, branch_context::BranchIssueSource::Bound);

    // No key: type/id fallback is named.
    let runner = branch_runner("feat/452", None);
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, Some(452));
    assert_eq!(status.source, branch_context::BranchIssueSource::Named);

    // No key: legacy trailing fallback is named.
    let runner = branch_runner("feat/help-unify-447", None);
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, Some(447));
    assert_eq!(status.source, branch_context::BranchIssueSource::Named);

    // No key and no fallback: none.
    let runner = branch_runner("main", None);
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, None);
    assert_eq!(status.source, branch_context::BranchIssueSource::None);
}

#[test]
fn execute_status_keeps_branch_and_issue_id_and_adds_source() {
    let runner = branch_runner("feat/452", None);
    let value = branch_context::execute_status(&runner).unwrap();
    assert_eq!(value["branch"], serde_json::json!("feat/452"));
    assert_eq!(value["issue_id"], serde_json::json!(452));
    assert_eq!(value["source"], serde_json::json!("named"));

    let runner = branch_runner("main", None);
    let value = branch_context::execute_status(&runner).unwrap();
    assert_eq!(value["branch"], serde_json::json!("main"));
    assert!(value["issue_id"].is_null());
    assert_eq!(value["source"], serde_json::json!("none"));
}

#[test]
fn sanitize_output_strips_control_characters_and_bounds_length() {
    assert_eq!(branch_context::sanitize_output(b"ok\n"), "ok");
    assert_eq!(branch_context::sanitize_output(b"a\x07b\x1b[31m"), "ab[31m");
    let long = "x".repeat(500);
    let sanitized = branch_context::sanitize_output(long.as_bytes());
    assert_eq!(sanitized.len(), 200);
}

// ---------------------------------------------------------------------------
// Issue #541 Phase 3: bind/unbind role gate and repeat-bind acquire skip.
// ---------------------------------------------------------------------------

fn bind_command(issue_id: u64) -> IssueCommand {
    IssueCommand::Bind {
        issue_id,
        replace: false,
        session: None,
    }
}

#[test]
fn explicit_non_orchestrator_roles_are_denied_bind_and_unbind() {
    for role in [Role::Executor, Role::Reviewer, Role::Tester, Role::Admin] {
        for (command, operation) in [
            (bind_command(541), "issue bind"),
            (IssueCommand::Unbind, "issue unbind"),
        ] {
            let denial = permission_denial(Some(role), &command)
                .unwrap_or_else(|| panic!("{role} must be denied {operation}"));
            assert_eq!(denial["kind"], serde_json::json!("permission"));
            assert_eq!(denial["role"], serde_json::json!(role.as_str()));
            assert_eq!(denial["operation"], serde_json::json!(operation));
            assert!(
                denial["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(operation)),
                "the message must name the denied operation: {denial}"
            );
        }
    }
}

#[test]
fn orchestrator_and_role_less_calls_pass_the_branch_gate() {
    assert!(permission_denial(Some(Role::Orchestrator), &bind_command(541)).is_none());
    assert!(permission_denial(Some(Role::Orchestrator), &IssueCommand::Unbind).is_none());
    assert!(permission_denial(None, &bind_command(541)).is_none());
    assert!(permission_denial(None, &IssueCommand::Unbind).is_none());
}

#[test]
fn status_branch_is_unrestricted_for_every_role() {
    for role in [
        None,
        Some(Role::Orchestrator),
        Some(Role::Executor),
        Some(Role::Reviewer),
        Some(Role::Tester),
        Some(Role::Admin),
    ] {
        assert!(
            permission_denial(role, &IssueCommand::StatusBranch).is_none(),
            "{role:?} must keep read-only status-branch access"
        );
    }
}

#[test]
fn repeat_bind_skips_auto_acquire_while_first_bind_keeps_it() {
    let already_bound = branch_context::execute_bind(&branch_runner("main", Some("23")), 23, false)
        .expect("repeat bind document");
    assert_eq!(already_bound["already_bound"], serde_json::json!(true));
    assert!(!should_auto_acquire(&already_bound));

    let fresh = branch_context::execute_bind(&branch_runner("feature/ctx", None), 23, false)
        .expect("first bind document");
    assert_eq!(fresh["already_bound"], serde_json::json!(false));
    assert!(should_auto_acquire(&fresh));

    // A missing flag is not a skip: only an explicit `true` suppresses the
    // hook, so an older document shape keeps the previous behaviour.
    assert!(should_auto_acquire(&serde_json::json!({"bound": true})));

    // The bind document stays the same five-field object; the gate only reads
    // a flag out of it (keys sort alphabetically without `preserve_order`).
    let mut keys: Vec<&str> = fresh
        .as_object()
        .expect("bind document is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["already_bound", "bound", "branch", "issue_id", "replaced"]
    );
}

// ---------------------------------------------------------------------------
// Real-Git integration (skips silently when git is unavailable).
// ---------------------------------------------------------------------------

struct TempRepo(PathBuf);

impl TempRepo {
    fn new(tag: &str) -> Option<Self> {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-bctx-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runner = branch_context::ProcessGitRunner::in_directory(&dir);
        runner.run(&["init", "-q"]).ok()?;
        if !dir.exists() {
            return None;
        }
        Some(Self(dir))
    }

    fn runner(&self) -> branch_context::ProcessGitRunner {
        branch_context::ProcessGitRunner::in_directory(self.0.clone())
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_git_repo_round_trips_binding_lifecycle() {
    let Some(repo) = TempRepo::new("round-trip") else {
        return;
    };
    let runner = repo.runner();

    let status = branch_context::status(&runner).unwrap();
    assert!(!status.branch.is_empty());
    assert_eq!(status.issue_id, None);

    let outcome = branch_context::bind(&runner, 23, false).unwrap();
    assert_eq!(outcome.issue_id, 23);
    assert_eq!(branch_context::status(&runner).unwrap().issue_id, Some(23));

    let error = branch_context::bind(&runner, 24, false).unwrap_err();
    assert_eq!(error.kind, "conflict");
    branch_context::bind(&runner, 24, true).unwrap();
    assert_eq!(branch_context::status(&runner).unwrap().issue_id, Some(24));

    assert_eq!(
        branch_context::unbind(&runner).unwrap(),
        UnbindOutcome::Unbound {
            branch: status.branch,
        }
    );
    assert_eq!(
        branch_context::unbind(&runner).unwrap(),
        UnbindOutcome::NotBound {
            branch: branch_context::current_branch(&runner).unwrap(),
        }
    );
}

#[test]
fn real_git_detached_head_is_rejected() {
    let Some(repo) = TempRepo::new("detached") else {
        return;
    };
    let runner = repo.runner();
    runner
        .run(&[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .unwrap();
    runner.run(&["checkout", "-q", "--detach"]).unwrap();

    let error = branch_context::bind(&runner, 23, false).unwrap_err();
    assert_eq!(error.kind, "branch");
    assert!(error.message.contains("detached"));
}

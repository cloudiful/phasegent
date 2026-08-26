use crate::branch_context::{
    self, BindOutcome, BranchContextError, GitOutput, GitRunner, UnbindOutcome,
};
use crate::command::{self, Command, IssueCommand};
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
    command::parse(
        &values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn issue_bind_parses_positive_id_and_optional_replace() {
    let invocation =
        parse_args(&["--role", "orchestrator", "issue", "bind", "23"]).expect("bind parses");
    match invocation.command {
        Command::Issue(IssueCommand::Bind { issue_id, replace }) => {
            assert_eq!(issue_id, 23);
            assert!(!replace);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let invocation = parse_args(&["--role", "orchestrator", "issue", "bind", "24", "--replace"])
        .expect("bind --replace parses");
    match invocation.command {
        Command::Issue(IssueCommand::Bind { issue_id, replace }) => {
            assert_eq!(issue_id, 24);
            assert!(replace);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_bind_rejects_zero_negative_and_nonnumeric_ids() {
    for raw in ["0", "-1", "abc", "12abc"] {
        assert!(
            parse_args(&["--role", "orchestrator", "issue", "bind", raw]).is_err(),
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
            parse_args(&["--role", "executor", "issue", operation, "extra"]).is_err(),
            "issue {operation} must reject extra arguments"
        );
        assert!(
            parse_args(&["--role", "executor", "issue", operation, "--unknown"]).is_err(),
            "issue {operation} must reject unknown options"
        );
    }
}

#[test]
fn hooks_install_parses_as_placeholder_command() {
    let invocation =
        parse_args(&["--role", "orchestrator", "hooks", "install"]).expect("hooks install parses");
    match invocation.command {
        Command::Hooks(crate::hooks::HooksCommand::Install) => {}
        other => panic!("unexpected command: {other:?}"),
    }

    assert!(parse_args(&["--role", "orchestrator", "hooks"]).is_ok());
    assert!(parse_args(&["--role", "orchestrator", "hooks", "uninstall"]).is_err());
    assert!(parse_args(&["--role", "orchestrator", "hooks", "install", "extra"]).is_err());
    assert!(parse_args(&["--help", "hooks"]).is_ok());
    assert!(parse_args(&["--help", "hooks", "install"]).is_ok());
}

#[test]
fn existing_issue_commands_still_parse() {
    // Compatibility guard: adding bind/unbind/status must not disturb the
    // pre-existing provider-backed subcommands.
    let invocation =
        parse_args(&["--role", "orchestrator", "issue", "close", "5"]).expect("close parses");
    match invocation.command {
        Command::Issue(IssueCommand::Close { number }) => assert_eq!(number, 5),
        other => panic!("unexpected command: {other:?}"),
    }
    assert!(parse_args(&["--role", "orchestrator", "issue", "search"]).is_ok());
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

    let runner = branch_runner("feature/x", Some("44"));
    let status = branch_context::status(&runner).unwrap();
    assert_eq!(status.issue_id, Some(44));
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
// Real-Git integration (skips silently when git is unavailable).
// ---------------------------------------------------------------------------

struct TempRepo(PathBuf);

impl TempRepo {
    fn new(tag: &str) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!(
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

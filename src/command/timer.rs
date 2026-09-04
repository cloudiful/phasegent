#[allow(unused_imports)]
use super::parse_helpers::{
    has_flag, optional_option, planning_options, positional_number, require_exact_positionals,
    required_nonempty_option, required_option, required_value, split_inline, validate_options,
};
#[allow(unused_imports)]
use super::{
    Command, CommentCommand, HelpTopic, Invocation, IssueCommand, PlanningOptions, ProjectCommand,
    RelationCommand, StatusCommand, TimerCommand, VersionCommand, WorkflowCommand,
};
#[allow(unused_imports)]
use crate::policy::Role;
#[allow(unused_imports)]
use crate::providers::ProviderKind;
#[allow(unused_imports)]
use crate::providers::api::ForgejoError;
#[allow(unused_imports)]
use crate::providers::redmine::model::RedmineRelationType;

pub(crate) fn parse_timer(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Timer));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::TimerCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "start" => {
            validate_options(
                args,
                1,
                &[
                    "--phase",
                    "--agent-role",
                    "--attempt",
                    "--run-id",
                    "--owner-session-id",
                    "--owner-call-id",
                ],
                &[],
                "timer start",
            )?;
            let issue = positional_number(args, 1, "timer start")?;
            if issue == 0 {
                return Err("timer start requires a positive issue id".to_owned());
            }
            let phase = required_nonempty_option(args, "--phase", "timer start")?;
            let raw_agent_role = required_nonempty_option(args, "--agent-role", "timer start")?;
            let agent_role = if raw_agent_role == "tester" {
                "tester".to_owned()
            } else {
                let parsed = raw_agent_role
                    .parse::<Role>()
                    .map_err(|error| format!("timer start --agent-role: {error}"))?;
                if !matches!(parsed, Role::Executor | Role::Reviewer) {
                    return Err(
                        "timer start --agent-role must be executor, reviewer, or tester".to_owned(),
                    );
                }
                parsed.as_str().to_owned()
            };
            let attempt = required_option(args, "--attempt", "timer start")?
                .parse::<u64>()
                .map_err(|_| "timer start --attempt requires a non-negative integer".to_owned())?;
            if attempt == 0 {
                return Err("timer start --attempt must be greater than zero".to_owned());
            }
            let run_id = optional_option(args, "--run-id");
            if run_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --run-id cannot be empty".to_owned());
            }
            let owner_session_id = optional_option(args, "--owner-session-id");
            if owner_session_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --owner-session-id cannot be empty".to_owned());
            }
            let owner_call_id = optional_option(args, "--owner-call-id");
            if owner_call_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --owner-call-id cannot be empty".to_owned());
            }
            Ok(Command::Timer(TimerCommand::Start {
                issue,
                phase,
                agent_role,
                attempt,
                run_id,
                owner_session_id,
                owner_call_id,
            }))
        }
        "finish" => {
            validate_options(args, 1, &["--result"], &[], "timer finish")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer finish run id cannot be empty".to_owned())?;
            let result = required_option(args, "--result", "timer finish")?;
            if !["DONE", "PARTIAL", "BLOCKED", "FAILED"].contains(&result.as_str()) {
                return Err(
                    "timer finish --result must be DONE, PARTIAL, BLOCKED, or FAILED".to_owned(),
                );
            }
            Ok(Command::Timer(TimerCommand::Finish { run_id, result }))
        }
        "list" => {
            validate_options(args, 0, &["--status", "--limit"], &[], "timer list")?;
            let status = optional_option(args, "--status").unwrap_or_else(|| "all".to_owned());
            if !matches!(status.as_str(), "running" | "finished" | "all") {
                return Err("timer list --status must be running, finished, or all".to_owned());
            }
            let limit = match optional_option(args, "--limit") {
                Some(value) => value
                    .parse::<u32>()
                    .map_err(|_| "timer list --limit requires a non-negative integer".to_owned())?,
                None => 100,
            };
            if limit == 0 {
                return Err("timer list --limit must be greater than zero".to_owned());
            }
            Ok(Command::Timer(TimerCommand::List { status, limit }))
        }
        "get" => {
            validate_options(args, 1, &[], &[], "timer get")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer get requires a run id".to_owned())?;
            Ok(Command::Timer(TimerCommand::Get { run_id }))
        }
        "recover" => {
            validate_options(args, 1, &[], &[], "timer recover")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer recover requires a run id".to_owned())?;
            Ok(Command::Timer(TimerCommand::Recover { run_id }))
        }
        value => Err(format!("unknown timer command '{value}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Command, TimerCommand};
    use crate::command;

    fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn tester_parses_as_child_identity() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "timer",
            "start",
            "42",
            "--phase",
            "impl",
            "--agent-role",
            "tester",
            "--attempt",
            "1",
        ]))
        .unwrap();
        match invocation.command {
            Command::Timer(TimerCommand::Start { agent_role, .. }) => {
                assert_eq!(agent_role, "tester")
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn tester_identity_is_not_global_role() {
        // Tester is now a first-class global role with its own credential.
        assert!("tester".parse::<crate::policy::Role>().is_ok());
        assert_eq!(
            "tester".parse::<crate::policy::Role>().unwrap(),
            crate::policy::Role::Tester
        );
        assert_eq!(crate::policy::Role::Tester.as_str(), "tester");
        // orchestrator remains the only CLI role for uploads/workflow
        assert!("orchestrator".parse::<crate::policy::Role>().is_ok());
    }

    #[test]
    fn timer_start_rejects_invalid_agent_role() {
        let err = command::parse(&strings([
            "--role",
            "orchestrator",
            "timer",
            "start",
            "42",
            "--phase",
            "impl",
            "--agent-role",
            "admin",
            "--attempt",
            "1",
        ]))
        .unwrap_err();
        assert!(
            err.contains("executor, reviewer, or tester") || err.contains("executor or reviewer"),
            "error should mention allowed roles: {err}"
        );
    }

    #[test]
    fn executor_and_reviewer_still_parse() {
        for role in ["executor", "reviewer"] {
            let invocation = command::parse(&strings([
                "--role",
                "orchestrator",
                "timer",
                "start",
                "7",
                "--phase",
                "p",
                "--agent-role",
                role,
                "--attempt",
                "2",
            ]))
            .unwrap();
            match invocation.command {
                Command::Timer(TimerCommand::Start { agent_role, .. }) => {
                    assert_eq!(agent_role, role)
                }
                other => panic!("unexpected {other:?} for {role}"),
            }
        }
    }
}

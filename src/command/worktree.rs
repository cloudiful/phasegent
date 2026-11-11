//! Parser for the `worktree` command group.
//!
//! Phase 2 of issue #239 (worktree-seamless) exposes the Rust core to the
//! CLI. The parser follows the existing `command/<name>.rs` pattern used
//! by `comment`, `timer`, `notify`, and others:
//!
//! * `args.first()` is the subcommand name (`acquire` / `status` /
//!   `list` / `probe` / `prune`); a
//!   missing or `--help` first token returns
//!   `Command::Help(HelpTopic::Worktree)`, and a trailing `--help`
//!   returns `Command::Help(HelpTopic::WorktreeCommand(name))`.
//! * Each subcommand uses the standard `validate_options` /
//!   `optional_option` / `required_nonempty_option` / `has_flag`
//!   helpers so the leading-dash escape hatch (`--option=VALUE`) works
//!   for values that begin with `-`.
//! * No provider / network is touched here; the role gate lives in
//!   `cli::worktree::execute_worktree`.

use super::parse_helpers::{
    has_flag, optional_nonempty_option, optional_option, page_request, required_nonempty_option,
    validate_options,
};
use super::{Command, HelpTopic, WorktreeCommand};

pub(crate) fn parse_worktree(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(Command::Help(HelpTopic::Worktree));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::WorktreeCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "acquire" => parse_acquire(args),
        "status" => parse_status(args),
        "list" => parse_list(args),
        "probe" => parse_probe(args),
        "prune" => parse_prune(args),
        value => Err(format!("unknown worktree command '{value}'")),
    }
}

fn parse_acquire(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--issue", "--session", "--base", "--format"],
        &["--isolate", "--reuse", "--no-sync"],
        "worktree acquire",
    )?;
    let issue_raw = required_nonempty_option(args, "--issue", "worktree acquire")?;
    let issue: u64 = issue_raw
        .parse()
        .map_err(|_| "worktree acquire --issue must be a positive integer".to_owned())?;
    if issue == 0 {
        return Err("worktree acquire --issue must be greater than zero".to_owned());
    }
    let session = match optional_option(args, "--session") {
        Some(raw) => Some(validate_session_id(&raw, "worktree acquire")?),
        None => None,
    };
    let base = optional_nonempty_option(args, "--base", "worktree acquire")?;
    let format = optional_option(args, "--format")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "json".to_owned());
    if format != "json" {
        return Err("worktree acquire --format must be 'json'".to_owned());
    }
    let isolate = has_flag(args, "--isolate");
    // `--reuse` states the default reuse preference explicitly (issue
    // 651 P3): it is accepted and otherwise a no-op, because the
    // default table never bypasses an occupied checkout path. Stating
    // both preferences at once is contradictory, so it fails fast here
    // instead of reaching the decision table.
    if isolate && has_flag(args, "--reuse") {
        return Err(
            "worktree acquire --reuse and --isolate are mutually exclusive; pass one or neither"
                .to_owned(),
        );
    }
    Ok(Command::Worktree(WorktreeCommand::Acquire {
        issue,
        session,
        base,
        format,
        isolate,
        no_sync: has_flag(args, "--no-sync"),
    }))
}

fn parse_status(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--issue", "--page", "--limit"],
        &[],
        "worktree status",
    )?;
    let issue_raw = required_nonempty_option(args, "--issue", "worktree status")?;
    let issue: u64 = issue_raw
        .parse()
        .map_err(|_| "worktree status --issue must be a positive integer".to_owned())?;
    if issue == 0 {
        return Err("worktree status --issue must be greater than zero".to_owned());
    }
    Ok(Command::Worktree(WorktreeCommand::Status {
        issue,
        page: Some(page_request(args, "worktree status")?),
    }))
}

fn parse_list(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--repo", "--page", "--limit"],
        &["--no-sync"],
        "worktree list",
    )?;
    let repo = optional_option(args, "--repo")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(Command::Worktree(WorktreeCommand::List {
        repo,
        no_sync: has_flag(args, "--no-sync"),
        page: Some(page_request(args, "worktree list")?),
    }))
}

fn parse_probe(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--path", "--issue", "--session"],
        &[],
        "worktree probe",
    )?;
    let path = optional_nonempty_option(args, "--path", "worktree probe")?;
    let issue = match optional_option(args, "--issue") {
        Some(raw) => {
            let parsed: u64 = raw
                .parse()
                .map_err(|_| "worktree probe --issue must be a positive integer".to_owned())?;
            if parsed == 0 {
                return Err("worktree probe --issue must be greater than zero".to_owned());
            }
            Some(parsed)
        }
        None => None,
    };
    let session = match optional_option(args, "--session") {
        Some(raw) => Some(validate_session_id(&raw, "worktree probe")?),
        None => None,
    };
    if path.is_some() && issue.is_some() {
        return Err("worktree probe --path and --issue are mutually exclusive".to_owned());
    }
    if session.is_some() && issue.is_none() {
        return Err("worktree probe --session requires --issue".to_owned());
    }
    Ok(Command::Worktree(WorktreeCommand::Probe {
        path,
        issue,
        session,
    }))
}

fn parse_prune(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--repo", "--format"],
        &[
            "--remove",
            "--no-sync",
            "--dryrun",
            "--dry-run",
            "--json",
            "--verbose",
        ],
        "worktree prune",
    )?;
    let repo = optional_option(args, "--repo")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    // Accepted compatibility no-op: removal is the default now, so `--remove`
    // is recorded but never changes the outcome. `--dryrun`/`--dry-run` is the
    // explicit preview and wins when the two are combined.
    let remove = has_flag(args, "--remove");
    let dry_run = has_flag(args, "--dryrun") || has_flag(args, "--dry-run");
    let json_flag = has_flag(args, "--json");
    let explicit_format =
        optional_option(args, "--format").map(|value| value.trim().to_ascii_lowercase());
    // The legacy format selection. The stored default is `"text"`, but the
    // effective output format follows the global `--output` selection unless a
    // legacy format was requested explicitly; the parser records which case
    // applies.
    let format = match (explicit_format, json_flag) {
        (Some(value), _) if value != "text" && value != "json" => {
            return Err("worktree prune --format must be 'text' or 'json'".to_owned());
        }
        (Some(value), true) if value != "json" => {
            return Err(
                "worktree prune --json and --format text are mutually exclusive".to_owned(),
            );
        }
        (Some(value), _) => value,
        (None, true) => "json".to_owned(),
        (None, false) => "text".to_owned(),
    };
    Ok(Command::Worktree(WorktreeCommand::Prune {
        repo,
        remove,
        dry_run,
        format,
        verbose: has_flag(args, "--verbose"),
        no_sync: has_flag(args, "--no-sync"),
    }))
}

/// Validate an explicit session id at parse time so blanks and overlong
/// values fail fast with a parser error (exit 2) instead of reaching the
/// lease table. `resolve_session` also trims the value, which keeps the
/// stored session id canonical.
pub(crate) fn validate_session_id(raw: &str, operation: &str) -> Result<String, String> {
    crate::worktree::resolve_session(Some(raw))
        .map(|context| context.id)
        .map_err(|error| format!("{operation}: {}", error.message))
}

#[cfg(test)]
mod tests {
    use super::super::{Command, WorktreeCommand};
    use crate::command;

    fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn acquire_parses_minimal_args() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "239"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
                no_sync,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session, None);
                assert_eq!(base, None);
                assert_eq!(format, "json");
                assert!(!isolate, "--isolate must default off");
                assert!(!no_sync, "--no-sync must default off");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_parses_isolate_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "247", "--isolate"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire { isolate, .. }) => {
                assert!(isolate, "--isolate must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_parses_session_base_and_format() {
        let invocation = command::parse_with_role_env(
            &strings([
                "worktree",
                "acquire",
                "--issue",
                "239",
                "--session",
                "alpha",
                "--base",
                "main",
                "--format",
                "json",
            ]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
                no_sync,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session.as_deref(), Some("alpha"));
                assert_eq!(base.as_deref(), Some("main"));
                assert_eq!(format, "json");
                assert!(!isolate);
                assert!(!no_sync);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_rejects_non_json_format() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "1", "--format", "yaml"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("--format"));
    }

    #[test]
    fn acquire_rejects_zero_issue() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "0"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("--issue"));
    }

    #[test]
    fn acquire_rejects_blank_session() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "1", "--session", ""]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("session"), "unexpected error: {err}");
    }

    #[test]
    fn acquire_rejects_overlong_session() {
        let overlong = "s".repeat(129);
        let err = command::parse_with_role_env(
            &strings([
                "worktree",
                "acquire",
                "--issue",
                "1",
                "--session",
                &overlong,
            ]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            err.contains("session") && err.contains("128"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn release_and_heartbeat_are_removed_subcommands() {
        for command in ["release", "heartbeat"] {
            let err =
                command::parse_with_role_env(&strings(["worktree", command]), Some("orchestrator"))
                    .unwrap_err();
            assert!(
                err.contains("unknown worktree command"),
                "{command} must be removed, got {err}"
            );
        }
    }

    #[test]
    fn status_parses_issue() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "status", "--issue", "42"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Status { issue, .. }) => assert_eq!(issue, 42),
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn list_parses_repo_optional() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "list", "--repo", "/tmp/repo"]),
            Some("executor"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::List { repo, .. }) => {
                assert_eq!(repo.as_deref(), Some("/tmp/repo"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_defaults_to_removal_and_text_output() {
        let invocation =
            command::parse_with_role_env(&strings(["worktree", "prune"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                repo,
                remove,
                dry_run,
                format,
                verbose,
                no_sync,
            }) => {
                assert_eq!(repo, None);
                assert!(!remove);
                assert!(!dry_run, "prune defaults to removal, not preview");
                assert_eq!(
                    format, "text",
                    "the stored legacy default stays text; the effective format follows the global output mode"
                );
                assert!(!verbose, "--verbose must default off");
                assert!(!no_sync, "--no-sync must default off");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_parses_remove_and_repo() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "prune", "--remove", "--repo", "/tmp/repo"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune { repo, remove, .. }) => {
                assert_eq!(repo.as_deref(), Some("/tmp/repo"));
                assert!(remove, "--remove stays an accepted no-op");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_rejects_the_removed_lease_flags() {
        for flag in ["--release-stale", "--stale-days", "--reason"] {
            let err = command::parse_with_role_env(
                &strings(["worktree", "prune", flag]),
                Some("orchestrator"),
            )
            .unwrap_err();
            assert!(
                err.contains("unknown option"),
                "{flag} must be rejected, got {err}"
            );
        }
    }

    // Issue 552 Phase 2: `acquire` / `list` / `prune` accept `--no-sync`,
    // the opt-out for the pre-subcommand reconciliation pass.

    #[test]
    fn acquire_parses_no_sync_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "552", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire { no_sync, .. }) => {
                assert!(no_sync, "--no-sync must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn list_parses_no_sync_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "list", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::List { repo, no_sync, .. }) => {
                assert_eq!(repo, None);
                assert!(no_sync, "--no-sync must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_parses_no_sync_flag_without_actions() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "prune", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                remove, no_sync, ..
            }) => {
                assert!(no_sync);
                assert!(!remove, "--no-sync does not imply --remove");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn read_only_subcommands_have_no_sync_switch() {
        for command in [
            WorktreeCommand::Status {
                issue: 1,
                page: None,
            },
            WorktreeCommand::Probe {
                path: None,
                issue: None,
                session: None,
            },
        ] {
            assert!(command.sync_taxi().is_none());
        }
        assert!(
            WorktreeCommand::List {
                repo: None,
                no_sync: true,
                page: None,
            }
            .sync_taxi()
            .is_none(),
            "--no-sync removes the list pass"
        );
        assert_eq!(
            WorktreeCommand::List {
                repo: None,
                no_sync: false,
                page: None,
            }
            .sync_taxi(),
            Some(("worktree list", None)),
            "without --repo the pass targets the current directory"
        );
        assert_eq!(
            WorktreeCommand::Acquire {
                issue: 552,
                session: None,
                base: None,
                format: "json".to_owned(),
                isolate: false,
                no_sync: false,
            }
            .sync_taxi(),
            Some(("worktree acquire", None))
        );
        assert!(
            WorktreeCommand::Prune {
                repo: Some("/tmp/repo".to_owned()),
                remove: true,
                dry_run: false,
                format: "text".to_owned(),
                verbose: false,
                no_sync: false,
            }
            .sync_taxi()
            .is_none(),
            "issue 913 removed prune from the reconciliation set"
        );
    }
}

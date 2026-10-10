use super::prelude::*;

pub(crate) fn parse_workflow(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Workflow));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::WorkflowCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "bootstrap" => {
            // The shared `AI Agents` group has been retired. Surface a clear
            // usage error so stale callers do not silently get the new
            // behaviour.
            for rejected in ["--group-name", "--group-role"] {
                if args.iter().any(|value| value == rejected)
                    || args
                        .iter()
                        .any(|value| split_inline(value, rejected).is_some())
                {
                    return Err(format!(
                        "workflow bootstrap {rejected} is no longer supported; \
                         direct memberships are reconciled automatically from the \
                         orchestrator, executor, reviewer, and explore API keys",
                    ));
                }
            }
            validate_options(
                args,
                0,
                &["--repository", "--close-status-id", "--close-status-name"],
                &["--all-projects", "--dry-run"],
                "admin workflow bootstrap",
            )?;
            let all_projects = flag_occurrences(args, "--all-projects");
            let dry_run = flag_occurrences(args, "--dry-run");
            if all_projects > 1 {
                return Err(
                    "admin workflow bootstrap received a duplicate --all-projects flag".to_owned(),
                );
            }
            if dry_run > 1 {
                return Err(
                    "admin workflow bootstrap received a duplicate --dry-run flag".to_owned(),
                );
            }
            let all_projects = all_projects == 1;
            let dry_run = dry_run == 1;
            if dry_run && !all_projects {
                return Err("admin workflow bootstrap --dry-run requires --all-projects".to_owned());
            }
            let repository = optional_option(args, "--repository");
            let close_status_id = optional_option(args, "--close-status-id");
            let close_status_name = optional_option(args, "--close-status-name");
            if all_projects
                && (repository.is_some()
                    || close_status_id.is_some()
                    || close_status_name.is_some())
            {
                return Err(
                    "admin workflow bootstrap --all-projects cannot be combined with \
                     --repository, --close-status-id, or --close-status-name"
                        .to_owned(),
                );
            }
            if close_status_id.is_some() && close_status_name.is_some() {
                return Err(
                    "admin workflow bootstrap accepts either --close-status-id or --close-status-name"
                        .to_owned(),
                );
            }
            Ok(Command::Workflow(WorkflowCommand::Bootstrap {
                repository,
                close_status_id,
                close_status_name,
                all_projects,
                dry_run,
            }))
        }
        value => Err(format!("unknown workflow command '{value}'")),
    }
}

/// Count exact occurrences of a boolean flag so a duplicate is rejected
/// instead of silently collapsing to `true`.
fn flag_occurrences(args: &[String], flag: &str) -> usize {
    args.iter().filter(|value| value.as_str() == flag).count()
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;

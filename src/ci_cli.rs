use crate::ci_command::CiCommand;
use crate::ci_model::{CiInspectRequest, CiRunsFilter};
use crate::cli;
use crate::policy::{Capability, Role};
use crate::provider::CiProvider;

pub fn execute(
    role_value: Option<Role>,
    api_base: Option<&str>,
    repository: Option<&str>,
    command: CiCommand,
) -> i32 {
    let role = cli::required_role(role_value);
    let capability = Capability::CiRead;
    if !role.allows(capability) {
        return cli::permission_error(role, capability);
    }
    let provider = match cli::provider(role, api_base, repository) {
        Ok(provider) => provider,
        Err(error) => return cli::provider_error(error),
    };
    match command {
        CiCommand::Runs {
            sha,
            ref_name,
            status,
            workflow,
            page,
            limit,
        } => cli::print_result(provider.ci_runs(&CiRunsFilter {
            sha,
            ref_name,
            status,
            workflow,
            page,
            limit,
        })),
        CiCommand::RunGet { run_id } => cli::print_result(provider.ci_run_get(run_id)),
        CiCommand::RunJobs { run_id } => cli::print_result(provider.ci_run_jobs(run_id)),
        CiCommand::JobLogs { job_id, tail } => {
            cli::print_result(provider.ci_job_logs(job_id, tail))
        }
        CiCommand::Inspect {
            sha,
            ref_name,
            wait,
            timeout,
            poll,
        } => cli::print_result(provider.ci_inspect(&CiInspectRequest {
            sha,
            ref_name,
            wait,
            timeout,
            poll,
        })),
    }
}

pub fn print_help(role: Option<Role>) {
    if role.is_some_and(|role| !role.allows(Capability::CiRead)) {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
        return;
    }
    println!(
        "CI read commands for {}:\n\n  runs           List workflow runs\n  run get       Get one workflow run\n  run jobs      List jobs for a workflow run\n  job logs      Read bounded job logs\n  inspect       Inspect the run for a commit\n\nUse 'phasegent --help ci <command>' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

pub fn print_command_help(role: Option<Role>, command: &str) {
    if role.is_some_and(|role| !role.allows(Capability::CiRead)) {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
        return;
    }
    let text = match command {
        "runs" => {
            "Usage: ci runs [--sha SHA] [--ref REF] [--status STATUS] [--workflow WORKFLOW] [--page N] [--limit N]"
        }
        "run" => "CI run commands:\n\n  run get RUN_ID\n  run jobs RUN_ID",
        "run get" => "Usage: ci run get RUN_ID",
        "run jobs" => "Usage: ci run jobs RUN_ID",
        "job" => "CI job commands:\n\n  job logs JOB_ID [--tail N]",
        "job logs" => "Usage: ci job logs JOB_ID [--tail N]",
        "inspect" => {
            "Usage: ci inspect --sha SHA [--ref REF] [--wait] [--timeout SEC] [--poll SEC]"
        }
        _ => {
            print_help(role);
            return;
        }
    };
    println!("{text}\n\n{}", Capability::CiRead.description());
}

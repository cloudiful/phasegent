mod auth;
mod branch_context;
mod ci_cli;
mod ci_command;
mod ci_inspect;
mod ci_model;
mod cli;
mod command;
mod config;
mod config_snapshot;
mod forgejo;
mod forgejo_ci;
mod forgejo_http;
mod forgejo_model;
mod gitlab;
mod gitlab_http;
mod gitlab_model;
mod hooks;
mod lifecycle;
mod policy;
mod provider;
mod provider_config;
mod redmine;
mod redmine_http;
mod redmine_model;
mod redmine_planning_cli;
mod redmine_relations_cli;
mod remote;
mod repo_cli;
mod repo_command;
mod storage;
mod storage_schema;
mod time_tracking_cli;
mod workflow;

#[cfg(test)]
mod phase2_tests;

#[cfg(test)]
mod phase3_tests;

#[cfg(test)]
mod branch_context_tests;

#[cfg(test)]
mod hooks_tests;

#[cfg(test)]
mod phase2_contract_tests;

#[cfg(test)]
mod phase1_ci_tests;

#[cfg(test)]
pub(crate) mod redmine_contract_tests;

#[cfg(test)]
mod gitlab_contract_tests;

#[cfg(test)]
mod storage_tests;

#[cfg(test)]
mod config_tests;

fn main() {
    std::process::exit(cli::run(std::env::args().skip(1)));
}

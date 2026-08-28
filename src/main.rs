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
mod hooks;
mod lifecycle;
mod policy;
mod remote;
mod repo_cli;
mod repo_command;
mod time_tracking;
mod time_tracking_cli;
mod workflow;

mod infra;
mod providers;

#[cfg(test)]
mod phase2_tests;

#[cfg(test)]
mod phase3_tests;

#[cfg(test)]
mod branch_context_tests;

#[cfg(test)]
mod hooks_tests;

#[cfg(test)]
mod config_tests;

fn main() {
    std::process::exit(cli::run(std::env::args().skip(1)));
}

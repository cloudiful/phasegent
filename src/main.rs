mod auth;
mod body_file;
mod branch_links;
mod branch_naming;
mod cli;
mod command;
mod config;
mod config_snapshot;
mod config_write;
mod git_runner;
mod hooks;
mod lifecycle;
mod lifecycle_auto;
mod notifications;
mod output;
mod pagination;
mod plugin;
mod policy;
mod record;
mod remote;
mod time_tracking;
mod time_tracking_cli;
mod workflow;
mod worktree;

mod infra;
mod providers;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod worktree_tests;

#[cfg(test)]
mod worktree_cli_tests;

#[cfg(test)]
mod plugin_tests;

#[cfg(test)]
mod test_scratch;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cli::run(args));
}

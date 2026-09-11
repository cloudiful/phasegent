mod auth;
mod branch_context;
mod cli;
mod command;
mod config;
mod config_snapshot;
mod config_write;
mod gui;
mod hooks;
mod launch;
mod lifecycle;
mod lifecycle_auto;
mod mcp;
mod notifications;
mod plugin;
mod policy;
mod remote;
mod repo_cli;
mod repo_command;
mod time_tracking;
mod time_tracking_cli;
mod workflow;
mod worktree;

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

#[cfg(test)]
mod lifecycle_auto_tests;

#[cfg(test)]
mod worktree_tests;

#[cfg(test)]
mod worktree_cli_tests;

#[cfg(test)]
mod plugin_tests;

#[cfg(test)]
mod skill_tests;

#[cfg(test)]
mod admin_tests;

#[cfg(test)]
mod comment_tests;

#[cfg(test)]
mod doctor_tests;

#[cfg(test)]
mod issue_tests;

fn main() {
    // Single-binary dispatch: the conservative no-argument desktop
    // heuristic lives here so `cli::run` keeps its exact existing
    // JSON/error contracts (`cli::run([])` still renders root help).
    // Explicit `gui` is parsed by the shared `command` module and
    // executed in `cli::run`; every other command never initializes
    // the GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() && launch::should_open_gui_on_no_args(&launch::current_no_arg_context()) {
        std::process::exit(gui::run());
    }
    std::process::exit(cli::run(args));
}

mod auth;
mod body_file;
mod branch_links;
mod branch_naming;
mod cli;
mod command;
mod config;
mod config_snapshot;
mod config_write;
mod desktop_bridge;
mod git_runner;
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
    // Hidden packaged-desktop entry: the Electron main process spawns this
    // binary with the `desktop-bridge` token and speaks newline-delimited
    // JSON over stdio. The token is deliberately absent from the command
    // registry, so it never appears in help and is not reachable as a normal
    // CLI command.
    if args.first().map(String::as_str) == Some(desktop_bridge::COMMAND) {
        std::process::exit(desktop_bridge::run(&args[1..]));
    }
    // A bare launch is always CLI space: the desktop shell is the packaged
    // Electron application, so this binary never opens a window and needs no
    // terminal-vs-desktop heuristic.
    std::process::exit(launch::run_cli(args));
}

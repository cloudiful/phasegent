//! Help text for the `plugin` command group (issue #239 Phase 3).
//!
//! The module mirrors the `hooks` help style:
//!
//! * Top-level `plugin` help lists the three subcommands with the
//!   safety guarantees (idempotent, no force-overwrite, marker-based
//!   ownership) spelled out so an operator never has to read the
//!   source to know whether the command is destructive.
//! * Per-subcommand help is plain prose, focused on the contract
//!   rather than the underlying `Bun.$` shell calls.

pub(crate) fn print_plugin_help() {
    println!(
        "Managed OpenCode plugin commands (issue #239 Phase 3):\n\n  install [--global] [--project] [--force]   Install or update the worktree adapter in the OpenCode plugin directory (default: both scopes); idempotent; foreign files are refused unless --force\n  status                                       Report global and project file presence, managed-flag match, size, and mtime (read-only)\n  uninstall [--global] [--project]            Remove managed adapter files; refuses files without the managed marker\n\nUse 'phasegent --help plugin <command>' for options."
    );
}

pub(crate) fn print_plugin_command_help(command: &str) {
    match command {
        "install" => println!(
            "Usage: plugin install [--global] [--project] [--force]\n\nInstalls or updates the phasegent worktree adapter in the OpenCode plugin directory. The global target is $XDG_CONFIG_HOME/opencode/plugins/phasegent-worktree.js (falling back to $HOME/.config/opencode/plugins/phasegent-worktree.js); the project target is .opencode/plugins/phasegent-worktree.js in the current working directory. When neither --global nor --project is supplied, both slots are written. Existing managed files (those that contain the `// phasegent:managed` marker) are updated in place when the embedded template changes; otherwise they are reported as skipped. Foreign files (no marker) are refused unless --force is supplied, in which case the foreign file is renamed to phasegent-worktree.js.phasegent-orig before the managed file is written. Adapter contract: experimental_workspace.register with name/description/configure/create/remove/target; target falls back to the original directory on any failure (no branch or directory is ever deleted by the adapter). No role or provider is required."
        ),
        "status" => println!(
            "Usage: plugin status\n\nReports the global and project slots of the OpenCode worktree adapter. Each slot reports path, exists, managed (whether the file contains the `// phasegent:managed` marker), size, and mtime. The file contents are never printed. No role or provider is required."
        ),
        "uninstall" => println!(
            "Usage: plugin uninstall [--global] [--project]\n\nRemoves the managed worktree adapter from the OpenCode plugin directory. Files without the `// phasegent:managed` marker are refused (remove them manually if needed); missing files are reported as warnings. When neither --global nor --project is supplied, both slots are processed. No role or provider is required. The adapter contract does not delete worktrees or branches; use `phasegent worktree prune` for that."
        ),
        _ => print_plugin_help(),
    }
}

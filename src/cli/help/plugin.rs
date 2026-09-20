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

use super::common::{HelpRow, print_group_help, render_group_help};

/// Split the top-level `plugin` overview into header plus rows so the shape
/// is testable without capturing stdout. The group needs no role gate (no
/// role or provider is required), so the caller always renders with
/// `role=None` and every row stays visible.
fn plugin_help_parts() -> (String, Vec<HelpRow<'static>>) {
    let header = "Managed OpenCode plugin commands:".to_owned();
    let rows: Vec<HelpRow<'static>> = vec![
        (
            "install",
            "Install or update the worktree adapter (default: both scopes; idempotent)",
            crate::policy::Capability::IssueRead,
        ),
        (
            "status",
            "Report global and project file presence, managed-flag match, size, and mtime (read-only)",
            crate::policy::Capability::IssueRead,
        ),
        (
            "uninstall",
            "Remove managed adapter files; refuses files without the managed marker",
            crate::policy::Capability::IssueRead,
        ),
    ];
    (header, rows)
}

/// Top-level `plugin` help body rendered through the shared group helper.
/// Contract prose (managed-marker ownership, scope defaults, adapter
/// redirect rules) lives on the per-subcommand detail pages and stays out
/// of this one-line-per-command overview.
pub(crate) fn plugin_help_text() -> String {
    let (header, rows) = plugin_help_parts();
    render_group_help(
        None,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help plugin <command>' for options.",
    )
}

pub(crate) fn print_plugin_help() {
    let (header, rows) = plugin_help_parts();
    print_group_help(
        None,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help plugin <command>' for options.",
    )
}

pub(crate) fn print_plugin_command_help(command: &str) {
    println!("{}", plugin_command_help_text(command));
}

/// Per-subcommand `plugin` help body (no trailing newline).
pub(crate) fn plugin_command_help_text(command: &str) -> String {
    match command {
        "install" => "Usage: plugin install [--global] [--project] [--force]\n\nInstalls or updates the phasegent worktree adapter in the OpenCode plugin directory. The global target is $XDG_CONFIG_HOME/opencode/plugins/phasegent-worktree.js (falling back to $HOME/.config/opencode/plugins/phasegent-worktree.js); the project target is .opencode/plugins/phasegent-worktree.js in the current working directory. When neither --global nor --project is supplied, both slots are written. Existing managed files (those that contain the `// phasegent:managed` marker) are updated in place when the embedded template changes; otherwise they are reported as skipped. Foreign files (no marker) are refused unless --force is supplied, in which case the foreign file is renamed to phasegent-worktree.js.phasegent-orig before the managed file is written. Adapter contract: the OpenCode v2 plugin shape `export default { id, setup }` (OpenCode >= 2.0; the v1 plugin contract is no longer supported). `setup` registers the `tool.execute.before` redirect hook and claims the `worktree.transform` strategy only when the checkout already carries a phasegent issue binding, so a non-phasegent project keeps the host git strategy; a failed acquire falls back to a plain git worktree. The acquired worktree becomes the session directory through `session.move`, and later tool calls have relative file paths and a bare or relative shell workdir redirected into it; absolute paths and sessions without a worktree pass through unchanged, and the external_directory permission check is never bypassed. The adapter never deletes a worktree or branch: removal stays with `phasegent worktree prune`. No role or provider is required.".to_owned(),
        "status" => "Usage: plugin status\n\nReports the global and project slots of the OpenCode worktree adapter. Each slot reports path, exists, managed (whether the file contains the `// phasegent:managed` marker), size, and mtime. The file contents are never printed. No role or provider is required.".to_owned(),
        "uninstall" => "Usage: plugin uninstall [--global] [--project]\n\nRemoves the managed worktree adapter from the OpenCode plugin directory. Files without the `// phasegent:managed` marker are refused (remove them manually if needed); missing files are reported as warnings. When neither --global nor --project is supplied, both slots are processed. No role or provider is required. The adapter contract does not delete worktrees or branches; use `phasegent worktree prune` for that.".to_owned(),
        _ => plugin_help_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_is_tabular_with_one_row_per_command() {
        let text = plugin_help_text();
        assert!(
            text.contains("Managed OpenCode plugin commands:"),
            "header must use the grouped shape; got: {text}"
        );
        for (name, desc) in [
            (
                "install",
                "Install or update the worktree adapter (default: both scopes; idempotent)",
            ),
            (
                "status",
                "Report global and project file presence, managed-flag match, size, and mtime (read-only)",
            ),
            (
                "uninstall",
                "Remove managed adapter files; refuses files without the managed marker",
            ),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "rows stay one-command-per-line; missing {name}; got: {text}"
            );
        }
        assert!(
            text.contains("Use 'phasegent --help plugin <command>' for options."),
            "footer must point to detail pages; got: {text}"
        );
    }

    #[test]
    fn overview_carries_no_issue_refs_or_flag_signatures() {
        let overview = plugin_help_text();
        for sunk in ["issue #", "#239", "[--global]", "[--project]", "[--force]"] {
            assert!(
                !overview.contains(sunk),
                "overview must not repeat contract prose ({sunk}); got: {overview}"
            );
        }
        let install = plugin_command_help_text("install");
        assert!(
            install.contains("Usage: plugin install [--global] [--project] [--force]")
                && install.contains("// phasegent:managed"),
            "install detail keeps flags + marker contract; got: {install}"
        );
        let uninstall = plugin_command_help_text("uninstall");
        assert!(
            uninstall.contains("phasegent worktree prune"),
            "uninstall detail keeps the prune pointer; got: {uninstall}"
        );
    }

    #[test]
    fn unknown_command_falls_back_to_top_level_help() {
        assert_eq!(plugin_command_help_text("fly"), plugin_help_text());
    }
}

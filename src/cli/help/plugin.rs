//! Help text for the `plugin` command group (issue #239 Phase 3; issue #22 P2
//! scope cleanup).
//!
//! The default target is the global OpenCode plugin directory. `--project`
//! selects only the project directory and `--path DIR` selects one explicit
//! directory; `--global` was removed. The overview stays tabular and the
//! detail pages carry the safety contract.

use super::common::{HelpRow, print_group_help, render_group_help};

fn plugin_help_parts() -> (String, Vec<HelpRow<'static>>) {
    let header = "Managed OpenCode plugin commands:".to_owned();
    let rows: Vec<HelpRow<'static>> = vec![
        (
            "install",
            "Install or update the worktree adapter (default: global; idempotent)",
            &["plugin", "install"],
        ),
        (
            "status",
            "Report the selected target's presence, managed flag, size, and mtime (read-only)",
            &["plugin", "status"],
        ),
        (
            "uninstall",
            "Remove the managed adapter; refuses files without the managed marker",
            &["plugin", "uninstall"],
        ),
    ];
    (header, rows)
}

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
    );
}

pub(crate) fn print_plugin_command_help(command: &str) {
    println!("{}", plugin_command_help_text(command));
}

pub(crate) fn plugin_command_help_text(command: &str) -> String {
    match command {
        "install" => "Usage: plugin install [--project] [--path DIR] [--force]\n\nThe default target is the global OpenCode plugin directory: $XDG_CONFIG_HOME/opencode/plugins/phasegent-worktree.js, falling back to $HOME/.config/opencode/plugins/phasegent-worktree.js and then, on Windows without HOME, %USERPROFILE%/.config/opencode/plugins/phasegent-worktree.js; unset or empty roots are a clear error rather than a current-directory target. --project selects only .opencode/plugins/phasegent-worktree.js in the current working directory. --path DIR selects one explicit directory that will contain phasegent-worktree.js; it cannot be combined with --project, and --path=DIR is the escape hatch for a value that begins with `-`. Existing managed files (those that contain the `// phasegent:managed` marker) are updated in place when the embedded dist changes; unchanged files are skipped. Foreign files are refused unless --force is supplied, in which case the foreign file is renamed to phasegent-worktree.js.phasegent-orig first. The generated dist lives at assets/opencode/phasegent-worktree.js; its sources are assets/opencode/src/** and the prompts are installed separately by `phasegent skill install`. The adapter never probes or installs into other agent hosts, registers no slash command, and uses the host's filesystem skill discovery instead of an embedded duplicate skill. No role or provider is required.".to_owned(),
        "status" => "Usage: plugin status [--project] [--path DIR]\n\nReports exactly one target: the global slot by default, the project slot with --project, or the explicit directory with --path DIR. Each target reports path, exists, managed (whether the file contains the `// phasegent:managed` marker), size, and mtime; file contents are never printed. The global path uses the same resolver as install and uninstall ($XDG_CONFIG_HOME, then $HOME/.config, then %USERPROFILE%/.config); an unusable environment returns an error. --path cannot be combined with --project and accepts --path=DIR for a value that begins with `-`. No role or provider is required.".to_owned(),
        "uninstall" => "Usage: plugin uninstall [--project] [--path DIR]\n\nRemoves the managed worktree adapter from exactly one target: the global slot by default, the project slot with --project, or the explicit directory with --path DIR. Files without the `// phasegent:managed` marker are refused (remove them manually if needed); missing files are reported as warnings. --path cannot be combined with --project and accepts --path=DIR for a value that begins with `-`. No role or provider is required. The adapter contract does not delete worktrees or branches; use `phasegent worktree prune` for that.".to_owned(),
        _ => plugin_help_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_is_tabular_with_one_row_per_command() {
        let text = plugin_help_text();
        assert!(text.contains("Managed OpenCode plugin commands:"), "{text}");
        for (name, desc) in [
            (
                "install",
                "Install or update the worktree adapter (default: global; idempotent)",
            ),
            (
                "status",
                "Report the selected target's presence, managed flag, size, and mtime (read-only)",
            ),
            (
                "uninstall",
                "Remove the managed adapter; refuses files without the managed marker",
            ),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "missing {name}; got: {text}"
            );
        }
    }

    #[test]
    fn install_detail_documents_default_and_explicit_targets() {
        let text = plugin_command_help_text("install");
        assert!(text.contains("Usage: plugin install [--project] [--path DIR] [--force]"));
        assert!(text.contains("default target is the global"));
        assert!(text.contains("%USERPROFILE%/.config/opencode/plugins"));
        assert!(text.contains("cannot be combined with --project"));
        assert!(text.contains("// phasegent:managed"));
        assert!(text.contains("phasegent skill install"));
        assert!(!text.contains("--global"));
        assert!(!text.contains("skill.transform"));
    }

    #[test]
    fn status_and_uninstall_document_one_selected_target() {
        assert!(plugin_command_help_text("status").contains("exactly one target"));
        assert!(plugin_command_help_text("uninstall").contains("exactly one target"));
    }

    #[test]
    fn unknown_command_falls_back_to_top_level_help() {
        assert_eq!(plugin_command_help_text("fly"), plugin_help_text());
    }
}

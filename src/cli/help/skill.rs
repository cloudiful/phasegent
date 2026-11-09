//! Help text for `phasegent skill` (issue #22 P2).
//!
//! `skill install` is a local, role-free installer for the single bundled
//! `phasegent` skill directory. There is no status or uninstall surface, and the
//! command never removes pre-existing user skills.

use super::common::{HelpRow, print_group_help, render_group_help};

fn skill_help_parts() -> (String, Vec<HelpRow<'static>>) {
    (
        "Bundled phasegent skill:".to_owned(),
        vec![(
            "install",
            "Install the single phasegent skill and its references (idempotent)",
            &["skill", "install"],
        )],
    )
}

pub(crate) fn skill_help_text() -> String {
    let (header, rows) = skill_help_parts();
    render_group_help(
        None,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help skill <command>' for options.",
    )
}

pub(crate) fn print_skill_help() {
    let (header, rows) = skill_help_parts();
    print_group_help(
        None,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help skill <command>' for options.",
    );
}

pub(crate) fn print_skill_command_help(command: &str) {
    println!("{}", skill_command_help_text(command));
}

pub(crate) fn skill_command_help_text(command: &str) -> String {
    match command {
        "install" => "Usage: skill install [--project] [--path DIR] [--force]\n\nInstalls the single bundled phasegent skill directory: SKILL.md plus on-demand references under references/. The default target is the agent home skills root, ~/.agents/skills/phasegent (HOME, then %USERPROFILE% on Windows; unset or empty roots are an error). --project selects .agents/skills/phasegent in the current working directory. --path DIR selects an explicit skills root that will contain phasegent/, and cannot be combined with --project; use --path=DIR for a value that begins with `-`. A missing, empty, or whitespace-only value is a parse error. The command is idempotent: an unchanged managed directory is skipped and changed bundled files are updated atomically. A directory without the .phasegent-managed marker is refused; with --force it is first preserved as phasegent.phasegent-orig, so no user content is lost. Symlinked roots, targets, or managed files are refused. The installer never deletes a pre-existing skill and there is no skill status or uninstall command. No role or provider is required; OpenCode discovers the installed directory on its normal filesystem skill path.".to_owned(),
        _ => skill_help_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_lists_the_single_install_row() {
        let text = skill_help_text();
        assert!(text.contains("Bundled phasegent skill:"), "{text}");
        assert!(
            text.contains("  install        Install the single phasegent skill"),
            "{text}"
        );
        assert!(
            text.contains("Use 'phasegent --help skill <command>'"),
            "{text}"
        );
    }

    #[test]
    fn install_detail_documents_scope_and_safety() {
        let text = skill_command_help_text("install");
        assert!(text.contains("Usage: skill install [--project] [--path DIR] [--force]"));
        assert!(text.contains("~/.agents/skills/phasegent"));
        assert!(text.contains("cannot be combined with --project"));
        assert!(text.contains(".phasegent-managed"));
        assert!(text.contains("phasegent.phasegent-orig"));
        assert!(text.contains("never deletes a pre-existing skill"));
        assert!(text.contains("No role or provider is required"));
    }

    #[test]
    fn unknown_command_falls_back_to_the_overview() {
        assert_eq!(skill_command_help_text("status"), skill_help_text());
    }
}

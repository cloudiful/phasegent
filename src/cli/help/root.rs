use crate::policy::Role;
use crate::providers::ProviderKind;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root usage block. The session role comes from the managed session or
/// `PHASEGENT_ROLE`, so the common invocation shape never carries a role
/// flag. `--provider` is resolved from configuration and named only in the
/// options list, so the common invocation shape never tags it on either.
const ROOT_USAGE: &str = "Usage:\n  phasegent [GLOBAL OPTIONS] <COMMAND> [ARGS]";
const ROOT_USAGE_ROLE: &str =
    "\n\nRole resolution:\n  Managed sessions supply the role; other hosts set PHASEGENT_ROLE.";

/// The root usage block for this build. The desktop shell is the packaged
/// Electron application, so there is no binary-local entry to advertise.
fn root_usage() -> String {
    format!("{ROOT_USAGE}{ROOT_USAGE_ROLE}")
}

/// Root-overview order (issue 597 Phase 2). Visibility comes from the command
/// registry; this list only pins the print order and omits the retired
/// top-level `auth`/`workflow` redirect leaves, which exist so the parser can
/// resolve their moved-error help topics. A test keeps the two in sync.
const ROOT_OVERVIEW: &[&str] = &[
    "issue", "comment", "record", "admin", "config", "doctor", "hooks", "notify", "plugin",
    "project", "status", "version", "relation", "timer", "worktree",
];

pub(crate) fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    println!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\n\n{}\n\nOptions:\n  --provider <NAME>      redmine or local (default: redmine)\n  --api-base <URL>       Override the Redmine API base\n  --repository <O/R>     Override the Git host repository for bootstrap/discovery\n  --project-id <ID>      Override the Redmine project id\n  --close-status-id <ID> Override the Redmine closed status\n  -h, --help             Print help\n  -V, --version          Print version\n\nCommands:",
        root_usage()
    );
    for &name in ROOT_OVERVIEW {
        if !crate::command::top_level_visible(name, role, provider) {
            continue;
        }
        if let Some(summary) = crate::command::top_level_summary(name) {
            println!("  {name:<23}{summary}");
        }
    }
    println!(
        "\nUse 'phasegent --help <command>' for the next level.\n\
          Provider resolution chain and machine-wide default: 'phasegent --help config provider'.\n\
          Role and credential guidance: 'phasegent --help admin'."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_usage_leads_with_the_role_less_command_shape() {
        let usage = root_usage();
        assert!(
            usage.contains("Usage:"),
            "root usage must label itself: {usage}"
        );
        let primary = usage
            .lines()
            .nth(1)
            .expect("root usage must carry a primary invocation line");
        assert!(
            !primary.contains("--role"),
            "the primary usage line must not carry a role prefix: {primary}"
        );
        assert!(
            primary.contains("phasegent [GLOBAL OPTIONS] <COMMAND> [ARGS]"),
            "the primary usage line must keep the global-option/command shape: {primary}"
        );
        assert!(
            !usage.contains("--provider"),
            "root usage must not tag --provider onto the common invocation: {usage}"
        );
    }

    /// The desktop shell is a separate Electron application: no root-usage
    /// line may advertise a binary-local desktop entry.
    #[test]
    fn root_usage_has_no_desktop_entry() {
        let usage = root_usage();
        assert!(
            !usage.contains("gui") && !usage.contains("desktop"),
            "root usage must not advertise a desktop entry: {usage}"
        );
    }

    #[test]
    fn root_usage_names_the_role_source() {
        let usage = root_usage();
        assert!(
            usage.contains("Managed sessions supply the role") && usage.contains("PHASEGENT_ROLE"),
            "root usage must state that managed sessions supply the role and other hosts set PHASEGENT_ROLE: {usage}"
        );
        assert!(
            !usage.contains("--role"),
            "root usage must not document a role flag: {usage}"
        );
    }

    /// The overview order must stay complete: every registered top-level
    /// command is listed except the retired `auth`/`workflow` redirect leaves.
    #[test]
    fn root_overview_covers_every_surface_command() {
        let mut expected: Vec<&str> = crate::command::top_level_names()
            .into_iter()
            .filter(|name| !matches!(*name, "auth" | "workflow"))
            .collect();
        let mut listed: Vec<&str> = ROOT_OVERVIEW.to_vec();
        expected.sort_unstable();
        listed.sort_unstable();
        assert_eq!(
            listed, expected,
            "root overview must list every real top-level command exactly once"
        );
    }
}

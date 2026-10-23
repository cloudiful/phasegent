use crate::policy::Role;
use crate::providers::ProviderKind;

use super::common::{HelpRow, HelpSection, render_group_help_visible};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root usage block. Ordinary human CLI needs no agent role: without
/// `PHASEGENT_ROLE` and without `PHASEGENT_SESSION_ID` the invocation is
/// the unmanaged Operator, so the common invocation shape never carries a
/// role flag. `--provider` is resolved from configuration and named only in
/// the options list, so the common invocation shape never tags it on either.
const ROOT_USAGE: &str = "Usage:\n  phasegent [GLOBAL OPTIONS] <COMMAND> [ARGS]";
const ROOT_USAGE_ROLE: &str = "\n\nCaller context:\n  No role and no session runs as Operator (ordinary human CLI, no --authorized needed except records); explicit PHASEGENT_ROLE selects agent-filtered help and scoped config; managed sessions without a role fail closed. PHASEGENT_ROLE/PHASEGENT_SESSION_ID are routing, not authentication. See 'phasegent --help admin' and docs/cli-caller-context.md.";

/// The root usage block for this build.
fn root_usage() -> String {
    format!("{ROOT_USAGE}{ROOT_USAGE_ROLE}")
}

/// The root overview groups, in print order. Every registered top-level
/// command appears exactly once; grouping is presentation only, and the
/// command registry still decides which role and provider see a row, so no
/// command becomes available or unavailable because of its group. The retired
/// top-level `auth`/`workflow` redirect leaves are omitted; they exist so the
/// parser can resolve their moved-error help topics. A test keeps the two in
/// sync.
const ROOT_GROUPS: &[(&str, &[&str])] = &[
    (
        "Work items",
        &[
            "issue", "comment", "record", "project", "status", "version", "relation",
        ],
    ),
    ("Orchestration", &["timer", "worktree"]),
    (
        "Configuration and operations",
        &["admin", "config", "notify"],
    ),
    ("Meta", &["doctor", "hooks", "plugin"]),
];

/// The root overview names in print order.
#[cfg(test)]
fn root_overview() -> impl Iterator<Item = &'static str> {
    ROOT_GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter().copied())
}

/// The global options block. The presentation options and the pointer to the
/// output contract live here with the provider options, so one page answers
/// "what can I pass at the root".
const ROOT_OPTIONS: &str = "Options:\n  --provider <NAME>      redmine or local (default: redmine)\n  --api-base <URL>       Override the Redmine API base\n  --repository <O/R>     Override the Git host repository for bootstrap/discovery\n  --project-id <ID>      Override the Redmine project id\n  --close-status-id <ID> Override the Redmine closed status\n  --output <MODE>        Output mode: auto (default), text, or json\n  --no-progress          Disable the progress spinner\n  -h, --help             Print help\n  -V, --version          Print version";

/// The root footer: the next help levels, the resolver and role pages, and the
/// output contract.
const ROOT_FOOTER: &str = "Use 'phasegent --help <command>' for the next level.\nProvider resolution chain and machine-wide default: 'phasegent --help config provider'.\nRole and credential guidance: 'phasegent --help admin'.\nOutput modes and the stable JSON contract: docs/cli-output.md.";

/// The description column of the root rows, aligned with the options block.
const ROOT_NAME_WIDTH: usize = 22;

pub(crate) fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    let header = format!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\n\n{}\n\n{ROOT_OPTIONS}",
        root_usage()
    );
    let mut rows_by_group: Vec<Vec<HelpRow<'_>>> = Vec::with_capacity(ROOT_GROUPS.len());
    for (_, names) in ROOT_GROUPS {
        let rows = names
            .iter()
            .filter_map(|name| {
                // The visibility closure gates by name through the registry,
                // so a root row carries no deeper registry path.
                crate::command::top_level_summary(name).map(|summary| (*name, summary, &[][..]))
            })
            .collect();
        rows_by_group.push(rows);
    }
    let sections: Vec<HelpSection<'_>> = ROOT_GROUPS
        .iter()
        .zip(rows_by_group.iter())
        .map(|((title, _), rows)| (Some(*title), rows.as_slice()))
        .collect();
    print!(
        "{}",
        render_group_help_visible(
            &header,
            &sections,
            ROOT_FOOTER,
            ROOT_NAME_WIDTH,
            |name, _| { crate::command::top_level_visible(name, role, provider) }
        )
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

    #[test]
    fn root_usage_names_the_role_source() {
        let usage = root_usage();
        assert!(
            usage.contains("runs as Operator")
                && usage.contains("PHASEGENT_ROLE")
                && usage.contains("fail closed"),
            "root usage must state Operator default, explicit PHASEGENT_ROLE, and managed fail-closed: {usage}"
        );
        assert!(
            usage.contains("routing, not authentication"),
            "root usage must state role env is routing, not authentication: {usage}"
        );
        assert!(
            !usage.contains("--role"),
            "root usage must not document a role flag: {usage}"
        );
    }

    /// The options block advertises the presentation switches and points at
    /// the output contract next to the provider options.
    #[test]
    fn root_options_document_the_output_contract() {
        assert!(
            ROOT_OPTIONS
                .contains("--output <MODE>        Output mode: auto (default), text, or json"),
            "the root options must document --output: {ROOT_OPTIONS}"
        );
        assert!(
            ROOT_OPTIONS.contains("--no-progress          Disable the progress spinner"),
            "the root options must document --no-progress: {ROOT_OPTIONS}"
        );
        assert!(
            ROOT_FOOTER.contains("docs/cli-output.md"),
            "the root footer must point at the output contract: {ROOT_FOOTER}"
        );
    }

    /// The overview groups must stay complete: every registered top-level
    /// command is listed once, except the retired `auth`/`workflow` redirect
    /// leaves.
    #[test]
    fn root_overview_covers_every_surface_command() {
        let mut expected: Vec<&str> = crate::command::top_level_names()
            .into_iter()
            .filter(|name| !matches!(*name, "auth" | "workflow"))
            .collect();
        let mut listed: Vec<&str> = root_overview().collect();
        expected.sort_unstable();
        listed.sort_unstable();
        assert_eq!(
            listed, expected,
            "root overview must list every real top-level command exactly once"
        );
    }

    /// A command belongs to exactly one group, and every group keeps its
    /// title, so the overview renders a stable four-section page.
    #[test]
    fn root_groups_are_titled_and_disjoint() {
        let titles: Vec<&str> = ROOT_GROUPS.iter().map(|(title, _)| *title).collect();
        assert_eq!(
            titles,
            [
                "Work items",
                "Orchestration",
                "Configuration and operations",
                "Meta"
            ],
            "the root overview keeps its four titled groups in order"
        );
        let mut seen: Vec<&str> = Vec::new();
        for (_, names) in ROOT_GROUPS {
            assert!(!names.is_empty(), "a root group must own at least one row");
            for name in *names {
                assert!(
                    !seen.contains(name),
                    "{name} must appear in exactly one root group"
                );
                seen.push(name);
            }
        }
    }
}

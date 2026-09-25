use crate::policy::Role;

pub(crate) fn print_not_supported_help(operation: &str) {
    println!("No command available for Redmine: {operation} is Forgejo-only.");
}

/// One row in a help group: command name, one-line description, and the
/// registry path used for the role gate. The registry owns the gate, so the
/// overview and the parser can never disagree about which role sees a row.
pub(crate) type HelpRow<'a> = (&'a str, &'a str, &'static [&'static str]);
/// One section in a help group: optional title (None = untitled first
/// block) plus its rows.
pub(crate) type HelpSection<'a> = (Option<&'a str>, &'a [HelpRow<'a>]);

/// Whether a row is visible for the resolved role. No role keeps the
/// compatibility superset view.
fn row_visible(role: Option<Role>, path: &[&str]) -> bool {
    role.is_none_or(|role| crate::command::registry_allows_role(role, path))
}

/// Render a `comment`/`status`-style group overview without printing, so the
/// shape is testable without capturing stdout.
pub(crate) fn render_group_help(
    role: Option<Role>,
    header: &str,
    sections: &[HelpSection<'_>],
    footer: &str,
) -> String {
    let mut out = format!("{header}\n\n");
    for (index, (title, rows)) in sections.iter().enumerate() {
        let visible: Vec<&HelpRow<'_>> = rows
            .iter()
            .filter(|(_, _, path)| row_visible(role, path))
            .collect();
        if visible.is_empty() {
            continue;
        }
        if let Some(title) = title {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&format!("{title}:\n"));
        }
        for (name, desc, _) in visible {
            out.push_str(&format!("  {name:<14} {desc}\n"));
        }
    }
    out.push_str(&format!("\n{footer}\n"));
    out
}

/// Print a `comment`/`status`-style group overview: header line, titled
/// sections of `(name, desc)` rows gated by the registry, and a footer
/// pointer to the per-command detail pages.
pub(crate) fn print_group_help(
    role: Option<Role>,
    header: &str,
    sections: &[HelpSection<'_>],
    footer: &str,
) {
    print!("{}", render_group_help(role, header, sections, footer));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sections_render_header_and_footer_only() {
        let text = render_group_help(None, "H:", &[], "F");
        assert!(text.contains("H:"), "got: {text}");
        assert!(text.contains('F'), "got: {text}");
        assert!(
            !text.contains("  "),
            "empty groups must print no rows; got: {text}"
        );
    }

    #[test]
    fn all_roles_show_every_row() {
        let rows: &[HelpRow<'_>] = &[
            ("get", "Read one issue", &["issue", "get"]),
            ("create", "Create an issue", &["issue", "create"]),
        ];
        let text = render_group_help(None, "H:", &[(None, rows)], "F");
        assert!(text.contains("get"), "got: {text}");
        assert!(text.contains("create"), "got: {text}");
    }

    #[test]
    fn role_filtering_hides_denied_commands() {
        let rows: &[HelpRow<'_>] = &[
            ("get", "Read one issue", &["issue", "get"]),
            ("create", "Create an issue", &["issue", "create"]),
        ];
        let text = render_group_help(Some(Role::Executor), "H:", &[(None, rows)], "F");
        assert!(
            text.contains("get"),
            "executor keeps issue get; got: {text}"
        );
        assert!(
            !text.contains("create"),
            "executor denies issue create; got: {text}"
        );
        let full = render_group_help(Some(Role::Orchestrator), "H:", &[(None, rows)], "F");
        assert!(
            full.contains("get") && full.contains("create"),
            "orchestrator sees all; got: {full}"
        );
    }

    #[test]
    fn multiple_sections_keep_title_order() {
        let first: &[HelpRow<'_>] = &[("get", "Read one issue", &["issue", "get"])];
        let second: &[HelpRow<'_>] = &[("bind", "Bind branch", &["issue", "bind"])];
        let text = render_group_help(None, "H:", &[(None, first), (Some("Local"), second)], "F");
        let header_at = text.find("H:").expect("header");
        let get_at = text.find("get").expect("first row");
        let local_at = text.find("Local:").expect("second title");
        let bind_at = text.find("bind").expect("second row");
        let footer_at = text.find('F').expect("footer");
        assert!(
            header_at < get_at && get_at < local_at && local_at < bind_at && bind_at < footer_at,
            "sections must keep header/rows/title/rows/footer order; got: {text}"
        );
    }
}

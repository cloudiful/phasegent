//! Read-only freshness check for the local OpenCode integration artifacts.
//!
//! The CLI compares the installed plugin adapter and the bundled skill with
//! the artifacts this build embeds and reports one bounded aggregate warning
//! on stderr. The comparison is local only: it never looks up the latest
//! release on the internet and never inspects what the running host loaded.
//!
//! The check is a pure policy over four observations (plugin and skill, each
//! in the global and the current-project scope), so the regression tests stay
//! in-process.
use std::io::Read;
use std::path::Path;

/// One installed copy of an integration artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlotStatus {
    /// The slot holds no copy at all.
    Missing,
    /// The slot holds a managed copy that matches the embedded artifact.
    Current,
    /// The slot holds a managed copy that differs from the embedded artifact.
    Stale,
    /// The slot holds a managed copy that is only partly present.
    Incomplete,
    /// The slot could not be observed, or holds something phasegent does not
    /// manage. An unknown slot never becomes a missing or stale claim.
    Unknown,
}

/// The two artifact families the CLI embeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Artifact {
    /// The OpenCode worktree adapter.
    Plugin,
    /// The bundled `phasegent` skill directory.
    Skill,
}

/// Where a copy lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    /// The user-level installation.
    Global,
    /// The installation inside the current checkout.
    Project,
}

/// The observed state of one slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Observation {
    pub artifact: Artifact,
    pub scope: Scope,
    pub status: SlotStatus,
}

/// One reported problem about one artifact family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Finding {
    pub artifact: Artifact,
    pub status: SlotStatus,
    /// The scopes the problem was observed in.
    pub scopes: Vec<Scope>,
}

/// Classify observed bytes against the embedded artifact.
///
/// Equal bytes are current, a proper prefix is an incomplete copy, and any
/// other difference is a stale copy.
pub(crate) fn classify_bytes(observed: &[u8], expected: &[u8]) -> SlotStatus {
    if observed == expected {
        return SlotStatus::Current;
    }
    if observed.len() < expected.len() && expected.starts_with(observed) {
        return SlotStatus::Incomplete;
    }
    SlotStatus::Stale
}

/// Whether the head of a file is exactly the managed marker.
///
/// Ownership is decided on the marker bytes alone, so a foreign body is never
/// read past its marker.
pub(crate) fn owns_managed_copy(head: &[u8], marker: &[u8]) -> bool {
    head == marker
}

/// Whether the observed skill marker file equals the expected bytes in full.
///
/// The skill marker is a whole file, not a header: a file that carries the
/// marker plus anything else is a foreign directory.
pub(crate) fn owns_managed_skill_marker(observed: &[u8], expected: &[u8]) -> bool {
    observed == expected
}

/// Fold the observations into the reported problems.
///
/// A family is missing only when every scope is resolved and absent, so an
/// unknown slot never claims a missing copy. A stale or incomplete managed
/// copy is reported even when another scope holds a current copy.
pub(crate) fn aggregate(observations: &[Observation]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for artifact in [Artifact::Plugin, Artifact::Skill] {
        let scopes: Vec<Scope> = observations
            .iter()
            .filter(|observation| observation.artifact == artifact)
            .map(|observation| observation.scope)
            .collect();
        if scopes.is_empty() {
            continue;
        }
        for status in [SlotStatus::Stale, SlotStatus::Incomplete] {
            let affected: Vec<Scope> = observations
                .iter()
                .filter(|observation| {
                    observation.artifact == artifact && observation.status == status
                })
                .map(|observation| observation.scope)
                .collect();
            if !affected.is_empty() {
                findings.push(Finding {
                    artifact,
                    status,
                    scopes: affected,
                });
            }
        }
        let absent = observations
            .iter()
            .filter(|observation| observation.artifact == artifact)
            .filter(|observation| observation.status == SlotStatus::Missing)
            .count();
        if absent == scopes.len() {
            findings.push(Finding {
                artifact,
                status: SlotStatus::Missing,
                scopes,
            });
        }
    }
    findings
}

/// The warning text for the reported problems.
///
/// The text names the scope but never a filesystem path, never file content,
/// and never a session value.
pub(crate) fn render(findings: &[Finding], cli_version: &str) -> String {
    let parts: Vec<String> = findings.iter().take(4).map(describe).collect();
    format!(
        "the local OpenCode integration does not match this phasegent CLI ({cli_version}): {}. \
The comparison uses the local files and the artifacts embedded in this CLI only; it does not \
check the latest release on the internet and does not report what OpenCode has loaded.",
        parts.join("; ")
    )
}

/// The refresh guidance for one finding.
///
/// A stale or incomplete copy is refreshed in the scope it was observed in,
/// so the guidance never points at the wrong scope. A missing copy is absent
/// in every scope, so either scope is offered as an alternative.
pub(crate) fn refresh_hint(artifact: Artifact, status: SlotStatus, scopes: &[Scope]) -> String {
    let commands: Vec<String> = scopes
        .iter()
        .map(|scope| {
            format!(
                "`{}`",
                match (artifact, scope) {
                    (Artifact::Plugin, Scope::Global) => "phasegent plugin install",
                    (Artifact::Plugin, Scope::Project) => "phasegent plugin install --project",
                    (Artifact::Skill, Scope::Global) => "phasegent skill install",
                    (Artifact::Skill, Scope::Project) => "phasegent skill install --project",
                }
            )
        })
        .collect();
    match status {
        SlotStatus::Missing => format!("install it in one scope: {}", commands.join(" or ")),
        SlotStatus::Stale | SlotStatus::Incomplete => format!("refresh {}", commands.join(" and ")),
        SlotStatus::Current | SlotStatus::Unknown => String::new(),
    }
}

fn describe(finding: &Finding) -> String {
    let label = match finding.artifact {
        Artifact::Plugin => "the OpenCode plugin",
        Artifact::Skill => "the phasegent skill",
    };
    let head = match finding.status {
        SlotStatus::Missing => format!("{label} is not installed in any scope"),
        SlotStatus::Stale => format!(
            "{label} differs from this build in the {}",
            scopes(&finding.scopes)
        ),
        SlotStatus::Incomplete => format!(
            "{label} is only partly installed in the {}",
            scopes(&finding.scopes)
        ),
        SlotStatus::Current | SlotStatus::Unknown => format!("{label} is current"),
    };
    let hint = refresh_hint(finding.artifact, finding.status, &finding.scopes);
    if hint.is_empty() {
        head
    } else {
        format!("{head} ({hint})")
    }
}

fn scopes(scopes: &[Scope]) -> String {
    let names: Vec<&str> = scopes
        .iter()
        .map(|scope| match scope {
            Scope::Global => "global scope",
            Scope::Project => "project scope",
        })
        .collect();
    match names.len() {
        0 => "unknown scope".to_owned(),
        1 => names[0].to_owned(),
        _ => format!("{} scopes", names.join(" and ")),
    }
}

/// Whether the host marker is present in the environment.
///
/// The marker classifies the shell only; it never carries an identity and
/// never grants a role.
fn host_marker_present() -> bool {
    std::env::var_os(crate::policy::HOST_SESSION_ENV).is_some()
}

/// Emit the warning for the current environment when the host marker is
/// present and the command is not suppressed.
pub(crate) fn warn_unless_suppressed(command: &crate::command::Command) {
    if !host_marker_present() || is_suppressed(command) {
        return;
    }
    warn();
}

/// Emit the warning for a managed session without a role whose business
/// command was denied at parse time.
pub(crate) fn warn_for_denied_command() {
    if !host_marker_present() {
        return;
    }
    warn();
}

/// The commands that never carry the warning: the help surface, the binary
/// version, the plugin operations, and the skill installation.
fn is_suppressed(command: &crate::command::Command) -> bool {
    matches!(
        command,
        crate::command::Command::Help(_)
            | crate::command::Command::Version
            | crate::command::Command::Plugin(_)
            | crate::command::Command::Skill(_)
    )
}

/// Observe every slot and report the aggregate warning.
fn warn() {
    let observations = observe();
    let findings = aggregate(&observations);
    if findings.is_empty() {
        return;
    }
    let message = render(&findings, env!("CARGO_PKG_VERSION"));
    crate::cli::report_local_warnings("integration", Some(message));
}

/// Observe the plugin and the skill in the global and the project scope.
fn observe() -> Vec<Observation> {
    let current = std::env::current_dir().ok();
    vec![
        Observation {
            artifact: Artifact::Plugin,
            scope: Scope::Global,
            status: plugin_global(),
        },
        Observation {
            artifact: Artifact::Plugin,
            scope: Scope::Project,
            status: match &current {
                Some(cwd) => plugin_slot(&crate::plugin::resolve_project_dir(cwd)),
                None => SlotStatus::Unknown,
            },
        },
        Observation {
            artifact: Artifact::Skill,
            scope: Scope::Global,
            status: skill_global(),
        },
        Observation {
            artifact: Artifact::Skill,
            scope: Scope::Project,
            status: match &current {
                Some(cwd) => skill_slot(&crate::skill_install::resolve_project_skills_root(cwd)),
                None => SlotStatus::Unknown,
            },
        },
    ]
}

/// The global plugin slot.
fn plugin_global() -> SlotStatus {
    match crate::plugin::resolve_global_dir() {
        Ok(dir) => plugin_slot(&dir),
        Err(_) => SlotStatus::Unknown,
    }
}

/// The plugin slot inside one plugin directory.
fn plugin_slot(dir: &Path) -> SlotStatus {
    let path = dir.join(crate::plugin::PLUGIN_FILENAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SlotStatus::Missing,
        Err(_) => return SlotStatus::Unknown,
    };
    // A symlinked or non-regular slot is never followed and never read.
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return SlotStatus::Unknown;
    }
    let marker = crate::plugin::managed_marker_bytes();
    // Ownership is decided on the marker bytes alone, so a foreign body is
    // never read.
    let head = match read_bounded(&path, marker.len()) {
        Some(head) => head,
        None => return SlotStatus::Unknown,
    };
    if !owns_managed_copy(&head, marker) {
        return SlotStatus::Unknown;
    }
    let expected = crate::plugin::embedded_adapter_bytes();
    match read_bounded(&path, expected.len() + 1) {
        Some(observed) => classify_bytes(&observed, expected),
        None => SlotStatus::Unknown,
    }
}

/// The global skill slot.
fn skill_global() -> SlotStatus {
    match crate::skill_install::resolve_global_skills_root() {
        Ok(root) => skill_slot(&root),
        Err(_) => SlotStatus::Unknown,
    }
}

/// The skill slot inside one skills root.
fn skill_slot(root: &Path) -> SlotStatus {
    let directory = root.join(crate::skill_install::SKILL_DIR_NAME);
    let metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SlotStatus::Missing,
        Err(_) => return SlotStatus::Unknown,
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return SlotStatus::Unknown;
    }
    let marker_path = directory.join(crate::skill_install::MANAGED_MARKER_FILE);
    let marker_metadata = match std::fs::symlink_metadata(&marker_path) {
        Ok(metadata) => metadata,
        Err(_) => return SlotStatus::Unknown,
    };
    if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
        return SlotStatus::Unknown;
    }
    let expected_marker = crate::skill_install::managed_marker_bytes();
    // The marker file must equal the expected bytes in full: the installer
    // owns a directory only on complete equality, so one extra byte makes it
    // a foreign directory whose bodies are never read.
    match read_bounded(&marker_path, expected_marker.len() + 1) {
        Some(marker) if owns_managed_skill_marker(&marker, expected_marker) => {}
        _ => return SlotStatus::Unknown,
    }
    let mut incomplete = false;
    let mut stale = false;
    for file in crate::skill_install::bundled_files() {
        let path = directory.join(file.relative);
        // A symlinked subdirectory or file is never followed.
        if let Some(parent) = path.parent()
            && let Ok(parent_metadata) = std::fs::symlink_metadata(parent)
            && (parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir())
        {
            return SlotStatus::Unknown;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                incomplete = true;
                continue;
            }
            Err(_) => return SlotStatus::Unknown,
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return SlotStatus::Unknown;
        }
        let expected = file.content.as_bytes();
        match read_bounded(&path, expected.len() + 1) {
            Some(observed) => match classify_bytes(&observed, expected) {
                SlotStatus::Current => {}
                SlotStatus::Incomplete => incomplete = true,
                _ => stale = true,
            },
            None => return SlotStatus::Unknown,
        }
    }
    if incomplete {
        SlotStatus::Incomplete
    } else if stale {
        SlotStatus::Stale
    } else {
        SlotStatus::Current
    }
}

/// Read at most `limit` bytes from a file, without following a symlink.
fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut buffer = Vec::new();
    file.take(limit as u64).read_to_end(&mut buffer).ok()?;
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(artifact: Artifact, scope: Scope, status: SlotStatus) -> Observation {
        Observation {
            artifact,
            scope,
            status,
        }
    }

    #[test]
    fn byte_classification_separates_current_stale_and_incomplete() {
        let expected = b"managed payload";
        assert_eq!(
            classify_bytes(b"managed payload", expected),
            SlotStatus::Current
        );
        assert_eq!(
            classify_bytes(b"managed pay", expected),
            SlotStatus::Incomplete
        );
        assert_eq!(
            classify_bytes(b"managed payload!", expected),
            SlotStatus::Stale
        );
        assert_eq!(classify_bytes(b"other", expected), SlotStatus::Stale);
    }

    #[test]
    fn the_embedded_plugin_marker_grants_ownership_only_at_the_head() {
        let marker = crate::plugin::managed_marker_bytes();
        let embedded = crate::plugin::embedded_adapter_bytes();
        assert!(owns_managed_copy(marker, marker));
        assert!(owns_managed_copy(&embedded[..marker.len()], marker));
        assert!(!owns_managed_copy(
            b"// other\n// phasegent:managed\n",
            marker
        ));
        assert!(!owns_managed_copy(
            b"// phasegent:managed-but-not\n",
            marker
        ));
        assert!(!owns_managed_copy(b"// phasegent:manag", marker));
    }

    #[test]
    fn the_skill_marker_must_equal_the_whole_file() {
        let marker = crate::skill_install::managed_marker_bytes();
        assert!(owns_managed_skill_marker(marker, marker));
        let mut oversized = marker.to_vec();
        oversized.push(b'\n');
        assert!(!owns_managed_skill_marker(&oversized, marker));
        assert!(!owns_managed_skill_marker(
            &marker[..marker.len() - 1],
            marker
        ));
        assert!(!owns_managed_skill_marker(b"", marker));
    }

    #[test]
    fn every_absent_scope_reports_a_missing_copy() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Missing),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Missing),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Missing),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Missing),
        ]);
        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .any(|finding| finding.artifact == Artifact::Plugin
                    && finding.status == SlotStatus::Missing)
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.artifact == Artifact::Skill
                    && finding.status == SlotStatus::Missing)
        );
    }

    #[test]
    fn a_current_copy_suppresses_the_absent_scope() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Missing),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Current),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Missing),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Current),
        ]);
        assert!(findings.is_empty());
    }

    #[test]
    fn an_unknown_scope_never_claims_a_missing_copy() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Unknown),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Missing),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Unknown),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Missing),
        ]);
        assert!(findings.is_empty());
    }

    #[test]
    fn a_stale_copy_is_reported_next_to_a_current_one() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Stale),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Current),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Current),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Current),
        ]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, SlotStatus::Stale);
        assert_eq!(findings[0].scopes, vec![Scope::Global]);
    }

    #[test]
    fn an_incomplete_copy_is_reported_on_its_own() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Current),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Current),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Incomplete),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Current),
        ]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].artifact, Artifact::Skill);
        assert_eq!(findings[0].status, SlotStatus::Incomplete);
    }

    #[test]
    fn the_refresh_guidance_follows_the_observed_scope() {
        assert_eq!(
            refresh_hint(Artifact::Plugin, SlotStatus::Stale, &[Scope::Global]),
            "refresh `phasegent plugin install`"
        );
        assert_eq!(
            refresh_hint(Artifact::Plugin, SlotStatus::Stale, &[Scope::Project]),
            "refresh `phasegent plugin install --project`"
        );
        assert_eq!(
            refresh_hint(Artifact::Skill, SlotStatus::Incomplete, &[Scope::Project]),
            "refresh `phasegent skill install --project`"
        );
        assert_eq!(
            refresh_hint(Artifact::Skill, SlotStatus::Incomplete, &[Scope::Global]),
            "refresh `phasegent skill install`"
        );
    }

    #[test]
    fn an_absent_copy_offers_the_scopes_as_alternatives() {
        let hint = refresh_hint(
            Artifact::Plugin,
            SlotStatus::Missing,
            &[Scope::Global, Scope::Project],
        );
        assert_eq!(
            hint,
            "install it in one scope: `phasegent plugin install` or `phasegent plugin install --project`"
        );
        assert!(!hint.contains(" and "));
    }

    #[test]
    fn the_warning_names_the_scope_and_states_the_local_comparison() {
        let findings = aggregate(&[
            observation(Artifact::Plugin, Scope::Global, SlotStatus::Stale),
            observation(Artifact::Plugin, Scope::Project, SlotStatus::Current),
            observation(Artifact::Skill, Scope::Global, SlotStatus::Missing),
            observation(Artifact::Skill, Scope::Project, SlotStatus::Missing),
        ]);
        let message = render(&findings, "1.2.3");
        assert!(message.contains("global scope"));
        assert!(message.contains("not installed"));
        assert!(message.contains("1.2.3"));
        assert!(message.contains("does not check the latest release on the internet"));
        assert!(message.contains("does not report what OpenCode has loaded"));
        assert!(message.contains("`phasegent plugin install`"));
        assert!(message.contains("`phasegent skill install`"));
        assert!(message.contains("`phasegent skill install --project`"));
        assert!(!message.contains('/'));
    }

    #[test]
    fn the_suppressed_commands_carry_no_warning() {
        use crate::command::{Command, HelpTopic, PluginCommand, SkillCommand};
        assert!(is_suppressed(&Command::Help(HelpTopic::Root)));
        assert!(is_suppressed(&Command::Version));
        assert!(is_suppressed(&Command::Plugin(PluginCommand::Status {
            project: false,
            path: None,
        })));
        assert!(is_suppressed(&Command::Skill(SkillCommand::Install {
            project: false,
            force: false,
            path: None,
        })));
        assert!(!is_suppressed(&Command::Doctor));
    }
}

//! Help text for the `worktree` command group.
//!
//! The overview lists every subcommand and its role gate. Per-subcommand help
//! describes the directory registry, fresh host occupancy, and the shared
//! cleanup guards rather than the retired lease state.

use super::common::{HelpRow, print_group_help, render_group_help};
use crate::policy::Role;

const ACQUIRE_HELP: &str = "Usage: worktree acquire --issue N [--session S] [--base REF] [--reuse] [--isolate] [--no-sync] [--format json]\n\nAcquire (or reuse) a registered worktree directory for the issue and record the caller's session; the session comes from --session or PHASEGENT_SESSION_ID and must be explicit. Idempotent: re-running with the same repository, issue, and session returns the same directory_id (reason=\"idempotent\"). The default is reuse-current-unless-the-target-path-is-occupied: the current checkout is reused (reason=\"no_conflict\") whether it is clean, dirty, unbound, or linked to a historical issue. Fresh occupancy reported by the connected service, or an associated task that is still open, protects the path; a confirmed closed historical task does not veto reuse by itself. An unresolved occupation of the checkout path is reported as an `isolation` error naming this command with `--isolate`; the command never creates a worktree unless `--isolate` was passed explicitly, in which case it allocates a fresh `phasegent/<issue>-<short6hex>` branch and a new worktree under ~/.cache/phasegent/worktrees/<fingerprint>/<slug> (reason=\"new_worktree\"). Dirty state is advisory only: it warns on stderr but never forces isolation, never stashes, and never mutates branches. `--reuse` states the reuse preference explicitly and `--isolate` forces a fresh worktree; the two are mutually exclusive, and `--reuse` never bypasses an occupied checkout. `--base REF` only selects the start point of the explicit `--isolate` worktree; without `--isolate` it is rejected and never creates one. When the `git status` probe itself fails the dirty state is unknown: `--isolate` creates a fresh worktree, otherwise the current checkout is reused, and both emit a stderr warning — an unknown status is never silently treated as clean. On every successful acquire the managed commit hooks are installed when the acquired checkout has a git origin, so one command leaves the checkout ready; the install reuses the standard hook helper and degrades to a warning that never fails the acquire. A provider-scoped Forgejo or GitLab acquisition writes the durable task bound to the issue in this repository and refuses before any registry write or taxi pass when the stored task contradicts the selected scope; task ownership applies for every provider and without repeating --provider. The command never writes branch/issue ownership outside the registry. Authorized Operator/orchestrator callers run the repository's issue sync pass first and forward its warnings to stderr; --no-sync skips it. Orchestrator-only for agents; ordinary human Operator shares this authority, managed sessions without a role fail closed. See docs/worktree-maintenance.md and docs/cli-caller-context.md.";

const STATUS_HELP: &str = "Usage: worktree status --issue N [--page N] [--limit N]\n\nRead the registered worktree directories associated with the issue. Returns `{items,page,limit,total_count,has_more,issue}` with page 1 and limit 50 by default. Each item carries directory_id, repo_identity, checkout_path, worktree_path, branch, created_at, tasks, legacy_issues, and sessions. --page is positive and 1-based; --limit is 1..100. An out-of-range page is empty and retains the exact total. The read never creates, migrates, or writes a store or lock file. Available to orchestrator, executor, and reviewer. Ordinary human Operator shares this read surface; managed sessions without a role fail closed. See docs/cli-pagination.md and docs/cli-caller-context.md.";

const PROBE_HELP: &str = "Usage: worktree probe [--path PATH | --issue N [--session S]]\n\nRead-only diagnostic for one worktree. With --path PATH it probes that directory; with --issue N [--session S] it resolves the verified registry directory for the current repository identity, the issue, and the optional session and probes that path; with no selector it probes the current checkout. --path and --issue are mutually exclusive, and --session requires --issue. Prints one bounded JSON document with resolved/path/exists/is_git_worktree/clean/branch/head/is_main_checkout/directory/errors: directory is the matching registry record or null, clean is true, false, or null (unknown), and a Git or filesystem failure is reported under errors instead of aborting, so a missing or non-Git path still returns a structured result. It never calls a provider, never writes a directory or session row, never syncs, and never deletes or repairs a directory, branch, or checkout. When --issue matches no registry directory the result is the stable empty envelope (resolved=false, path=null) and no path is guessed. Available to orchestrator, executor, and reviewer.";

const LIST_HELP: &str = "Usage: worktree list [--repo PATH] [--no-sync] [--page N] [--limit N]\n\nList registered worktree directories for the resolved repo identity. --repo defaults to the current working directory; the value is canonicalised through `git rev-parse --git-common-dir` so the same physical repository yields the same identity from a main checkout, a linked worktree, or a subdirectory. Returns `{items,page,limit,total_count,has_more,repo_identity}` with page 1 and limit 50 by default. Each item carries directory_id, repo_identity, checkout_path, worktree_path, branch, created_at, tasks, legacy_issues, and sessions. --page is positive and 1-based; --limit is 1..100. An out-of-range page is empty and retains the exact total. The read never creates, migrates, or writes a store or lock file. Available to orchestrator, executor, and reviewer. Authorized Operator/orchestrator callers run a complete repository-scoped `issue sync` before listing and forward its warnings to stderr; --no-sync skips it. Pagination does not limit that sync pass. See docs/cli-pagination.md and docs/worktree-maintenance.md.";

const PRUNE_HELP: &str = "Usage: worktree prune [--repo PATH] [--dryrun] [--json|--format json] [--verbose] [--remove] [--no-sync]\n\nGlobal, removal-by-default cleanup of registered worktrees, grouped by canonical directory. Without --repo it enumerates every repository identity the directory registry records (even outside Git) and resolves each through its main checkout, skipping an identity it cannot verify with a bounded reason instead of guessing; --repo PATH restricts the pass to one verified checkout. A directory is removed only when it is a verified non-main, clean directory (the narrow root `.cache` exception is preserved); every associated task is confirmed closed through its own durable task or a proven legacy route; a fresh host occupancy check reports no running or unassociated session in it; and every present idle session the directory's registry associations name is safely relocated to the verified main checkout. A confirmed closed historical task does not keep the directory; a still-open associated task, an unavailable or incomplete occupancy listing, an unresolved session, a mismatched path or session id, a running session, or an unconfirmed move keeps it. A proven absent path has its directory registration and owned associations reclaimed under the operation lock after a fresh row and path check; dry-run reports would-reclaim without writes. An absent main registration is a stale row like any other and is reclaimed the same way, while an existing main checkout stays excluded from the candidate and kept totals. Inaccessible paths and symlinks stay. Existing directories in unverifiable repositories remain protected. Independent histories are preserved; absence requires no provider read or move. The scan reuses one running-session snapshot and destructive boundaries check fresh occupancy. A cache-bearing candidate keeps two fresh checks around the destructive cache deletion: one after the family relocation and before the root .cache is deleted, and one after the deletion and before the worktree removal; a candidate without a root .cache runs the non-mutating cleanliness probe and one final fresh check. In both flows no external Git process sits between the final fresh check and the removal. Kept reason codes distinguish issue state, lookup failure, legacy provenance, local probes and running sessions, and name the session-family, cache, path and registry failures a fresh re-check found. The read-only classification never moves a session; the preview then reads each candidate's fresh occupancy and the lineage the host reports, so it reports the same blockers and the same move eligibility the removal would meet, without moving, deleting, or writing anything. --dryrun (alias --dry-run) is the read-only preview and creates no store, schema, or operation lock. The removal stage is the only place that may move a session. There is no waiting period or age threshold. The local guards — never the main checkout, never a dirty or untracked directory, except the exact root `.cache` exception — are decided before any running-session query, so a main-checkout or dirty directory makes zero host or provider calls. The running-session answer comes from the one connected service; coverage is that service, not a whole-machine multi-service inventory. The default output follows the global --output auto selection: the compact human summary on an interactive terminal and the machine-readable aggregate envelope on a pipe or in CI, while --json or --format json still forces the envelope, --format text forces the summary, and a legacy selection that contradicts an explicit --output is rejected. The summary is one heading, one aggregate count line, one kept-reason counts line, at most five removal-failure examples with their path and reason plus the omitted count, a bounded absent-description line, a skipped-repository count when nonzero, and the measured elapsed time; its size never grows with the number of directories, because it lists no per-directory row. --verbose keeps that summary and adds one bounded row per actual removal, per removal failure, per eligible preview row (never labelled as removed), per absent directory, and per kept directory with its reason; it changes nothing in the machine document. The aggregate envelope is the complete-detail inspection channel. It also carries the measured timings of the pass: the wall clock, the pipeline stages, and the nested closure evaluations and host calls. The reclamation stage times the root .cache clearance or its cleanliness probe, the git worktree removal, the directory registry row removal and the absent registration reclamation, including the calls that fail. The two families overlap, because every nested call is measured inside the stage that made it, so the durations are observations and never an additive total; a closure evaluation counts evaluations, not network requests. The compact text keeps the elapsed line only; --verbose adds the bounded stage and operation breakdown, and the machine document keeps exactly one document either way. --remove is an accepted compatibility no-op and --no-sync is accepted but no longer runs a sync pass: prune never syncs. Branches and registry rows are never deleted by a dry run; a removed directory's registry row is deleted only after the tree is gone. Progress is a human affordance: on an interactive terminal one stderr line is redrawn in place through scanning, verification and deletion, showing completed repositories against the known repository total and cumulative completed directory counts for removal, absence, retention and failure, never a directory path or completion before an action returns, and it is cleared before the result. Orchestrator-only.";

/// Split the top-level `worktree` overview into header plus mutating and
/// read-only row groups so the shape is testable without capturing stdout.
fn worktree_help_parts(
    role: Option<Role>,
) -> (String, Vec<HelpRow<'static>>, Vec<HelpRow<'static>>) {
    let header = format!(
        "Worktree commands for {}:",
        role.map_or("all roles", Role::as_str)
    );
    let mutating: Vec<HelpRow<'static>> = vec![
        (
            "acquire",
            "Acquire or reuse a registered worktree directory",
            &["worktree", "acquire"],
        ),
        (
            "prune",
            "Prune registered worktrees and clean directories",
            &["worktree", "prune"],
        ),
    ];
    let readonly: Vec<HelpRow<'static>> = vec![
        (
            "status",
            "List registered directories associated with an issue",
            &["worktree", "status"],
        ),
        (
            "list",
            "List every registered directory for the resolved repo identity",
            &["worktree", "list"],
        ),
        (
            "probe",
            "Probe a checkout or registered directory read-only",
            &["worktree", "probe"],
        ),
    ];
    (header, mutating, readonly)
}

/// Top-level `worktree` help body rendered through the shared group helper.
pub(crate) fn worktree_help_text(role: Option<Role>) -> String {
    if role.is_some_and(|role| !is_read_role(role) && role != Role::Orchestrator) {
        return format!(
            "No worktree commands available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
    let (header, mutating, readonly) = worktree_help_parts(role);
    render_group_help(
        role,
        &header,
        &[
            (None, &mutating),
            (Some("Read-only (status/list/probe)"), &readonly),
        ],
        "Use 'phasegent --help worktree <command>' for options. acquire and list run their repository's sync pass first; --no-sync skips it.",
    )
}

pub(crate) fn print_worktree_help(role: Option<Role>) {
    if role.is_some_and(|role| !is_read_role(role) && role != Role::Orchestrator) {
        println!("{}", worktree_help_text(role));
        return;
    }
    let (header, mutating, readonly) = worktree_help_parts(role);
    print_group_help(
        role,
        &header,
        &[
            (None, &mutating),
            (Some("Read-only (status/list/probe)"), &readonly),
        ],
        "Use 'phasegent --help worktree <command>' for options. acquire and list run their repository's sync pass first; --no-sync skips it.",
    )
}

/// Per-subcommand `worktree` help body (no trailing newline).
pub(crate) fn worktree_command_help_text(role: Option<Role>, command: &str) -> String {
    match command {
        "acquire" => orchestrator_help(role, ACQUIRE_HELP),
        "status" => read_surface_help(role, STATUS_HELP),
        "list" => read_surface_help(role, LIST_HELP),
        "probe" => read_surface_help(role, PROBE_HELP),
        "prune" => orchestrator_help(role, PRUNE_HELP),
        _ => worktree_help_text(role),
    }
}

pub(crate) fn print_worktree_command_help(role: Option<Role>, command: &str) {
    println!("{}", worktree_command_help_text(role, command));
}

fn orchestrator_help(role: Option<Role>, text: &str) -> String {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        text.to_owned()
    } else {
        format!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        )
    }
}

fn read_surface_help(role: Option<Role>, text: &str) -> String {
    if role.is_none_or(is_read_role) {
        text.to_owned()
    } else {
        format!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        )
    }
}

fn is_read_role(role: Role) -> bool {
    matches!(role, Role::Orchestrator | Role::Executor | Role::Reviewer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_help_documents_registry_reuse_and_isolation() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "acquire");
        assert!(text.contains("PHASEGENT_SESSION_ID"), "got: {text}");
        assert!(
            text.contains("reuse-current-unless-the-target-path-is-occupied"),
            "got: {text}"
        );
        assert!(
            text.contains("confirmed closed historical task does not veto reuse"),
            "got: {text}"
        );
        assert!(
            text.contains("`--reuse` states the reuse preference explicitly")
                && text.contains("`--isolate` forces a fresh worktree"),
            "got: {text}"
        );
        assert!(
            text.contains("durable task bound to the issue in this repository")
                && text.contains("task ownership applies for every provider"),
            "got: {text}"
        );
        assert!(!text.contains("lease"), "got: {text}");
    }

    #[test]
    fn overview_is_tabular_and_lists_only_live_commands() {
        let text = worktree_help_text(Some(Role::Orchestrator));
        assert!(
            text.contains("Worktree commands for orchestrator:"),
            "got: {text}"
        );
        assert!(
            text.contains("Read-only (status/list/probe):"),
            "got: {text}"
        );
        for (name, desc) in [
            (
                "acquire",
                "Acquire or reuse a registered worktree directory",
            ),
            ("prune", "Prune registered worktrees and clean directories"),
            (
                "status",
                "List registered directories associated with an issue",
            ),
            (
                "list",
                "List every registered directory for the resolved repo identity",
            ),
            (
                "probe",
                "Probe a checkout or registered directory read-only",
            ),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "missing {name}; got: {text}"
            );
        }
        for removed in ["release", "heartbeat"] {
            assert!(
                !text.contains(&format!("  {removed:<14}")),
                "removed command {removed} must not appear; got: {text}"
            );
        }
    }

    #[test]
    fn prune_help_documents_registry_cleanup_without_lease_flags() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(text.contains("removal-by-default"), "got: {text}");
        assert!(text.contains("--dryrun"), "got: {text}");
        assert!(text.contains("--dry-run"), "got: {text}");
        assert!(text.contains("--remove"), "got: {text}");
        assert!(text.contains("--json"), "got: {text}");
        assert!(!text.contains("--release-stale"), "got: {text}");
        assert!(!text.contains("--stale-days"), "got: {text}");
        assert!(!text.contains("--reason"), "got: {text}");
        assert!(
            text.contains("confirmed closed through its own durable task"),
            "got: {text}"
        );
        assert!(
            text.contains("A confirmed closed historical task does not keep"),
            "got: {text}"
        );
        assert!(text.contains("still-open associated task"), "got: {text}");
        assert!(
            text.contains("read-only preview and creates no store"),
            "got: {text}"
        );
    }

    #[test]
    fn prune_help_documents_read_only_host_checks_and_local_first_prechecks() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("decided before any running-session query"),
            "got: {text}"
        );
        assert!(text.contains("zero host or provider calls"), "got: {text}");
        assert!(text.contains("never a directory path"), "got: {text}");
        assert!(
            text.contains("size never grows with the number of directories"),
            "got: {text}"
        );
        assert!(
            text.contains("not a whole-machine multi-service inventory"),
            "got: {text}"
        );
        assert!(
            text.contains("every present idle session the directory's registry associations name"),
            "got: {text}"
        );
    }

    #[test]
    fn status_and_probe_document_directory_json_and_read_only_semantics() {
        let status = worktree_command_help_text(Some(Role::Executor), "status");
        assert!(status.contains("directory_id"), "got: {status}");
        assert!(status.contains("legacy_issues"), "got: {status}");
        assert!(
            status.contains("never creates, migrates, or writes"),
            "got: {status}"
        );
        let probe = worktree_command_help_text(Some(Role::Executor), "probe");
        assert!(
            probe.contains("directory is the matching registry record"),
            "got: {probe}"
        );
        assert!(
            probe.contains("--path and --issue are mutually exclusive"),
            "got: {probe}"
        );
        assert!(probe.contains("never calls a provider"), "got: {probe}");
    }

    #[test]
    fn acquire_and_list_advertise_the_sync_pass_and_skip_switch() {
        for command in ["acquire", "list"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert!(text.contains("--no-sync"), "{command}: {text}");
            assert!(text.contains("issue sync"), "{command}: {text}");
        }
        let overview = worktree_help_text(Some(Role::Orchestrator));
        assert!(
            overview.contains("acquire and list run their repository's sync pass first"),
            "got: {overview}"
        );
        let prune = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            prune.contains("--no-sync") && !prune.contains("issue sync"),
            "got: {prune}"
        );
    }

    #[test]
    fn removed_commands_and_unknown_topics_fall_back_to_the_overview() {
        for command in ["release", "heartbeat", "release-stale", "fly"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert_eq!(text, worktree_help_text(Some(Role::Orchestrator)));
        }
    }

    #[test]
    fn read_role_is_denied_mutating_help() {
        let text = worktree_command_help_text(Some(Role::Executor), "prune");
        assert!(
            text.contains("No command available for executor"),
            "got: {text}"
        );
        let list = worktree_command_help_text(Some(Role::Reviewer), "list");
        assert!(list.contains("Usage: worktree list"), "got: {list}");
        let probe = worktree_command_help_text(Some(Role::Reviewer), "probe");
        assert!(probe.contains("Usage: worktree probe"), "got: {probe}");
    }

    #[test]
    fn overview_filters_by_role() {
        let read_only = worktree_help_text(Some(Role::Executor));
        assert!(
            read_only.contains("  status")
                && read_only.contains("  list")
                && read_only.contains("  probe"),
            "got: {read_only}"
        );
        for command in ["acquire", "prune"] {
            assert!(
                !read_only.contains(&format!("  {command:<14}")),
                "executor overview must hide {command}; got: {read_only}"
            );
        }
        let denied = worktree_help_text(Some(Role::Admin));
        assert!(
            denied.contains("No worktree commands available for admin"),
            "got: {denied}"
        );
    }
}

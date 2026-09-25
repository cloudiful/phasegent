//! Help text for the `worktree` command group (issue #239 Phase 2).
//!
//! The module mirrors the `timer` / `relation` help style:
//!
//! * The top-level `worktree` help lists each subcommand with its
//!   role gate spelled out so the operator can see at a glance which
//!   subcommands are orchestrator-only and which mirror the
//!   issue-status read surface (orchestrator, executor, reviewer).
//! * Per-subcommand help is plain prose, focused on the contract
//!   rather than the underlying `git` calls, and explicitly notes
//!   the safety guarantees (no branch deletion, no dirty removal,
//!   `.env` is never touched, `git worktree remove` is invoked
//!   without `--force`).
//! * The `acquire` / `list` / `prune` pages and the group footer also
//!   advertise the pre-subcommand `issue sync` pass and the
//!   `--no-sync` switch that skips it.
//!
//! The text builders are split from the `print_*` wrappers so the
//! advertised contract (session precedence, dry-run recovery, prune
//! boundaries) can be asserted directly in tests.

use super::common::{HelpRow, print_group_help, render_group_help};
use crate::policy::Role;

const ACQUIRE_HELP: &str = "Usage: worktree acquire --issue N [--session S] [--base REF] [--isolate] [--no-sync] [--format json]\n\nAcquire (or refresh) a per-(repo, issue, session) worktree lease and finish the local setup in one command. Idempotent: re-running with the same triple returns the same lease_id and updates the heartbeat (reason=\"idempotent\"). When the current checkout is clean and no other lease is active for the repo it is reused (reason=\"no_conflict\"); when it is dirty or any other active lease exists for the repo a fresh `phasegent/<issue>-<short6hex>` branch and a new worktree under ~/.cache/phasegent/worktrees/<fingerprint>/<slug> are created by default (reason=\"new_worktree\"), with the trigger explained by a stderr warning. That new default is the issue #436 behavior change: a dirty checkout or an existing lease no longer reuses the shared checkout, so a second session cannot collide with the `(repo, worktree_path)` lease index; `--isolate` remains accepted as the explicit opt-in for the same outcome. When the `git status` probe itself fails the dirty state is unknown: `--isolate` or the resolved `worktree-auto` switch creates a fresh worktree, otherwise the current checkout is reused, and both emit a stderr warning — an unknown status is never silently treated as clean, and this is the only case where the switches still change the outcome. On every successful acquire the issue is bound to the acquired checkout's branch and the managed commit hooks are installed when that checkout has a git origin, so one command leaves the checkout ready; both steps reuse the standard bind/hook helpers, never overwrite an existing binding to a different issue (the conflict is a warning naming `--replace`), and degrade to warnings that never fail the acquire. Returns compact JSON on stdout. --session resolves from the explicit flag, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback (legacy only warns on stderr); --base REF requests an explicit baseline: after the idempotent home-coming for the same (repo, issue, session), a fresh acquire creates the new worktree/branch from REF instead of HEAD and does not reuse the current checkout; the ref is validated read-only before anything is created, so a bad REF fails locally and leaves no half lease or worktree behind. --format is json (the only accepted value). Orchestrator-only. No branch, lease row, or dirty worktree is ever deleted, .env / secret material is never read or copied, and the lease table is created lazily through `CREATE TABLE IF NOT EXISTS` so pre-Phase-1 databases still open. Before its own work an orchestrator session runs a repository-scoped `issue sync` pass and forwards its warnings to stderr; --no-sync skips that pass.";

const RELEASE_HELP: &str = "Usage: worktree release --lease ID [--retain=true|false] [--force --reason TEXT]\n\nFlip an active lease to retained (default) or released. --retain defaults to true; the boolean accepts true|1|yes|on and false|0|no|off. The release is a no-op when the lease is already in the requested terminal state. The directory and the branch are never deleted by `release`; that is `prune`'s job. --force requires a non-empty --reason and persists it on the lease row (visible in status/list) so forced overrides stay attributable; --reason without --force is rejected. Lease rows are audit records and are never deleted — use force+reason instead of deleting rows. Orchestrator-only.";

const STATUS_HELP: &str = "Usage: worktree status --issue N\n\nList every active lease for the given issue id (read-only). Returns a JSON envelope with `{ \"issue\": N, \"leases\": [...] }`. The result is bounded by the storage layer (256 active rows max per query). Available to orchestrator, executor, and reviewer; tester is denied.";

const PROBE_HELP: &str = "Usage: worktree probe [--path PATH | --issue N [--session S]]\n\nRead-only diagnostic for one worktree. With --path PATH it probes that directory; with --issue N [--session S] it resolves the active lease for the current repository identity, the issue, and the optional session and probes the lease's worktree path; with no selector it probes the current checkout. --path and --issue are mutually exclusive, and --session requires --issue. Prints one bounded JSON document with resolved/path/exists/is_git_worktree/clean/branch/head/is_main_checkout/lease/errors: clean is true, false, or null (unknown), and a Git or filesystem failure is reported under errors instead of aborting, so a missing or non-Git path still returns a structured result. It never calls a provider, never writes or flips a lease, never syncs, and never deletes or repairs a directory, branch, or checkout. When --issue matches no active lease the result is the stable empty envelope (resolved=false, path=null) and no path is guessed. Available to orchestrator, executor, and reviewer; tester is denied.";

const LIST_HELP: &str = "Usage: worktree list [--repo PATH] [--no-sync]\n\nList every lease (active + terminal) for the resolved repo identity. --repo defaults to the current working directory; the value is canonicalised through `git rev-parse --git-common-dir` so the same physical repository yields the same identity from a main checkout, a linked worktree, or a subdirectory. Returns a JSON envelope with `{ \"repo_identity\": \"...\", \"leases\": [...] }`. Available to orchestrator, executor, and reviewer; tester is denied. An orchestrator session runs a repository-scoped `issue sync` pass before the listing and forwards its warnings to stderr; --no-sync skips that pass, and the listing envelope is unchanged either way.";

const PRUNE_HELP: &str = "Usage: worktree prune [--repo PATH] [--stale-days N] [--release-stale --reason TEXT] [--remove] [--no-sync]\n\nSingle pruning entry point (folds the former release-stale). With neither --release-stale nor --remove this is a read-only dry-run that reports stale active leases and prunable worktrees and changes nothing. --release-stale requires a non-empty --reason and flips exactly the active leases whose heartbeat is older than --stale-days (default 7) to `retained` in a single transaction, recording the reason; --reason without --release-stale is rejected. --remove deletes clean + expired + retained worktrees. Supplying both runs the recovery first and then the removal. A worktree is a removal candidate only when all three conditions hold: status is `retained`; heartbeat is older than stale-days days; and `git status --porcelain` reports an empty output. Dirty worktrees are never deleted (prunable=false, skipped_dirty); active and released leases are skipped; recent retained leases are skipped. `git worktree remove` is the only git command invoked, and it is never passed `--force`; branches are never deleted (no `git branch -D`). --repo defaults to the current working directory and is canonicalised through `git rev-parse --git-common-dir`. Returns a JSON envelope that records lease and directory actions separately. Orchestrator-only. Before its own work it runs a repository-scoped `issue sync` pass and forwards its warnings to stderr; --no-sync skips that pass.";

const HEARTBEAT_HELP: &str = "Usage: worktree heartbeat --lease ID [--session SESSION]\n\nRefresh heartbeat_at on an active lease so a long-running session is not mistaken for stale. The update only matches when the lease is still `active` AND its stored session equals the caller's resolved session (--session, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback with a stderr warning). A foreign session, a terminal lease, or an unknown lease id returns a structured `state` conflict and leaves the row untouched; the conditional update also guarantees a heartbeat and a concurrent stale recovery cannot both win. Returns a JSON envelope with lease_id/issue/session/status/heartbeat_at. Never deletes a worktree or branch. Orchestrator-only.";

/// Split the top-level `worktree` overview into header plus mutating and
/// read-only row groups so the shape is testable without capturing stdout.
/// Every row carries its registry path, so the overview and the parser share
/// the same role gate (the execution layer also checks `Role::Orchestrator`
/// directly as defense in depth).
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
            "Acquire or reuse a per-(repo, issue, session) lease",
            &["worktree", "acquire"],
        ),
        (
            "release",
            "Flip an active lease to retained or released",
            &["worktree", "release"],
        ),
        (
            "heartbeat",
            "Refresh an active lease heartbeat",
            &["worktree", "heartbeat"],
        ),
        (
            "prune",
            "Prune stale leases and clean worktrees",
            &["worktree", "prune"],
        ),
    ];
    let readonly: Vec<HelpRow<'static>> = vec![
        (
            "status",
            "List active leases for an issue",
            &["worktree", "status"],
        ),
        (
            "list",
            "List every lease for the resolved repo identity",
            &["worktree", "list"],
        ),
        (
            "probe",
            "Probe a checkout or lease path read-only",
            &["worktree", "probe"],
        ),
    ];
    (header, mutating, readonly)
}

/// Top-level `worktree` help body rendered through the shared group helper.
/// Contract prose (#436 isolation default, #239 Phase 2, session precedence,
/// worktree-auto) lives on the per-subcommand detail pages and stays out of
/// this one-line-per-command overview.
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
        "Use 'phasegent --help worktree <command>' for options. acquire, list, and prune run their repository's sync pass first; --no-sync skips it.",
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
        "Use 'phasegent --help worktree <command>' for options. acquire, list, and prune run their repository's sync pass first; --no-sync skips it.",
    )
}

/// Per-subcommand `worktree` help body (no trailing newline).
pub(crate) fn worktree_command_help_text(role: Option<Role>, command: &str) -> String {
    match command {
        "acquire" => orchestrator_help(role, ACQUIRE_HELP),
        "release" => orchestrator_help(role, RELEASE_HELP),
        "status" => read_surface_help(role, STATUS_HELP),
        "list" => read_surface_help(role, LIST_HELP),
        "probe" => read_surface_help(role, PROBE_HELP),
        "prune" => orchestrator_help(role, PRUNE_HELP),
        "heartbeat" => orchestrator_help(role, HEARTBEAT_HELP),
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
    fn orchestrator_help_advertises_session_precedence_and_unknown_status() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "acquire");
        assert!(text.contains("PHASEGENT_SESSION_ID"), "got: {text}");
        assert!(
            text.contains("unknown status is never silently treated as clean"),
            "got: {text}"
        );
        assert!(
            text.contains("new default is the issue #436 behavior change"),
            "got: {text}"
        );
        assert!(
            text.contains("--isolate` remains accepted as the explicit opt-in"),
            "got: {text}"
        );
        assert!(
            text.contains("bound to the acquired checkout's branch")
                && text.contains("managed commit hooks are installed"),
            "acquire help must advertise the one-command bind + hooks closure: {text}"
        );
    }

    #[test]
    fn overview_is_tabular_with_read_only_section() {
        let text = worktree_help_text(Some(Role::Orchestrator));
        assert!(
            text.contains("Worktree commands for orchestrator:"),
            "header must use the grouped shape; got: {text}"
        );
        assert!(
            text.contains("Read-only (status/list/probe):"),
            "status/list/probe need their own section; got: {text}"
        );
        for (name, desc) in [
            (
                "acquire",
                "Acquire or reuse a per-(repo, issue, session) lease",
            ),
            ("release", "Flip an active lease to retained or released"),
            ("heartbeat", "Refresh an active lease heartbeat"),
            ("prune", "Prune stale leases and clean worktrees"),
            ("status", "List active leases for an issue"),
            ("list", "List every lease for the resolved repo identity"),
            ("probe", "Probe a checkout or lease path read-only"),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "rows stay one-command-per-line; missing {name}; got: {text}"
            );
        }
        assert!(
            text.contains("Use 'phasegent --help worktree <command>' for options."),
            "footer must point to detail pages; got: {text}"
        );
    }

    #[test]
    fn overview_sinks_contract_prose_to_detail_pages_without_loss() {
        let overview = worktree_help_text(Some(Role::Orchestrator));
        for sunk in [
            "issue #436",
            "issue #239",
            "PHASEGENT_SESSION_ID",
            "worktree-auto",
            "phasegent/<issue>-",
        ] {
            assert!(
                !overview.contains(sunk),
                "overview must not repeat contract prose ({sunk}); got: {overview}"
            );
        }
        let acquire = worktree_command_help_text(Some(Role::Orchestrator), "acquire");
        assert!(
            acquire.contains("issue #436") && acquire.contains("PHASEGENT_SESSION_ID"),
            "acquire detail keeps isolation + session prose; got: {acquire}"
        );
        assert!(
            acquire.contains("worktree-auto"),
            "acquire detail keeps the unknown-probe switch; got: {acquire}"
        );
        let heartbeat = worktree_command_help_text(Some(Role::Orchestrator), "heartbeat");
        assert!(
            heartbeat.contains("PHASEGENT_SESSION_ID"),
            "heartbeat detail keeps session prose; got: {heartbeat}"
        );
        let status = worktree_command_help_text(Some(Role::Orchestrator), "status");
        assert!(
            status.contains("orchestrator, executor, and reviewer"),
            "status detail keeps the read-surface prose; got: {status}"
        );
    }

    #[test]
    fn prune_help_documents_default_dry_run_and_action_flags() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(text.contains("read-only dry-run"), "got: {text}");
        assert!(text.contains("--release-stale"), "got: {text}");
        assert!(text.contains("--remove"), "got: {text}");
        assert!(text.contains("--reason"), "got: {text}");
        assert!(
            text.contains("records lease and directory actions separately"),
            "got: {text}"
        );
    }

    #[test]
    fn removed_release_stale_help_topic_falls_back_to_top_level() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "release-stale");
        assert_eq!(text, worktree_help_text(Some(Role::Orchestrator)));
    }

    #[test]
    fn heartbeat_help_documents_owner_match() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "heartbeat");
        assert!(text.contains("stored session equals"), "got: {text}");
        assert!(text.contains("PHASEGENT_SESSION_ID"), "got: {text}");
        assert!(
            text.contains("Never deletes a worktree or branch"),
            "got: {text}"
        );
    }

    #[test]
    fn prune_help_documents_dirty_boundary() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("Dirty worktrees are never deleted"),
            "got: {text}"
        );
        assert!(text.contains("never passed `--force`"), "got: {text}");
        assert!(text.contains("branches are never deleted"), "got: {text}");
    }

    /// Issue 552 Phase 2 added the repository-scoped reconciliation pass
    /// that `acquire` / `list` / `prune` run before their own work; every
    /// page it applies to must advertise the pass and its `--no-sync`
    /// switch, and the pages it does not touch must stay silent about it.
    #[test]
    fn acquire_list_and_prune_advertise_the_sync_pass_and_its_skip_switch() {
        for command in ["acquire", "list", "prune"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert!(
                text.contains("--no-sync"),
                "{command} help must advertise --no-sync; got: {text}"
            );
            assert!(
                text.contains("issue sync"),
                "{command} help must name the reconciliation pass; got: {text}"
            );
        }
        let overview = worktree_help_text(Some(Role::Orchestrator));
        assert!(
            overview.contains("--no-sync"),
            "the overview footer must advertise the switch; got: {overview}"
        );
        for command in ["release", "heartbeat", "status", "probe"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert!(
                !text.contains("--no-sync"),
                "{command} has no reconciliation pass; got: {text}"
            );
        }
    }

    #[test]
    fn overview_lists_every_command_and_filters_by_role() {
        let full = worktree_help_text(Some(Role::Orchestrator));
        for command in [
            "acquire",
            "release",
            "heartbeat",
            "prune",
            "status",
            "list",
            "probe",
        ] {
            assert!(
                full.contains(command),
                "orchestrator overview missing {command}; got: {full}"
            );
        }
        let all_roles = worktree_help_text(None);
        for command in [
            "acquire",
            "release",
            "heartbeat",
            "prune",
            "status",
            "list",
            "probe",
        ] {
            assert!(
                all_roles.contains(command),
                "all-roles overview missing {command}; got: {all_roles}"
            );
        }
        let read_only = worktree_help_text(Some(Role::Executor));
        assert!(
            read_only.contains("  status")
                && read_only.contains("  list")
                && read_only.contains("  probe"),
            "executor keeps the read-only section; got: {read_only}"
        );
        for command in ["acquire", "release", "heartbeat", "prune"] {
            assert!(
                !read_only.contains(&format!("  {command:<14}")),
                "executor overview must hide mutating row {command}; got: {read_only}"
            );
        }
        let reviewer = worktree_help_text(Some(Role::Reviewer));
        assert!(
            reviewer.contains("  status") && !reviewer.contains("  acquire"),
            "reviewer keeps status and hides acquire; got: {reviewer}"
        );
        for role in [Role::Tester, Role::Admin] {
            let denied = worktree_help_text(Some(role));
            assert!(
                denied.contains(&format!(
                    "No worktree commands available for {}",
                    role.as_str()
                )),
                "{role:?} must stay denied without a table; got: {denied}"
            );
        }
    }

    #[test]
    fn read_role_is_denied_mutating_and_unknown_help() {
        let text = worktree_command_help_text(Some(Role::Executor), "heartbeat");
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
    fn unknown_command_falls_back_to_top_level_help() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "fly");
        assert_eq!(text, worktree_help_text(Some(Role::Orchestrator)));
    }

    #[test]
    fn heartbeat_and_prune_are_documented_not_fallbacks() {
        for command in ["heartbeat", "prune", "probe"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert!(
                text.contains(&format!("Usage: worktree {command}")),
                "missing command help for {command}; got: {text}"
            );
        }
    }

    #[test]
    fn probe_help_documents_the_read_only_contract() {
        let text = worktree_command_help_text(Some(Role::Executor), "probe");
        assert!(text.contains("Usage: worktree probe"), "got: {text}");
        assert!(text.contains("--path PATH"), "got: {text}");
        assert!(text.contains("--issue N"), "got: {text}");
        assert!(text.contains("mutually exclusive"), "got: {text}");
        assert!(text.contains("never calls a provider"), "got: {text}");
        assert!(text.contains("resolved=false"), "got: {text}");
        assert!(text.contains("tester is denied"), "got: {text}");
        let denied = worktree_command_help_text(Some(Role::Tester), "probe");
        assert!(
            denied.contains("No command available for tester"),
            "tester must be denied the probe page; got: {denied}"
        );
    }
}

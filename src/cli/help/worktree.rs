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

const ACQUIRE_HELP: &str = "Usage: worktree acquire --issue N [--session S] [--base REF] [--reuse] [--isolate] [--no-sync] [--format json]\n\nAcquire (or refresh) a per-(repo, issue, session) worktree lease and finish the local setup in one command. Idempotent: re-running with the same triple returns the same lease_id and updates the heartbeat (reason=\"idempotent\"). The default is reuse-current-unless-the-target-path-is-actively-occupied (issue 651): the current checkout is reused (reason=\"no_conflict\") whether it is clean, dirty, unbound, or linked to a historical issue, and only an active lease on the checkout path itself allocates a dedicated worktree — a fresh `phasegent/<issue>-<short6hex>` branch and a new worktree under ~/.cache/phasegent/worktrees/<fingerprint>/<slug> (reason=\"new_worktree\"), with the occupying session, issue, and lease named in a stderr warning. An active lease on any other path never blocks a free primary checkout, and terminal lease history never conflicts. Dirty state is advisory only: it warns on stderr but never forces isolation, never stashes, and never mutates branches. `--reuse` states the reuse preference explicitly and `--isolate` forces a fresh worktree; the two are mutually exclusive, and `--reuse` never bypasses an occupied checkout. When the `git status` probe itself fails the dirty state is unknown: `--isolate` creates a fresh worktree, otherwise the current checkout is reused, and both emit a stderr warning — an unknown status is never silently treated as clean. On every successful acquire the managed commit hooks are installed when the acquired checkout has a git origin, so one command leaves the checkout ready; the install reuses the standard hook helper and degrades to a warning that never fails the acquire. Acquire never writes a branch/issue association: the durable link store has no provider/project scope here, and the lease identity plus the generated branch name are the operation's supported signals. Returns compact JSON on stdout. --session resolves from the explicit flag, else PHASEGENT_SESSION_ID; one of the two is required, and a missing identity fails before any lease work. --base REF requests an explicit baseline: after the idempotent home-coming for the same (repo, issue, session), a fresh acquire creates the new worktree/branch from REF instead of HEAD and does not reuse the current checkout; the ref is validated read-only before anything is created, so a bad REF fails locally and leaves no half lease or worktree behind. --format is json (the only accepted value). Orchestrator-only. No branch, lease row, or dirty worktree is ever deleted, .env / secret material is never read or copied, and the lease table is created lazily through `CREATE TABLE IF NOT EXISTS` so pre-Phase-1 databases still open. Before its own work an orchestrator session runs a repository-scoped `issue sync` pass and forwards its warnings to stderr; --no-sync skips that pass.";

const RELEASE_HELP: &str = "Usage: worktree release --lease ID [--retain=true|false] [--force --reason TEXT]\n\nFlip an active lease to retained (default) or released. --retain defaults to true; the boolean accepts true|1|yes|on and false|0|no|off. The release is a no-op when the lease is already in the requested terminal state. The directory and the branch are never deleted by `release`; that is `prune`'s job. --force requires a non-empty --reason and persists it on the lease row (visible in status/list) so forced overrides stay attributable; --reason without --force is rejected. Lease rows are audit records and are never deleted — use force+reason instead of deleting rows. Orchestrator-only for agents; ordinary human Operator shares this authority, managed sessions without a role fail closed. See docs/cli-caller-context.md.";

const STATUS_HELP: &str = "Usage: worktree status --issue N [--page N] [--limit N]\n\nRead active leases for the issue. Returns `{items,page,limit,total_count,has_more,issue}` with page 1 and limit 50 by default. --page is positive and 1-based; --limit is 1..100. An out-of-range page is empty and retains the exact total. Available to orchestrator, executor, and reviewer. Ordinary human Operator shares this read surface; managed sessions without a role fail closed. See docs/cli-pagination.md and docs/cli-caller-context.md.";

const PROBE_HELP: &str = "Usage: worktree probe [--path PATH | --issue N [--session S]]\n\nRead-only diagnostic for one worktree. With --path PATH it probes that directory; with --issue N [--session S] it resolves the active lease for the current repository identity, the issue, and the optional session and probes the lease's worktree path; with no selector it probes the current checkout. --path and --issue are mutually exclusive, and --session requires --issue. Prints one bounded JSON document with resolved/path/exists/is_git_worktree/clean/branch/head/is_main_checkout/lease/errors: clean is true, false, or null (unknown), and a Git or filesystem failure is reported under errors instead of aborting, so a missing or non-Git path still returns a structured result. It never calls a provider, never writes or flips a lease, never syncs, and never deletes or repairs a directory, branch, or checkout. When --issue matches no active lease the result is the stable empty envelope (resolved=false, path=null) and no path is guessed. Available to orchestrator, executor, and reviewer.";

const LIST_HELP: &str = "Usage: worktree list [--repo PATH] [--no-sync] [--page N] [--limit N]\n\nList active and terminal leases for the resolved repo identity. --repo defaults to the current working directory; the value is canonicalised through `git rev-parse --git-common-dir` so the same physical repository yields the same identity from a main checkout, a linked worktree, or a subdirectory. Returns `{items,page,limit,total_count,has_more,repo_identity}` with page 1 and limit 50 by default. --page is positive and 1-based; --limit is 1..100. An out-of-range page is empty and retains the exact total. Available to orchestrator, executor, and reviewer. Authorized Operator/orchestrator callers run a complete repository-scoped `issue sync` before listing and forward its warnings to stderr; --no-sync skips it. Pagination does not limit that sync pass. See docs/cli-pagination.md and docs/worktree-maintenance.md.";

const PRUNE_HELP: &str = "Usage: worktree prune [--repo PATH] [--stale-days N] [--release-stale --reason TEXT] [--dryrun] [--json|--format json] [--remove] [--no-sync]\n\nGlobal, removal-by-default cleanup of managed worktrees, grouped by canonical directory. Without --repo it enumerates every repository identity the local lease store records (even outside Git) and resolves each through its main checkout, skipping an identity it cannot verify with a bounded reason instead of guessing; --repo PATH restricts the pass to one verified checkout. A directory is removed as soon as every issue associated with it is confirmed closed on the configured provider, no active lease references the canonical directory, and prune's running-session guard clears it: only a session the connected shared foreground-drain service reports as *currently running* in the directory blocks removal, checked read-only, so an inactive historical session record that still points at the directory never keeps it. Prune never enumerates historical sessions and never moves a session during the read-only scan; coverage is that one connected service, not a whole-machine multi-service inventory. The removal stage is the only place that may move a session: before it deletes a directory it reads that directory's fresh occupancy and returns every associated idle `ses_` session to the repository's verified main checkout, confirming each move with a fresh authoritative read. Those checks describe the one connected service at that moment, not a guarantee about session changes after them. A session no lease of this repository associates keeps the directory and is never moved; so do an unavailable or incomplete occupancy listing, an unresolvable associated session, an answer that names another session or reports an unusable location, a rejected, queued, or unconfirmed move, an unverifiable main checkout, and a session that started running in the meantime. An unavailable, malformed, mismatched, or non-absolute running-session answer, or any running session (root or child) at the directory, keeps it; removal then uses `git worktree remove` without `--force` after a fresh re-check of the running-session answer and fresh lease rows. There is no waiting period: a freshly retained closed clean directory is removed immediately, so --stale-days never gates directory deletion and exists only for the explicit --release-stale recovery below. Closure is read from the configured provider through the same current-configured-provider resolution the rest of the CLI uses; a missing, unavailable, ambiguous, or mismatched issue is never treated as closed, and a preview may read closure but never mutates. The local guards — never the main checkout, never a path an `active` lease points at, never dirty or untracked — are decided before any running-session query, so a main-checkout or dirty directory makes zero host or provider calls. Because the runtime-host dependency is explicit, an unreadable service keeps even a pure-CLI candidate; the human failure count scopes to failed `git worktree remove` calls rather than to kept directories, which can include an unproven running-session answer. --dryrun (alias --dry-run) is the read-only preview: it performs the same read-only running-session discovery to report eligibility, but never mutates any directory, lease row, or session and never moves a session. --release-stale requires a non-empty --reason unless it is a preview and flips exactly the active leases whose heartbeat is older than --stale-days to `retained` in a single transaction; a directory that held an active lease at invocation start is never removed by the same call, and --reason without --release-stale is rejected. The default output follows the global --output auto selection: the compact human summary on an interactive terminal and the machine-readable aggregate envelope on a pipe or in CI, while --json or --format json still forces the envelope, --format text forces the summary, and a legacy selection that contradicts an explicit --output is rejected. The summary is one heading, one line per actual removal or removal failure (and one per eligible row in a preview, never labelled as removed), one kept line with its reason per kept directory, one aggregate count line, one kept-reason counts line, a skipped-repository count when nonzero, and the measured elapsed time; the aggregate envelope (per repository plus totals) is the complete-detail inspection channel, since the summary never lists every absent or no-op path. --remove is an accepted compatibility no-op and --no-sync is accepted but no longer runs a sync pass: prune never syncs. Branches and lease rows are never deleted. Progress is a human affordance: on an interactive terminal the repository scan and at most one `verifying … before removal` step per attempted deletion update the stderr spinner, which is cleared before the result, while a machine run (JSON, a pipe, CI, a dumb terminal, an agent role, or --no-progress) never sees a progress line; actual warnings and errors keep their stderr channel, and stdout stays reserved for the final summary or the single JSON document; within one invocation the read-only scan shares a repeated running-session answer and resolves each repository's identity once, but every removal re-checks the running-session answer, closure, and lease rows live and reads the fresh occupancy of the directory it is about to remove. Orchestrator-only.";

const HEARTBEAT_HELP: &str = "Usage: worktree heartbeat --lease ID [--session SESSION]\n\nRefresh heartbeat_at on an active lease so a long-running session is not mistaken for stale. The update only matches when the lease is still `active` AND its stored session equals the caller's resolved session (--session, else PHASEGENT_SESSION_ID; one of the two is required). A foreign session, a terminal lease, or an unknown lease id returns a structured `state` conflict and leaves the row untouched; the conditional update also guarantees a heartbeat and a concurrent stale recovery cannot both win. Returns a JSON envelope with lease_id/issue/session/status/heartbeat_at. Never deletes a worktree or branch. Orchestrator-only for agents; ordinary human Operator shares this authority, managed sessions without a role fail closed. See docs/cli-caller-context.md.";

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
/// Contract prose (lease-first isolation, #239 Phase 2, session precedence)
/// lives on the per-subcommand detail pages and stays out of
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
            text.contains("reuse-current-unless-the-target-path-is-actively-occupied"),
            "got: {text}"
        );
        assert!(
            text.contains("`--reuse` states the reuse preference explicitly")
                && text.contains("`--isolate` forces a fresh worktree"),
            "got: {text}"
        );
        assert!(
            !text.contains("worktree-auto"),
            "the removed switch must not appear in help; got: {text}"
        );
        assert!(
            text.contains("managed commit hooks are installed")
                && text.contains("never writes a branch/issue association"),
            "acquire help must advertise the one-command hooks closure: {text}"
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
            "phasegent/<issue>-",
        ] {
            assert!(
                !overview.contains(sunk),
                "overview must not repeat contract prose ({sunk}); got: {overview}"
            );
        }
        let acquire = worktree_command_help_text(Some(Role::Orchestrator), "acquire");
        assert!(
            acquire.contains("issue 651") && acquire.contains("PHASEGENT_SESSION_ID"),
            "acquire detail keeps isolation + session prose; got: {acquire}"
        );
        assert!(
            acquire.contains("--reuse") && !acquire.contains("worktree-auto"),
            "acquire detail advertises --reuse instead of the removed switch; got: {acquire}"
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
    fn prune_help_documents_default_removal_preview_and_action_flags() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(text.contains("removal-by-default"), "got: {text}");
        assert!(text.contains("--dryrun"), "got: {text}");
        assert!(text.contains("--dry-run"), "got: {text}");
        assert!(text.contains("--release-stale"), "got: {text}");
        assert!(text.contains("--remove"), "got: {text}");
        assert!(text.contains("--reason"), "got: {text}");
        assert!(text.contains("--json"), "got: {text}");
        assert!(
            text.contains("machine-readable aggregate envelope"),
            "got: {text}"
        );
        assert!(
            text.contains("complete-detail inspection channel"),
            "help must point at --json for full detail; got: {text}"
        );
    }

    /// Issue 916: the page must distinguish the immediate closed-issue
    /// deletion rule from the age-based explicit recovery, and it must not
    /// imply a waiting period.
    #[test]
    fn prune_help_separates_immediate_removal_from_stale_recovery() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("no waiting period"),
            "help must state there is no waiting period; got: {text}"
        );
        assert!(
            text.contains("--stale-days never gates directory deletion"),
            "help must separate stale-days from deletion; got: {text}"
        );
        assert!(
            text.contains("confirmed closed"),
            "help must name the closed-issue gate; got: {text}"
        );
        assert!(
            text.contains("configured provider"),
            "help must name the provider read; got: {text}"
        );
        assert!(
            text.contains("active lease at invocation start"),
            "help must state the invocation-start active rule; got: {text}"
        );
        assert!(
            text.contains("compact human summary"),
            "help must state the human output is the compact summary; got: {text}"
        );
        assert!(
            text.contains("--output auto selection"),
            "help must state that the default follows the auto selection; got: {text}"
        );
        assert!(
            text.contains("skipped-repository count"),
            "help must state the skipped-repository count; got: {text}"
        );
        assert!(
            text.contains("measured elapsed time"),
            "help must state the elapsed time; got: {text}"
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
        assert!(text.contains("never dirty or untracked"), "got: {text}");
        assert!(text.contains("without `--force`"), "got: {text}");
        assert!(
            text.contains("Branches and lease rows are never deleted"),
            "got: {text}"
        );
    }

    /// Issue 928 P1: the page must describe the current-service running-session
    /// criterion, the single-service scope, and that the read-only scan never
    /// enumerates history or moves a session.
    #[test]
    fn prune_help_documents_running_session_occupancy() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("currently running"),
            "help must name the running-session criterion; got: {text}"
        );
        assert!(
            text.contains("never enumerates historical sessions"),
            "help must state the scan never enumerates history; got: {text}"
        );
        assert!(
            text.contains("not a whole-machine multi-service inventory"),
            "help must scope coverage to one service; got: {text}"
        );
    }

    /// Issue 1061 P1: the page must describe the removal stage's session
    /// return — an associated idle session goes to the verified main checkout,
    /// and every failure keeps the directory.
    #[test]
    fn prune_help_documents_the_session_return_before_removal() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("associated idle"),
            "help must name the sessions that may be returned; got: {text}"
        );
        assert!(
            text.contains("verified main checkout"),
            "help must name the move target; got: {text}"
        );
        assert!(
            text.contains("confirming each move with a fresh authoritative read"),
            "help must state the move is confirmed; got: {text}"
        );
        assert!(
            text.contains("keeps the directory and is never moved"),
            "help must state that a foreign session is never moved; got: {text}"
        );
        assert!(
            text.contains("an unavailable or incomplete occupancy listing"),
            "help must state the failure modes that keep a directory; got: {text}"
        );
    }

    /// Issues 917 P1, 929 P1, and 1054 P3: the page must announce the gated
    /// progress surface, and that the scan memoizes as it resolves each
    /// repository identity once while every removal re-checks live.
    #[test]
    fn prune_help_documents_progress_and_live_removal_check() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("update the stderr spinner"),
            "help must announce the progress surface; got: {text}"
        );
        assert!(
            text.contains("never sees a progress line"),
            "help must state that a machine run never animates; got: {text}"
        );
        assert!(
            text.contains("actual warnings and errors keep their stderr channel"),
            "help must keep the diagnostic channel; got: {text}"
        );
        assert!(
            text.contains("shares a repeated running-session answer"),
            "help must describe the scan memo; got: {text}"
        );
        assert!(
            text.contains("every removal re-checks the running-session answer"),
            "help must state removals re-check live; got: {text}"
        );
        assert!(
            text.contains("resolves each repository's identity once"),
            "help must describe the per-repository identity reuse; got: {text}"
        );
    }

    /// Issue 928 P1: the preview reads the running-session answer read-only and
    /// never enumerates history.
    #[test]
    fn prune_help_preview_documents_read_only_host_discovery() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("read-only running-session discovery"),
            "help must state the preview discovers the running sessions read-only; got: {text}"
        );
        assert!(
            !text.contains("never probes the host"),
            "help must not claim the preview avoids the host; got: {text}"
        );
    }

    /// Issue 928 P1: the page must state that local guards precede the
    /// running-session query.
    #[test]
    fn prune_help_documents_local_first_prechecks() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            text.contains("decided before any running-session query"),
            "help must state local guards precede the running-session query; got: {text}"
        );
        assert!(
            text.contains("zero host or provider calls"),
            "help must state a locally-excluded directory makes no host call; got: {text}"
        );
        assert!(
            text.contains("runtime-host dependency is explicit"),
            "help must state the runtime-host dependency is explicit; got: {text}"
        );
        assert!(
            text.contains("scopes to failed `git worktree remove` calls"),
            "help must scope the failure count; got: {text}"
        );
    }

    /// Issue 552 Phase 2 added the repository-scoped reconciliation pass
    /// that `acquire` / `list` run before their own work; every page it
    /// applies to must advertise the pass and its `--no-sync` switch, and
    /// the pages it does not touch must stay silent about it. Issue 913
    /// removed `prune` from the set: its `--no-sync` is still accepted but
    /// it never runs a sync pass.
    #[test]
    fn acquire_and_list_advertise_the_sync_pass_and_its_skip_switch() {
        for command in ["acquire", "list"] {
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
        assert!(
            overview.contains("acquire and list run their repository's sync pass first"),
            "the footer must name only acquire and list; got: {overview}"
        );
        let prune = worktree_command_help_text(Some(Role::Orchestrator), "prune");
        assert!(
            prune.contains("--no-sync") && !prune.contains("issue sync"),
            "prune advertises --no-sync but no sync pass; got: {prune}"
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
        let denied = worktree_help_text(Some(Role::Admin));
        assert!(
            denied.contains("No worktree commands available for admin"),
            "admin must stay denied without a table; got: {denied}"
        );
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
        assert!(
            text.contains("Available to orchestrator, executor, and reviewer"),
            "got: {text}"
        );
        let denied = worktree_command_help_text(Some(Role::Admin), "probe");
        assert!(
            denied.contains("No command available for admin"),
            "admin must be denied the probe page; got: {denied}"
        );
    }
}

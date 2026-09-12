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
//!
//! The text builders are split from the `print_*` wrappers so the
//! advertised contract (session precedence, dry-run recovery, prune
//! boundaries) can be asserted directly in tests.

use crate::policy::Role;

const ACQUIRE_HELP: &str = "Usage: worktree acquire --issue N [--session S] [--base REF] [--isolate] [--format json]\n\nAcquire (or refresh) a per-(repo, issue, session) worktree lease. Idempotent: re-running with the same triple returns the same lease_id and updates the heartbeat (reason=\"idempotent\"). When no other lease is active for the repo, the current checkout is reused (reason=\"no_conflict\"); otherwise a fresh `phasegent/<issue>-<short6hex>` branch and a new worktree under ~/.cache/phasegent/worktrees/<fingerprint>/<slug> are created (reason=\"new_worktree\"). A checkout that is dirty and bound to a different issue also forces a fresh worktree (reason=\"new_worktree\") so an incoming task never lands in another task's dirty tree, with the trigger explained by warnings on stderr. When the `git status` probe itself fails, the dirty state is unknown: auto-isolation on creates a fresh worktree, auto-isolation off reuses the current checkout, and both emit a stderr warning — an unknown status is never silently treated as clean. Auto-isolation defaults off (issue #247): with neither `--isolate` nor `PHASEGENT_WORKTREE_AUTO`/`worktree-auto` true, conflict triggers reuse the current checkout and warn instead of creating a branch or directory; pass `--isolate` (one-shot) or set the global boolean on (env over SQLite, default false) to restore the creating path. Returns compact JSON on stdout. --session resolves from the explicit flag, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback (legacy only warns on stderr); --base is accepted for forward compatibility and the implementation always bases on HEAD. --format is json (the only accepted value). Orchestrator-only. The branch is never deleted, .env / secret material is never read or copied, and the lease table is created lazily through `CREATE TABLE IF NOT EXISTS` so pre-Phase-1 databases still open.";

const RELEASE_HELP: &str = "Usage: worktree release --lease ID [--retain=true|false] [--force --reason TEXT]\n\nFlip an active lease to retained (default) or released. --retain defaults to true; the boolean accepts true|1|yes|on and false|0|no|off. The release is a no-op when the lease is already in the requested terminal state. The directory and the branch are never deleted by `release`; that is `prune`'s job. --force requires a non-empty --reason and persists it on the lease row (visible in status/list) so forced overrides stay attributable; --reason without --force is rejected. Lease rows are audit records and are never deleted — use force+reason instead of deleting rows. Orchestrator-only.";

const STATUS_HELP: &str = "Usage: worktree status --issue N\n\nList every active lease for the given issue id (read-only). Returns a JSON envelope with `{ \"issue\": N, \"leases\": [...] }`. The result is bounded by the storage layer (256 active rows max per query). Available to orchestrator, executor, and reviewer; tester is denied.";

const LIST_HELP: &str = "Usage: worktree list [--repo PATH]\n\nList every lease (active + terminal) for the resolved repo identity. --repo defaults to the current working directory; the value is canonicalised through `git rev-parse --git-common-dir` so the same physical repository yields the same identity from a main checkout, a linked worktree, or a subdirectory. Returns a JSON envelope with `{ \"repo_identity\": \"...\", \"leases\": [...] }`. Available to orchestrator, executor, and reviewer; tester is denied.";

const PRUNE_HELP: &str = "Usage: worktree prune [--repo PATH] [--stale-days N] [--release-stale --reason TEXT] [--remove]\n\nSingle pruning entry point (folds the former release-stale). With neither --release-stale nor --remove this is a read-only dry-run that reports stale active leases and prunable worktrees and changes nothing. --release-stale requires a non-empty --reason and flips exactly the active leases whose heartbeat is older than --stale-days (default 14) to `retained` in a single transaction, recording the reason; --reason without --release-stale is rejected. --remove deletes clean + expired + retained worktrees. Supplying both runs the recovery first and then the removal. A worktree is a removal candidate only when all three conditions hold: status is `retained`; heartbeat is older than stale-days days; and `git status --porcelain` reports an empty output. Dirty worktrees are never deleted (prunable=false, skipped_dirty); active and released leases are skipped; recent retained leases are skipped. `git worktree remove` is the only git command invoked, and it is never passed `--force`; branches are never deleted (no `git branch -D`). --repo defaults to the current working directory and is canonicalised through `git rev-parse --git-common-dir`. Returns a JSON envelope that records lease and directory actions separately. Orchestrator-only.";

const HEARTBEAT_HELP: &str = "Usage: worktree heartbeat --lease ID [--session SESSION]\n\nRefresh heartbeat_at on an active lease so a long-running session is not mistaken for stale. The update only matches when the lease is still `active` AND its stored session equals the caller's resolved session (--session, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback with a stderr warning). A foreign session, a terminal lease, or an unknown lease id returns a structured `state` conflict and leaves the row untouched; the conditional update also guarantees a heartbeat and a concurrent stale recovery cannot both win. Returns a JSON envelope with lease_id/issue/session/status/heartbeat_at. Never deletes a worktree or branch. Orchestrator-only.";

/// Top-level `worktree` help body (no trailing newline).
pub(crate) fn worktree_help_text(role: Option<Role>) -> String {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        "Worktree commands for orchestrators (issue #239 Phase 2; mutating subcommands are orchestrator-only; status/list mirror the issue-status read surface):\n\n  acquire --issue N [--session S] [--base REF] [--isolate] [--format json]    Acquire or reuse a per-(repo, issue, session) worktree lease; auto-isolation defaults off (issue #247), use --isolate or PHASEGENT_WORKTREE_AUTO/worktree-auto=true to enable; session resolves from --session, PHASEGENT_SESSION_ID, or the legacy \"phasegent\" fallback; returns lease_id/path/branch/created/reason JSON\n  release --lease ID [--retain=true|false] [--force --reason TEXT]  Flip an active lease to retained (default) or released; never deletes the directory or the branch; --force records --reason on the row so the override stays attributable (lease rows are never deleted)\n  heartbeat --lease ID [--session SESSION]                         Refresh an active lease's heartbeat; only the owning session may update it; a foreign session or terminal lease returns a structured conflict\n  prune [--repo PATH] [--stale-days N] [--release-stale --reason TEXT] [--remove]  Single pruning entry point; default dry-run reports stale active leases and prunable worktrees, --release-stale flips stale active leases to retained (requires --reason), --remove deletes clean + expired + retained worktrees (combined runs recovery first); never deletes a branch and never removes a dirty worktree\n  status --issue N                                                 List active leases for an issue (read-only; available to orchestrator, executor, and reviewer)\n  list [--repo PATH]                                               List every lease for the resolved repo identity (read-only; --repo defaults to the current directory)"
            .to_owned()
    } else if role.is_some_and(is_read_role) {
        format!(
            "Worktree commands for {} (read-only surface):\n\n  status --issue N                                                 List active leases for an issue (read-only)\n  list [--repo PATH]                                               List every lease for the resolved repo identity (read-only; --repo defaults to the current directory)\n\nMutating subcommands (acquire, release, heartbeat, prune) are orchestrator-only.",
            role.unwrap().as_str()
        )
    } else {
        format!(
            "No worktree commands available for {}.",
            role.map_or("this role", Role::as_str)
        )
    }
}

pub(crate) fn print_worktree_help(role: Option<Role>) {
    println!("{}", worktree_help_text(role));
}

/// Per-subcommand `worktree` help body (no trailing newline).
pub(crate) fn worktree_command_help_text(role: Option<Role>, command: &str) -> String {
    match command {
        "acquire" => orchestrator_help(role, ACQUIRE_HELP),
        "release" => orchestrator_help(role, RELEASE_HELP),
        "status" => read_surface_help(role, STATUS_HELP),
        "list" => read_surface_help(role, LIST_HELP),
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
        assert!(text.contains("Auto-isolation defaults off"), "got: {text}");
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

    #[test]
    fn top_level_help_lists_every_mutating_subcommand_as_orchestrator_only() {
        let text = worktree_help_text(Some(Role::Orchestrator));
        for command in ["acquire", "release", "heartbeat", "prune"] {
            assert!(
                text.contains(command),
                "top-level help missing {command}; got: {text}"
            );
        }
        assert!(
            text.contains("mutating subcommands are orchestrator-only"),
            "got: {text}"
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
    }

    #[test]
    fn unknown_command_falls_back_to_top_level_help() {
        let text = worktree_command_help_text(Some(Role::Orchestrator), "fly");
        assert_eq!(text, worktree_help_text(Some(Role::Orchestrator)));
    }

    #[test]
    fn heartbeat_and_prune_are_documented_not_fallbacks() {
        for command in ["heartbeat", "prune"] {
            let text = worktree_command_help_text(Some(Role::Orchestrator), command);
            assert!(
                text.contains(&format!("Usage: worktree {command}")),
                "missing command help for {command}; got: {text}"
            );
        }
    }
}

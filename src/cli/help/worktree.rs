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

use crate::policy::Role;

pub(crate) fn print_worktree_help(role: Option<Role>) {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "Worktree commands for orchestrators (issue #239 Phase 2; mutating subcommands are orchestrator-only; status/list mirror the issue-status read surface):\n\n  acquire --issue N [--session S] [--base REF] [--format json]    Acquire or reuse a per-(repo, issue, session) worktree lease; returns lease_id/path/branch/created/reason JSON\n  release --lease ID [--retain=true|false]                        Flip an active lease to retained (default) or released; never deletes the directory or the branch\n  status --issue N                                                 List active leases for an issue (read-only; available to orchestrator, executor, and reviewer)\n  list [--repo PATH]                                               List every lease for the resolved repo identity (read-only; --repo defaults to the current directory)\n  prune [--stale-days N] [--dry-run]                               Remove clean + expired + retained worktrees; never deletes a branch; never removes a dirty worktree; never force-removes"
        );
    } else if role.is_some_and(is_read_role) {
        println!(
            "Worktree commands for {} (read-only surface):\n\n  status --issue N                                                 List active leases for an issue (read-only)\n  list [--repo PATH]                                               List every lease for the resolved repo identity (read-only; --repo defaults to the current directory)\n\nMutating subcommands (acquire, release, prune) are orchestrator-only.",
            role.unwrap().as_str()
        );
    } else {
        println!(
            "No worktree commands available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

pub(crate) fn print_worktree_command_help(role: Option<Role>, command: &str) {
    match command {
        "acquire" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: worktree acquire --issue N [--session S] [--base REF] [--format json]\n\nAcquire (or refresh) a per-(repo, issue, session) worktree lease. Idempotent: re-running with the same triple returns the same lease_id and updates the heartbeat (reason=\"idempotent\"). When no other lease is active for the repo, the current checkout is reused (reason=\"no_conflict\"); otherwise a fresh `phasegent/<issue>-<short6hex>` branch and a new worktree under ~/.cache/phasegent/worktrees/<fingerprint>/<slug> are created (reason=\"new_worktree\"). A checkout that is dirty and bound to a different issue also forces a fresh worktree (reason=\"new_worktree\") so an incoming task never lands in another task's dirty tree, with the trigger explained by warnings on stderr. Returns compact JSON on stdout. --session defaults to \"phasegent\"; --base is accepted for forward compatibility and the implementation always bases on HEAD. --format is json (the only accepted value). Orchestrator-only. The branch is never deleted, .env / secret material is never read or copied, and the lease table is created lazily through `CREATE TABLE IF NOT EXISTS` so pre-Phase-1 databases still open."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "release" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: worktree release --lease ID [--retain=true|false]\n\nFlip an active lease to retained (default) or released. --retain defaults to true; the boolean accepts true|1|yes|on and false|0|no|off. The release is a no-op when the lease is already in the requested terminal state. The directory and the branch are never deleted by `release`; that is `prune`'s job. Orchestrator-only."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "status" => {
            if role.is_none_or(is_read_role) {
                println!(
                    "Usage: worktree status --issue N\n\nList every active lease for the given issue id (read-only). Returns a JSON envelope with `{{ \"issue\": N, \"leases\": [...] }}`. The result is bounded by the storage layer (256 active rows max per query). Available to orchestrator, executor, and reviewer; tester is denied."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "list" => {
            if role.is_none_or(is_read_role) {
                println!(
                    "Usage: worktree list [--repo PATH]\n\nList every lease (active + terminal) for the resolved repo identity. --repo defaults to the current working directory; the value is canonicalised through `git rev-parse --git-common-dir` so the same physical repository yields the same identity from a main checkout, a linked worktree, or a subdirectory. Returns a JSON envelope with `{{ \"repo_identity\": \"...\", \"leases\": [...] }}`. Available to orchestrator, executor, and reviewer; tester is denied."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "prune" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: worktree prune [--stale-days N] [--dry-run]\n\nRemove clean + expired + retained worktrees. --stale-days defaults to 14. A lease is a prune candidate only when all three conditions hold: status is `retained`; heartbeat is older than stale-days days; and `git status --porcelain` reports an empty output. Dirty worktrees are never deleted (prunable=false, skipped_dirty); active and released leases are skipped; recent retained leases are skipped. `git worktree remove` is the only git command invoked, and it is never passed `--force`; branches are never deleted (no `git branch -D`). --dry-run lists candidates and reports prunable/skip without mutating. Returns a JSON envelope with scanned/candidates/pruned/skipped_* counts and a per-lease actions array. Orchestrator-only."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        _ => print_worktree_help(role),
    }
}

fn is_read_role(role: Role) -> bool {
    matches!(role, Role::Orchestrator | Role::Executor | Role::Reviewer)
}

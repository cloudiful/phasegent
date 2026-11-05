//! Guarded removal of a closed issue's worktree directories.
//!
//! Three local guards decide a candidate, in this order, and each one
//! keeps the directory with a bounded reason naming itself:
//!
//! 1. it is not the repository's main checkout;
//! 2. no *other* session holds an `active` lease on it;
//! 3. it is clean — `git status --porcelain` is empty, so untracked
//!    files count as dirty;
//! 4. no OpenCode session is still hosted in it (issue 747 P1).
//!
//! The fourth guard is the one that needs the host. A lease records where
//! a session *was*; the OpenCode API reports where it *is*, and which
//! session it descends from. Only persisted `ses_`-prefixed lease sessions
//! mark a candidate as OpenCode-associated, so a pure CLI close never reaches
//! the API and keeps its previous behaviour exactly.
//!
//! Everything is best-effort: the remote close already succeeded, so an
//! unreachable API, an incomplete listing, an unproven main checkout, or a
//! move that has not taken effect yet all keep the directory and surface
//! one stderr warning. Lease rows are never touched and branches are never
//! deleted.
//!
//! The same evidence backs `worktree prune`, which returns the idle members
//! of a candidate's lease family to the verified main checkout before it
//! removes a directory: only a lease-associated `ses_` session — or a
//! descendant of one — may be moved, and a running or foreign occupant keeps
//! the directory.

use std::path::{Path, PathBuf};

#[path = "cleanup/candidate.rs"]
pub(crate) mod candidate;
#[path = "cleanup/closure.rs"]
pub(crate) mod closure;
#[path = "cleanup/context.rs"]
pub(crate) mod context;
#[path = "cleanup/guards.rs"]
mod guards;
#[path = "cleanup/sessions.rs"]
pub(crate) mod sessions;
#[path = "cleanup/target.rs"]
pub(crate) mod target;
#[path = "cleanup/wait.rs"]
pub(crate) mod wait;

#[cfg(test)]
#[path = "cleanup/tests.rs"]
mod tests;
#[cfg(test)]
#[path = "cleanup/tests_evidence.rs"]
mod tests_evidence;
#[cfg(test)]
#[path = "cleanup/tests_modes.rs"]
mod tests_modes;
#[cfg(test)]
#[path = "cleanup/tests_prune_guards.rs"]
mod tests_prune_guards;

pub(crate) use context::CleanupContext;
pub(crate) use target::TargetResolver;

use crate::worktree::LeaseRow;
use crate::worktree::opencode::{OpenCodeApi, ProcessOpenCodeApi};
use crate::worktree::same_directory;

#[derive(Debug, PartialEq, Eq)]
pub enum AutoCleanupOutcome {
    Noop,
    Cleaned {
        removed: u64,
        kept: Vec<String>,
    },
    /// The repository identity or the lease store could not be resolved.
    /// Nothing was deleted; the remote close already succeeded, so this
    /// is a bounded warning only.
    Warning {
        reason: String,
    },
}

impl AutoCleanupOutcome {
    /// Stderr warning lines: one per preserved directory (reason first,
    /// directory verbatim), or the single warning of the
    /// unresolvable-context arm. Empty for a no-op and for a cleanup
    /// that removed every candidate.
    pub fn warnings(&self) -> Vec<String> {
        match self {
            Self::Noop => Vec::new(),
            Self::Cleaned { kept, .. } => kept.clone(),
            Self::Warning { reason } => vec![super::bounded(reason)],
        }
    }
}

/// How much freedom this pass has to touch an OpenCode session.
///
/// The distinction is a safety boundary, not a performance knob: the
/// reconciliation pass may *look* and never *move*, and the read-only
/// report pass does not probe at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupMode {
    /// `issue close`: may request a move for the closing session and for the
    /// idle members of its lease family.
    Close,
    /// `worktree prune --remove`: may request a move for the idle members of
    /// the candidate's lease family. There is no running exemption here.
    Prune,
    /// `worktree prune --dryrun`: the read-only preview of the removal
    /// stage. It probes the same occupancy and lineage evidence and reports
    /// what the removal would do, but it never moves anything.
    Preview,
    /// `issue sync` clean mode: occupancy is checked, sessions are never
    /// moved, so a directory the close could not release stays until a
    /// later pass finds it free.
    Sync,
    /// `issue sync --no-clean`: writes nothing and probes nothing, so an
    /// OpenCode-associated candidate is conservatively reported as kept.
    Report,
    /// A post-cache revalidation: reads current occupancy and running state
    /// directly, never moves a session, and keeps whenever any session still
    /// occupies the directory.
    Verify,
}

impl CleanupMode {
    /// Whether this pass may relocate an idle member of the candidate's
    /// lease family out of it. Only the close and prune chains may.
    pub(crate) fn may_move_sessions(self) -> bool {
        matches!(self, Self::Close | Self::Prune)
    }

    /// Whether this pass reads the connected service's running-session set.
    /// The reconciliation pass does not: it never moves a session, so
    /// "idle" is not a question it has to answer.
    pub(crate) fn reads_running_state(self) -> bool {
        matches!(
            self,
            Self::Close | Self::Prune | Self::Preview | Self::Verify
        )
    }

    /// Whether a candidate without a lease-associated OpenCode session is
    /// still enumerated. The close and reconciliation chains decide such a
    /// candidate by the local guards alone and never reach the host; the
    /// prune chain enumerates it, because an occupant no lease associates is
    /// somebody else's session and keeps the directory.
    pub(crate) fn enumerates_unmanaged_candidates(self) -> bool {
        matches!(self, Self::Prune | Self::Preview | Self::Verify)
    }
}

/// Delete the closed issue's clean worktree directories in this
/// repository, after the remote close and the lease release succeeded.
///
/// The dirty probe ([`crate::worktree::is_clean`]) and the removal call
/// ([`crate::worktree::worktree_remove`]) are the same primitives the
/// `worktree prune --remove` pass is built on; only the guards differ.
/// [`crate::cli::sync`] drives [`cleanup_closed_issue_worktrees_with`]
/// directly with the narrower mode its own pass is allowed.
pub fn cleanup_closed_issue_worktrees(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
    closure: &dyn closure::DirectoryIssueClosure,
) -> AutoCleanupOutcome {
    cleanup_closed_issue_worktrees_with(
        runner,
        &ProcessOpenCodeApi::new(),
        CleanupMode::Close,
        repo_path,
        issue,
        session,
        closure,
    )
}

/// Injected core of [`cleanup_closed_issue_worktrees`]. `api` and `mode`
/// are the only knobs the tests turn; every predicate below is the
/// production one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cleanup_closed_issue_worktrees_with(
    runner: &dyn crate::worktree::WorktreeRunner,
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
    closure: &dyn closure::DirectoryIssueClosure,
) -> AutoCleanupOutcome {
    let context = match context::resolve(runner, repo_path, issue) {
        Ok(context) => context,
        Err(reason) => return AutoCleanupOutcome::Warning { reason },
    };
    cleanup_closed_issue_worktrees_in_context(
        runner,
        &candidate::DirectOccupancy(api),
        mode,
        context,
        repo_path,
        issue,
        session,
        closure,
        None,
    )
}

/// Injected core of [`cleanup_closed_issue_worktrees`] for a provider-scoped
/// close: the exact resolved `(provider, project, external_id)` scope the
/// closed issue was resolved under. A directory whose scoped leases all
/// match it exactly may proceed through the same guards; mixed, foreign,
/// and unscoped-context directories keep. Every other guard is unchanged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cleanup_closed_issue_worktrees_with_scope(
    runner: &dyn crate::worktree::WorktreeRunner,
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
    closure: &dyn closure::DirectoryIssueClosure,
    allowed_scope: &crate::worktree::LeaseScope,
) -> AutoCleanupOutcome {
    let context = match context::resolve(runner, repo_path, issue) {
        Ok(context) => context,
        Err(reason) => return AutoCleanupOutcome::Warning { reason },
    };
    cleanup_closed_issue_worktrees_in_context(
        runner,
        &candidate::DirectOccupancy(api),
        mode,
        context,
        repo_path,
        issue,
        session,
        closure,
        Some(allowed_scope),
    )
}

/// Public scoped close cleanup: the production close path for an explicitly
/// resolved Forgejo issue.
pub fn cleanup_closed_issue_worktrees_scoped(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
    closure: &dyn closure::DirectoryIssueClosure,
    allowed_scope: &crate::worktree::LeaseScope,
) -> AutoCleanupOutcome {
    cleanup_closed_issue_worktrees_with_scope(
        runner,
        &ProcessOpenCodeApi::new(),
        CleanupMode::Close,
        repo_path,
        issue,
        session,
        closure,
        allowed_scope,
    )
}

/// Injected core of the guarded cleanup: the caller supplies the resolved
/// context (repository identity plus the rows as they are *now*), the host
/// guard, and the closure evidence for the directory's other issues. The
/// reconciliation pass uses this to reuse its store, identity and an
/// invocation-local host memo, while every caller that does not inject
/// still reaches the exact same predicate through
/// [`cleanup_closed_issue_worktrees_with`].
///
/// The pass re-reads the complete lease evidence after every family
/// movement and immediately before each removal: a new active owner or a
/// newly associated open issue that appeared during the (possibly slow)
/// moves keeps the directory. The initial snapshot alone never authorizes
/// a deletion.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cleanup_closed_issue_worktrees_in_context(
    runner: &dyn crate::worktree::WorktreeRunner,
    guard: &dyn candidate::OccupancyGuard,
    mode: CleanupMode,
    context: CleanupContext,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
    closure: &dyn closure::DirectoryIssueClosure,
    allowed_scope: Option<&crate::worktree::LeaseScope>,
) -> AutoCleanupOutcome {
    if !context.has_issue(issue) {
        return AutoCleanupOutcome::Noop;
    }
    let session = session.map(str::trim).filter(|value| !value.is_empty());
    // The move target is only needed by an OpenCode-associated candidate
    // and only by a pass that may move, so it is resolved at most once per
    // pass — and never at all for a pure CLI close, a reconciliation
    // pass, or a read-only report.
    let target = TargetResolver::new(repo_path, &context.identity);
    let mut removed = 0u64;
    let mut kept: Vec<String> = Vec::new();
    // A provider-scoped pass only considers the closed issue's own scoped
    // leases: a same-number legacy row, or a row of another provider scope,
    // is never a candidate of this pass (it stays untouched).
    for row in
        context
            .rows
            .iter()
            .filter(|row| row.issue == issue)
            .filter(|row| match allowed_scope {
                Some(scope) => context.scopes.get(&row.lease_id) == Some(scope),
                None => true,
            })
    {
        let directory = PathBuf::from(&row.worktree_path);
        if !directory.exists() {
            continue;
        }
        if !context.directory_scope_allowed(&directory, allowed_scope) {
            kept.push(kept_entry(
                issue,
                "scoped provider lease requires provider-aware cleanup",
                &row.worktree_path,
            ));
            continue;
        }
        // The local guards run first and read nothing from the host: a main
        // checkout, an active peer's path, or a dirty/untracked directory is
        // excluded here, so it never reaches the host or the provider. A
        // cache-only verdict carries its initial inventory as the baseline the
        // later removal must still match.
        let local = candidate::local_guard(runner, &context.identity, &context.rows, session, row);
        if let Some(reason) = local.protected_reason {
            kept.push(kept_entry(issue, &reason, &row.worktree_path));
            continue;
        }
        let cache = local.cache;
        // The closure of the directory's *other* issues is read before any
        // session may be moved: a directory an open issue shares is kept,
        // and moving one of our sessions out of it would be a side effect
        // of a removal that must not happen.
        let issues = associated_issues(&context.rows, &directory);
        if let Err(reason) = closure.confirm_others_closed(issue, &issues) {
            kept.push(kept_entry(issue, &reason, &row.worktree_path));
            continue;
        }
        let blocked = guard.keep_reason(
            mode,
            &target,
            &directory,
            &associated_sessions(&context.rows, &directory),
            session,
        );
        if let Some(reason) = blocked {
            kept.push(kept_entry(issue, &reason, &row.worktree_path));
            continue;
        }
        // The guard moved the family (or found it already gone). Its answers
        // are not a permission: rows, local guards, and closure evidence are
        // re-read *now*, after any movement, because a new active owner or a
        // newly linked open issue that appeared during the moves must keep
        // the directory. The candidate's own rows are re-derived from the
        // fresh read, never from the invocation-start snapshot.
        if let Err(reason) = reverify_after_movement(
            runner,
            closure,
            mode,
            &context.identity,
            issue,
            session,
            &directory,
            &row.worktree_path,
            cache.as_ref(),
            allowed_scope,
        ) {
            kept.push(kept_entry(issue, &reason, &row.worktree_path));
            continue;
        }
        // The only mutation allowed before the no-force removal is the shared
        // root-.cache clearance: it compares the initial eligibility baseline,
        // re-reads the inventory immediately before deleting, and leaves every
        // other untracked path untouched. Report mode performs no local or host
        // mutation at all, so it skips both this clearance and its re-check.
        if mode != CleanupMode::Report {
            if let Err(reason) =
                crate::worktree::cache::clear_cache_only(runner, &directory, cache.as_ref())
            {
                kept.push(kept_entry(
                    issue,
                    &format!("cache cleanup could not finish ({reason})"),
                    &row.worktree_path,
                ));
                continue;
            }
            // Clearing the cache can take a while, so every applicable safety
            // gate is re-read once more before the no-force removal. A new
            // active owner, a reopened association, or a running/foreign
            // occupant keeps the directory; the cache is never rolled back and
            // no safeguard is overridden.
            if let Err(reason) = recheck_after_cache(
                runner,
                guard,
                &target,
                &context.identity,
                issue,
                session,
                &directory,
                closure,
                allowed_scope,
            ) {
                kept.push(kept_entry(
                    issue,
                    &format!("kept after cache cleanup ({reason})"),
                    &row.worktree_path,
                ));
                continue;
            }
        }
        // Run the removal from inside the candidate: it exists at this
        // point, while the close's own working directory may already be
        // a removed sibling by the time a later candidate is handled.
        match crate::worktree::worktree_remove(runner, &directory, &directory) {
            Ok(()) => removed += 1,
            Err(error) => kept.push(kept_entry(
                issue,
                &format!(
                    "worktree removal failed ({})",
                    super::bounded(&error.message)
                ),
                &row.worktree_path,
            )),
        }
    }
    if removed == 0 && kept.is_empty() {
        return AutoCleanupOutcome::Noop;
    }
    AutoCleanupOutcome::Cleaned { removed, kept }
}

/// The post-movement re-verification every removal path shares: reload the
/// complete lease rows, re-check the candidate's own local guards against
/// them, and re-validate the directory's other issue closures with the
/// memo dropped. An unreadable lease store, a vanished candidate row, a new
/// active owner, dirt, or an open neighbour each keep the directory.
///
/// A pass that cannot move anything never changed local or provider state,
/// so re-reading it would answer the same question twice; the movement
/// window is what makes the re-read necessary.
#[allow(clippy::too_many_arguments)]
fn reverify_after_movement(
    runner: &dyn crate::worktree::WorktreeRunner,
    closure: &dyn closure::DirectoryIssueClosure,
    mode: CleanupMode,
    identity: &str,
    issue: u64,
    session: Option<&str>,
    directory: &Path,
    worktree_path: &str,
    expected_cache: Option<&crate::worktree::cache::CacheInventory>,
    allowed_scope: Option<&crate::worktree::LeaseScope>,
) -> Result<(), String> {
    if !mode.may_move_sessions() {
        return Ok(());
    }
    closure.refresh();
    let fresh_context =
        match CleanupContext::load(&crate::infra::storage::Storage::open()?, identity) {
            Ok(context) => context,
            Err(reason) => {
                return Err(format!(
                    "the lease evidence could not be re-read after the moves: {reason}"
                ));
            }
        };
    if !fresh_context.directory_scope_allowed(directory, allowed_scope) {
        return Err("scoped provider lease requires provider-aware cleanup".to_owned());
    }
    let fresh = fresh_context.rows;
    let fresh_row = fresh
        .iter()
        .find(|row| same_directory(Path::new(&row.worktree_path), directory))
        .ok_or_else(|| {
            format!(
                "the candidate's lease row is gone after the moves ({worktree_path}); the directory survives"
            )
        })?;
    let fresh_local = candidate::local_guard(runner, identity, &fresh, session, fresh_row);
    if let Some(reason) = fresh_local.protected_reason {
        return Err(format!("the directory changed after the moves ({reason})"));
    }
    if fresh_local.cache.as_ref() != expected_cache {
        return Err("the root .cache inventory changed after the moves".to_owned());
    }
    let fresh_issues = associated_issues(&fresh, directory);
    if let Err(reason) = closure.confirm_others_closed(issue, &fresh_issues) {
        return Err(format!(
            "the directory's issue closures changed after the moves ({reason})"
        ));
    }
    Ok(())
}

/// The post-cache re-verification: after the root `.cache` was cleared and
/// immediately before the no-force removal, reload the complete lease rows,
/// re-run the local guards, re-read the directory's issue closures, and ask the
/// host guard again with a no-move mode. Any new active owner, reopened
/// association, running or foreign occupant, or reappearing dirty content keeps
/// the directory. The cache is never rolled back.
#[allow(clippy::too_many_arguments)]
fn recheck_after_cache(
    runner: &dyn crate::worktree::WorktreeRunner,
    guard: &dyn candidate::OccupancyGuard,
    target: &TargetResolver<'_>,
    identity: &str,
    issue: u64,
    session: Option<&str>,
    directory: &Path,
    closure: &dyn closure::DirectoryIssueClosure,
    allowed_scope: Option<&crate::worktree::LeaseScope>,
) -> Result<(), String> {
    closure.refresh();
    let fresh_context =
        match CleanupContext::load(&crate::infra::storage::Storage::open()?, identity) {
            Ok(context) => context,
            Err(reason) => {
                return Err(format!("the lease evidence could not be re-read: {reason}"));
            }
        };
    if !fresh_context.directory_scope_allowed(directory, allowed_scope) {
        return Err("scoped provider lease requires provider-aware cleanup".to_owned());
    }
    let fresh = fresh_context.rows;
    let fresh_row = fresh
        .iter()
        .find(|row| same_directory(Path::new(&row.worktree_path), directory))
        .ok_or_else(|| "the candidate's lease row disappeared".to_owned())?;
    let local = candidate::local_guard(runner, identity, &fresh, session, fresh_row);
    if let Some(reason) = local.protected_reason {
        return Err(reason);
    }
    if local.cache.is_some() {
        return Err("a root .cache reappeared before the worktree removal".to_owned());
    }
    let issues = associated_issues(&fresh, directory);
    closure.confirm_others_closed(issue, &issues)?;
    let associated = associated_sessions(&fresh, directory);
    if associated.is_empty() {
        // A pure CLI candidate keeps its historical no-host contract. Prune's
        // explicit runtime-host dependency is handled by its own pass.
        return Ok(());
    }
    let blocked = guard.keep_reason(CleanupMode::Verify, target, directory, &associated, None);
    match blocked {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// The distinct issues the persisted leases associate with one directory.
pub(crate) fn associated_issues(rows: &[LeaseRow], directory: &Path) -> Vec<u64> {
    let mut issues: Vec<u64> = rows
        .iter()
        .filter(|row| same_directory(Path::new(&row.worktree_path), directory))
        .map(|row| row.issue)
        .collect();
    issues.sort_unstable();
    issues.dedup();
    issues
}

/// Distinct OpenCode-managed sessions whose persisted lease points at
/// `directory`, sorted so one pass reports identically every time.
pub(crate) fn associated_sessions(rows: &[LeaseRow], directory: &Path) -> Vec<String> {
    let mut ids: Vec<String> = rows
        .iter()
        .filter(|row| same_directory(Path::new(&row.worktree_path), directory))
        .map(|row| row.session.trim().to_owned())
        .filter(|session| crate::worktree::opencode::is_opencode_session(session))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn kept_entry(issue: u64, reason: &str, directory: &str) -> String {
    format!("issue {issue} closed; kept worktree ({reason}): {directory}")
}

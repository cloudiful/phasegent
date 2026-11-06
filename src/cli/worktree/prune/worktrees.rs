//! Directory half of `worktree prune`: the read-only candidate scan and
//! the removal apply.
//!
//! Since issue 916 the unit of candidacy is a **canonical directory**, not a
//! lease row: every lease row whose `worktree_path` resolves to the same
//! directory is grouped together and considered once. A directory is removed
//! only when
//!
//! 1. no `active` lease references it — including a row belonging to the same
//!    session, and including the invocation-start snapshot so a stale recovery
//!    in the same call can never make an active directory removable;
//! 2. the local Git guards clear it (never the main checkout, never dirty or
//!    untracked) — before any host or provider read;
//! 3. the connected shared OpenCode service reports no *currently running*
//!    session in it (issue 928 P1) — historical session records that still point
//!    at the directory never block, and the scan never moves a session;
//! 4. every associated issue is *confirmed closed* by
//!    [`IssueStateSource`][crate::cli::issue_closure::IssueStateSource];
//! 5. the removal stage proves the directory is free of sessions it may not
//!    touch and returns the associated idle ones it may (issue 1061 P1);
//! 6. `git worktree remove` (no `--force`) succeeds after a fresh re-read of
//!    rows and guards.
//!
//! The removal stage is the only place that may move a session, and it moves
//! only a lease-associated `ses_` session that the host reports as idle and
//! hosted in the directory: the move goes to the repository's verified main
//! checkout and is confirmed by a fresh authoritative read. A running session,
//! a foreign occupant, an incomplete listing, an unresolvable associated
//! session, an answer that names another session or reports a location that
//! cannot be read, an unavailable host, a rejected, queued, or unconfirmed
//! move, and an invalid target all keep the directory with a bounded reason.
//!
//! Prune's runtime-host dependency is explicit: even a pure-CLI candidate needs
//! a valid running-session answer before removal, and an unavailable, malformed,
//! or mismatched answer keeps the directory. Age never gates directory deletion:
//! a freshly retained, closed, clean directory is removable immediately.
//! `--stale-days` remains only for the explicit stale-active recovery half.
//! Lease rows and branches are preserved as history.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::report_local_warnings;
use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::TargetResolver;
use crate::lifecycle::cleanup::sessions::{self, FamilyOutcome};
use crate::lifecycle::cleanup::wait::MovePolicy;
use crate::lifecycle::cleanup::{CleanupMode, associated_sessions};
use crate::worktree::git::worktree_remove;
use crate::worktree::leases::{list_all_for_repo, load_lease, update_status};
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::{
    CanonicalPaths, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    WorktreeRunner, canonical_or_self,
};

use super::active_guard;
use crate::cli::issue_closure::{IssueClosure, IssueStateSource, TaskScopeRef};

/// Whether a prune pass only reports candidates or also executes the
/// actions it classified. The executor maps a non-preview invocation onto
/// [`PruneMode::Remove`]; `--dryrun` selects [`PruneMode::Report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneMode {
    /// Read-only: classify and report, never write.
    Report,
    /// Execute the removals in addition to reporting them.
    Remove,
}

impl PruneMode {
    fn is_report(self) -> bool {
        matches!(self, PruneMode::Report)
    }
}

/// One repository scope of a prune pass: its canonical identity, the
/// checkout the shared guards validate against, the rows recorded
/// against it, and the lazily resolved move target its associated idle
/// sessions may be returned to.
pub(crate) struct PruneScope<'a> {
    pub identity: &'a str,
    pub repo_path: &'a Path,
    pub rows: &'a [LeaseRow],
    pub target: &'a TargetResolver<'a>,
}

/// Read-only classification of one canonical directory before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PruneDisposition {
    /// At least one `active` lease referenced the directory.
    SkippedActive,
    /// A lease row carried an unrecognised status; kept safely.
    SkippedUnknownStatus,
    /// The directory does not exist; counted separately, nothing removed.
    MissingDirectory,
    /// A shared candidate guard kept the directory.
    SkippedProtected(String),
    /// An associated issue is not confirmed closed.
    SkippedIssue(String),
    /// Every condition held; the directory may be removed.
    Removable,
}

/// Every row that resolves to one canonical directory.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryCandidate {
    pub canonical_path: PathBuf,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub lease_ids: Vec<String>,
    /// The subset of `lease_ids` whose persisted row is already terminal
    /// `released`. A repeated absent run must not rewrite their heartbeat.
    pub released_lease_ids: Vec<String>,
    pub issues: Vec<u64>,
    pub disposition: PruneDisposition,
    /// Whether the scan saw a sidecar scope on this directory; a later
    /// recheck must keep when that proof disappeared or became mixed.
    pub scoped: bool,
    /// `(lease_id, status, worktree_path)` as scanned, used to revalidate the
    /// exact terminal row identity before an absent-path update.
    pub lease_proof: Vec<(String, String, String)>,
    /// Complete affected-directory ownership projection approved at scan time
    /// for the absent-release transaction.
    pub release_proof: crate::worktree::leases::ScopedRetentionProof,
    /// The exact root `.cache` baseline assessed before any host or session
    /// movement; the later removal must still match it.
    pub cache: Option<crate::worktree::cache::CacheInventory>,
}

/// One directory action of the prune pass. `prunable` is the explicit safety
/// signal; the caller can tell at a glance which directories were removed,
/// which are absent, and which were kept and why.
#[derive(Debug, Serialize)]
pub(crate) struct PruneAction {
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub lease_ids: Vec<String>,
    pub issues: Vec<u64>,
    pub prunable: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Retained for JSON compatibility only. The removal stage returns an
    /// associated idle session to the verified main checkout, but this list
    /// stays empty and is therefore omitted; the outcome is reported through
    /// the existing action message (issue 1061 P1).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relocated_children: Vec<String>,
    /// Retained for JSON compatibility only. A removal never proposes a
    /// relocation, so this list is always empty and therefore omitted; it is
    /// never populated or logged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub planned_children: Vec<String>,
}

/// Envelope returned by the directory pass for one repository. All counts are
/// directory-based, so a lease row is never mistaken for a directory.
#[derive(Debug, Serialize)]
pub(crate) struct PruneSummary {
    pub scanned: usize,
    pub removed: usize,
    pub eligible: usize,
    pub absent: usize,
    pub kept_active: usize,
    pub kept_unknown: usize,
    pub kept_protected: usize,
    pub kept_issue: usize,
    pub failed: usize,
    pub dry_run: bool,
    pub stale_days: u32,
    pub actions: Vec<PruneAction>,
}

/// One grouped directory before classification.
struct DirectoryGroup {
    canonical_path: PathBuf,
    worktree_path: String,
    branch: String,
    status: String,
    heartbeat_at: i64,
    lease_ids: Vec<String>,
    issues: Vec<u64>,
    rows: Vec<LeaseRow>,
    scoped: bool,
    /// The lease ids in this group that carry a sidecar scope, and each
    /// one's `(provider, project)` key. Populated by the scan; the
    /// classification compares these keys against the invocation's
    /// provider context.
    scoped_lease_ids: Vec<String>,
    scoped_lease_keys: Vec<ScopedLeaseKey>,
}

/// Group lease rows by canonical directory path, in a deterministic order.
fn group_rows(rows: &[LeaseRow], paths: &CanonicalPaths) -> Vec<DirectoryGroup> {
    let mut groups: Vec<DirectoryGroup> = Vec::new();
    let mut index: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    // `rows` arrive newest-first; sorting the output keeps the pass stable
    // regardless of row order.
    let mut ordered: Vec<&LeaseRow> = rows.iter().collect();
    ordered.sort_by(|a, b| a.lease_id.cmp(&b.lease_id));
    for row in ordered {
        let canonical = paths.resolve(Path::new(&row.worktree_path));
        match index.get(&canonical) {
            Some(position) => {
                let group = &mut groups[*position];
                group.lease_ids.push(row.lease_id.clone());
                if !group.issues.contains(&row.issue) {
                    group.issues.push(row.issue);
                }
                group.rows.push(row.clone());
                if group.status != row.status {
                    group.status = "mixed".to_owned();
                }
            }
            None => {
                index.insert(canonical.clone(), groups.len());
                groups.push(DirectoryGroup {
                    canonical_path: canonical,
                    worktree_path: row.worktree_path.clone(),
                    branch: row.branch.clone(),
                    status: row.status.clone(),
                    heartbeat_at: row.heartbeat_at,
                    lease_ids: vec![row.lease_id.clone()],
                    issues: vec![row.issue],
                    rows: vec![row.clone()],
                    scoped: false,
                    scoped_lease_ids: Vec::new(),
                    scoped_lease_keys: Vec::new(),
                });
            }
        }
    }
    for group in &mut groups {
        group.issues.sort_unstable();
    }
    groups.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    groups
}

/// Scan every lease row into a read-only directory candidate set. Storage is
/// not touched and no `git` mutation runs. The issue-state source is consulted
/// only for a directory that already passed the active, absent, unknown, local,
/// and running-session guards, so no active/missing/main-checkout/dirty path
/// resolves a credential.
#[cfg(test)]
pub(crate) fn scan_worktree_candidates(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
) -> Vec<DirectoryCandidate> {
    scan_with_paths(
        runner,
        api,
        scope,
        pinned_active,
        source,
        &CanonicalPaths::default(),
    )
}

#[cfg(test)]
fn scan_with_paths(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    paths: &CanonicalPaths,
) -> Vec<DirectoryCandidate> {
    scan_scoped_with_paths(
        runner,
        api,
        scope,
        pinned_active,
        source,
        paths,
        &std::collections::HashMap::new(),
        None,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_scoped_with_paths(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    paths: &CanonicalPaths,
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
) -> Vec<DirectoryCandidate> {
    group_rows(scope.rows, paths)
        .into_iter()
        .map(|mut group| {
            attach_scope_proof(&mut group, scopes);
            classify(
                runner,
                api,
                scope,
                pinned_active,
                source,
                group,
                paths,
                scopes,
                provider_scope,
                explicit_forgejo,
            )
        })
        .collect()
}

/// One classification: the ordered disposition plus the exact root `.cache`
/// baseline assessed before any host or session movement.
#[derive(Debug, Clone)]
struct Classification {
    disposition: PruneDisposition,
    cache: Option<crate::worktree::cache::CacheInventory>,
}

impl Classification {
    fn keep(disposition: PruneDisposition) -> Self {
        Self {
            disposition,
            cache: None,
        }
    }
}

/// Classify one directory group. Read-only.
#[allow(clippy::too_many_arguments)]
fn classify(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    group: DirectoryGroup,
    paths: &CanonicalPaths,
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
) -> DirectoryCandidate {
    let classification = classify_group(
        runner,
        api,
        scope,
        pinned_active,
        source,
        &group,
        paths,
        scopes,
        provider_scope,
        explicit_forgejo,
    );
    let released_lease_ids = group
        .rows
        .iter()
        .filter(|row| row.status == LEASE_STATUS_RELEASED)
        .map(|row| row.lease_id.clone())
        .collect();
    let release_proof = crate::worktree::leases::directory_ownership_proof(
        &group.canonical_path,
        scope.rows,
        scopes,
    );
    DirectoryCandidate {
        canonical_path: group.canonical_path,
        worktree_path: group.worktree_path,
        branch: group.branch,
        status: group.status,
        heartbeat_at: group.heartbeat_at,
        lease_ids: group.lease_ids,
        released_lease_ids,
        issues: group.issues,
        disposition: classification.disposition,
        scoped: group.scoped,
        lease_proof: group
            .rows
            .iter()
            .map(|row| {
                (
                    row.lease_id.clone(),
                    row.status.clone(),
                    row.worktree_path.clone(),
                )
            })
            .collect(),
        release_proof,
        cache: classification.cache,
    }
}

/// The ordered decision for one directory: active lease, unrecognised status,
/// absent, local Git guards, current-service running occupancy, closure. The
/// first four read nothing from the host, so a main/dirty/active-lease
/// directory makes zero host or provider calls; the running-session query runs
/// only for an otherwise-local-eligible existing directory, and closure is read
/// only for one that also passed it.
#[allow(clippy::too_many_arguments)]
fn classify_group(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    group: &DirectoryGroup,
    paths: &CanonicalPaths,
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
) -> Classification {
    // Explicit Forgejo admits only complete exact-scope ownership. A purely
    // legacy group, an absent path, and every mixed/foreign/same-number
    // combination keep before provider reads or removal; compatible legacy
    // providers keep their unscoped reconciliation.
    if group
        .issues
        .iter()
        .any(|issue| same_number_ambiguous(*issue, scope.rows, scopes))
    {
        return Classification::keep(PruneDisposition::SkippedIssue(
            "same-number unscoped history makes the ownership ambiguous".to_owned(),
        ));
    }
    if (explicit_forgejo || provider_scope.is_some_and(|(provider, _)| provider == "forgejo"))
        && !group.scoped
    {
        return Classification::keep(PruneDisposition::SkippedIssue(
            "explicit Forgejo does not own unscoped legacy history".to_owned(),
        ));
    }
    if group.scoped && !scoped_group_matches(provider_scope, group) {
        return Classification::keep(PruneDisposition::SkippedIssue(
            "scoped provider lease requires provider-aware cleanup".to_owned(),
        ));
    }
    let canonical = group.canonical_path.to_string_lossy().into_owned();
    if group
        .rows
        .iter()
        .any(|row| row.status == LEASE_STATUS_ACTIVE)
        || pinned_active.contains(&canonical)
    {
        return Classification::keep(PruneDisposition::SkippedActive);
    }
    if group.rows.iter().any(|row| {
        row.status != LEASE_STATUS_ACTIVE
            && row.status != LEASE_STATUS_RETAINED
            && row.status != LEASE_STATUS_RELEASED
    }) {
        // An unrecognised status is kept safely before anything else is
        // inferred from the path.
        return Classification::keep(PruneDisposition::SkippedUnknownStatus);
    }
    match path_entry(&group.canonical_path) {
        PathEntry::Present => {}
        PathEntry::Missing => return Classification::keep(PruneDisposition::MissingDirectory),
        PathEntry::Unreadable(reason) => {
            return Classification::keep(PruneDisposition::SkippedProtected(reason));
        }
    }
    // The local Git guards run first and read nothing from the host: a main
    // checkout, an active peer's path, or a dirty/untracked directory is
    // excluded here, so it never reaches the running-session query. A
    // cache-only verdict carries the initial inventory baseline forward.
    let local = paths.evaluate_guards(
        runner,
        scope.identity,
        scope.rows,
        None,
        &representative(group),
    );
    if let Some(reason) = local.protected_reason {
        return Classification::keep(PruneDisposition::SkippedProtected(reason));
    }
    let cache = local.cache;
    // The running-session guard: occupancy is free only when the connected
    // service gave a complete, valid answer and no running session (root or
    // child) is currently in the directory. An unreadable, malformed, or
    // mismatched answer keeps the directory rather than reading as vacancy.
    match active_guard::RunningSnapshot::capture(api) {
        Err(reason) => {
            return Classification::keep(PruneDisposition::SkippedProtected(reason));
        }
        Ok(snapshot) => {
            if let Some(reason) = snapshot.keep_reason(&group.canonical_path) {
                return Classification::keep(PruneDisposition::SkippedProtected(reason));
            }
        }
    }
    match closure_of(source, scope, group) {
        IssueClosure::Closed => Classification {
            disposition: PruneDisposition::Removable,
            cache,
        },
        IssueClosure::Keep(reason) => Classification::keep(PruneDisposition::SkippedIssue(reason)),
    }
}

/// Whether a scoped directory group matches the invocation's provider
/// context exactly. Every lease of the group must carry a sidecar scope —
/// a mixed legacy/scoped directory keeps — and every key must equal the
/// resolved dispatcher's provider and repository/project. A missing
/// context, a mixed scope, or a foreign scope keeps. The per-issue
/// external-id identity is the issue-state source's job (its link-scope
/// read compares provider/project/external id per number), never this
/// repo-level check.
fn same_number_ambiguous(
    issue: u64,
    rows: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> bool {
    let scoped = rows
        .iter()
        .any(|row| row.issue == issue && scopes.contains_key(&row.lease_id));
    let unscoped = rows
        .iter()
        .any(|row| row.issue == issue && !scopes.contains_key(&row.lease_id));
    scoped && unscoped
}

/// The scope one scoped lease names.
///
/// A durable task names its own provider, project and instance: the group is
/// matched against that exact task scope, never against the pass-wide
/// default, so a task on another instance is never swept by a legacy
/// selector.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ScopedLeaseKey {
    provider: String,
    project: String,
    task_id: Option<i64>,
    instance: Option<String>,
}

/// The one exact scope a group's scoped leases name, when they name one.
///
/// A group whose scoped leases name different scopes is mixed evidence: it
/// never matches and keeps before any read or mutation.
fn group_scope(group: &DirectoryGroup) -> Option<&ScopedLeaseKey> {
    let first = group.scoped_lease_keys.first()?;
    if group.scoped_lease_keys.iter().any(|key| key != first) {
        return None;
    }
    Some(first)
}

fn scoped_group_matches(provider_scope: Option<(&str, &str)>, group: &DirectoryGroup) -> bool {
    if !group.scoped
        || group.scoped_lease_ids.len() != group.lease_ids.len()
        || group.scoped_lease_keys.is_empty()
    {
        return false;
    }
    let Some(key) = group_scope(group) else {
        return false;
    };
    match key.task_id {
        // A durable task authorizes its own routing: its provider and project
        // come from the task, not from the pass-wide default.
        Some(_) => true,
        None => match provider_scope {
            Some((provider, project)) => key.provider == provider && key.project == project,
            None => false,
        },
    }
}

fn attach_scope_proof(
    group: &mut DirectoryGroup,
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) {
    group.scoped = false;
    group.scoped_lease_ids.clear();
    group.scoped_lease_keys.clear();
    for id in &group.lease_ids {
        if let Some(lease_scope) = scopes.get(id) {
            group.scoped = true;
            group.scoped_lease_ids.push(id.clone());
            group.scoped_lease_keys.push(ScopedLeaseKey {
                provider: lease_scope.provider().to_owned(),
                project: lease_scope.project().to_owned(),
                task_id: lease_scope.task_id(),
                instance: lease_scope.instance().map(str::to_owned),
            });
        }
    }
}

fn representative(group: &DirectoryGroup) -> LeaseRow {
    group.rows.first().cloned().unwrap_or_else(|| LeaseRow {
        lease_id: String::new(),
        repo_identity: String::new(),
        issue: 0,
        session: String::new(),
        checkout_path: String::new(),
        worktree_path: group.worktree_path.clone(),
        branch: group.branch.clone(),
        status: group.status.clone(),
        created_at: group.heartbeat_at,
        heartbeat_at: group.heartbeat_at,
        release_reason: None,
    })
}

/// The filesystem entry state of a candidate path. Only a genuine `NotFound`
/// is absence; a permission or I/O error, and a dangling or resolvable symlink
/// entry, are never silently treated as a missing directory.
enum PathEntry {
    Present,
    Missing,
    Unreadable(String),
}

fn path_entry(path: &Path) -> PathEntry {
    match std::fs::symlink_metadata(path) {
        Ok(_) => PathEntry::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PathEntry::Missing,
        Err(error) => PathEntry::Unreadable(format!(
            "could not verify the worktree directory: {}",
            crate::worktree::bounded(&error.to_string())
        )),
    }
}
/// Closure requires *every* associated issue to be confirmed closed.
///
/// A directory whose own scope names a durable task reads that issue through
/// the task's pinned instance and every other associated number through its own
/// durable task in this repository. Every other directory keeps the previous
/// configured-provider read, so no legacy behaviour changes.
fn closure_of(
    source: &dyn IssueStateSource,
    scope: &PruneScope<'_>,
    group: &DirectoryGroup,
) -> IssueClosure {
    let own = own_task_scope(group);
    let mut keep: Vec<String> = Vec::new();
    for issue in &group.issues {
        let verdict = match &own {
            Some((task, own_issue)) if issue == own_issue => {
                source.own_task_closure(scope.repo_path, *task, *issue)
            }
            Some(_) => source.associated_closure(scope.repo_path, *issue),
            None => source.closure(scope.repo_path, *issue),
        };
        if !verdict.is_closed()
            && let Some(reason) = verdict.keep_reason()
        {
            keep.push(reason.to_owned());
        }
    }
    if keep.is_empty() {
        IssueClosure::Closed
    } else {
        IssueClosure::Keep(keep.join("; "))
    }
}

/// The exact durable task a directory's own scope proves, paired with the
/// issue that task's scope names.
///
/// Only a uniformly scoped task group carries one: a group with mixed scopes
/// or without a task key has no own task, so its closure stays configured.
fn own_task_scope(group: &DirectoryGroup) -> Option<(TaskScopeRef<'_>, u64)> {
    let key = group_scope(group)?;
    let task_id = key.task_id?;
    let issue = group
        .rows
        .iter()
        .find(|row| group.scoped_lease_ids.contains(&row.lease_id))
        .map(|row| row.issue)?;
    Some((
        TaskScopeRef {
            task_id,
            provider: &key.provider,
            project: &key.project,
            instance: key.instance.as_deref().unwrap_or_default(),
        },
        issue,
    ))
}

/// The prune pass. Candidate scanning is read-only; only [`PruneMode::Remove`]
/// may delete a directory, and only after a fresh re-read of rows and guards
/// through `fresh_api` (never the scan cache).
///
/// `scan_api` may be a memoizing view for the classification pass, but every
/// removal authorization re-reads through `fresh_api`, so a stale scan answer
/// can never authorize a deletion.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_pass(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    stale_days: u32,
    mode: PruneMode,
    progress: &dyn Fn(&str),
) -> PruneSummary {
    prune_pass_with_policy(
        storage,
        runner,
        scan_api,
        fresh_api,
        scope,
        pinned_active,
        source,
        stale_days,
        mode,
        MovePolicy::NoWait,
        None,
        false,
        progress,
    )
}

/// [`prune_pass`] with an explicit move-confirmation policy. The production
/// pass supplies one invocation-shared bounded budget in removal mode; every
/// test that does not care keeps the no-wait default.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_pass_with_policy(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    stale_days: u32,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
    progress: &dyn Fn(&str),
) -> PruneSummary {
    // The durable key of the physical repository this pass acts on, proven
    // from that repository's own checkout. It is derived once, outside every
    // write boundary, and every sidecar map read below is checked against
    // it: a task-bearing lease row that is not a member of this repository
    // is refused instead of being released or removed.
    let repo_key = crate::worktree::lease_scope::verified_repo_key_for_identity(scope.identity);
    // The scan is silent: the pass streams one global start line (from the
    // caller) and an at-most-one truthful "verifying … before removal" notice
    // per attempted deletion, never per-repository scan chatter.
    let scopes = match crate::worktree::lease_scope::scope_map_for_repo_with_key(
        storage,
        scope.identity,
        repo_key.as_deref(),
    ) {
        Ok(scopes) => scopes,
        Err(error) => {
            return scope_error_summary(
                scope,
                &format!("could not read lease scopes: {}", error.message),
            );
        }
    };
    let candidates = scan_scoped_with_paths(
        runner,
        scan_api,
        scope,
        pinned_active,
        source,
        &CanonicalPaths::default(),
        &scopes,
        provider_scope,
        explicit_forgejo,
    );
    let actions: Vec<PruneAction> = candidates
        .iter()
        .map(|candidate| {
            apply_candidate(
                storage,
                runner,
                fresh_api,
                scope,
                repo_key.as_deref(),
                pinned_active,
                source,
                candidate,
                mode,
                confirmation,
                provider_scope,
                explicit_forgejo,
                progress,
            )
        })
        .collect();
    summarize(actions, candidates.len(), mode, stale_days)
}

#[allow(clippy::too_many_arguments)]
fn apply_candidate(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    repo_key: Option<&str>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
    progress: &dyn Fn(&str),
) -> PruneAction {
    let mut action = PruneAction {
        worktree_path: candidate.worktree_path.clone(),
        branch: candidate.branch.clone(),
        status: candidate.status.clone(),
        heartbeat_at: candidate.heartbeat_at,
        lease_ids: candidate.lease_ids.clone(),
        issues: candidate.issues.clone(),
        prunable: false,
        result: "noop".to_owned(),
        message: None,
        relocated_children: Vec::new(),
        planned_children: Vec::new(),
    };
    match &candidate.disposition {
        PruneDisposition::SkippedActive => {
            action.result = "skipped_active".to_owned();
            action.message = Some("active lease references this directory".to_owned());
        }
        PruneDisposition::SkippedUnknownStatus => {
            action.result = "skipped_unknown".to_owned();
            action.message = Some("unrecognised lease status; kept safely".to_owned());
        }
        PruneDisposition::SkippedProtected(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::SkippedIssue(reason) => {
            action.result = "skipped_issue".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::MissingDirectory => {
            action.result = "absent".to_owned();
            action.message = Some(
                "worktree directory already absent; the historical lease path stays recorded"
                    .to_owned(),
            );
            if mode == PruneMode::Remove {
                // Converge only rows that are not already terminal. The
                // update is revalidated in one immediate transaction against
                // the scanned row identity/status/path, physical absence, the
                // complete sidecar map, and repository-wide same-number
                // membership; any changed or newly conflicting evidence
                // aborts all affected updates.
                let targets: Vec<(String, String, String)> = candidate
                    .lease_proof
                    .iter()
                    .filter(|(id, _, _)| {
                        !candidate
                            .released_lease_ids
                            .iter()
                            .any(|released| released == id)
                    })
                    .cloned()
                    .collect();
                if !targets.is_empty() {
                    match crate::worktree::leases::release_absent_leases_proven(
                        storage,
                        scope.identity,
                        repo_key,
                        &targets,
                        &candidate.release_proof,
                    ) {
                        Ok(flipped) => {
                            if !flipped.is_empty()
                                && let Err(reason) =
                                    reload_statuses(storage, &candidate.lease_ids, &mut action)
                            {
                                action.result = "failed".to_owned();
                                action.message =
                                    Some(format!("could not re-read lease status: {reason}"));
                            }
                        }
                        Err(error) => {
                            action.result = "skipped_protected".to_owned();
                            action.message =
                                Some(format!("kept before absent-path release: {error}"));
                        }
                    }
                }
            }
        }
        PruneDisposition::Removable => {
            if mode.is_report() {
                // The preview probes the same occupancy and lineage evidence
                // the removal stage would meet, so it reports the blockers
                // and the move eligibility it finds. It never moves a
                // session, deletes a directory, or writes a lease row.
                let base = match preview_family(api, scope, candidate) {
                    FamilyOutcome::Keep(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.message = Some(reason);
                        None
                    }
                    FamilyOutcome::Releasable { sessions } => Some(dry_run_message(sessions.len())),
                    _ => Some("dry-run: would call git worktree remove".to_owned()),
                };
                if let Some(base) = base {
                    action.prunable = true;
                    action.result = "prunable".to_owned();
                    action.message = Some(match preview_cache_detail(runner, candidate) {
                        Some(detail) => format!("{base}; {detail}"),
                        None => base,
                    });
                }
            } else {
                progress(&format!(
                    "verifying {} before removal",
                    candidate.worktree_path
                ));
                // Re-read rows and guards immediately before removal so a
                // running session, a new active lease, or a dirty state that
                // appeared after the scan keeps the directory.
                match recheck(
                    storage,
                    runner,
                    api,
                    scope,
                    repo_key,
                    pinned_active,
                    source,
                    candidate,
                    provider_scope,
                    explicit_forgejo,
                ) {
                    Err(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.message = Some(format!("kept on re-check: {reason}"));
                    }
                    Ok((associated, fresh_cache)) => {
                        if fresh_cache != candidate.cache {
                            action.result = "skipped_protected".to_owned();
                            action.message = Some(
                                "kept on re-check: the root .cache inventory changed before the moves"
                                    .to_owned(),
                            );
                        } else {
                            // The shared family assessment is the removal stage's
                            // session guard: it proves that every session hosted
                            // here belongs to this directory's lease family,
                            // returns the idle ones to the verified main
                            // checkout, and confirms the result with fresh
                            // evidence before anything is removed.
                            match sessions::finalize_family_with(
                                api,
                                CleanupMode::Prune,
                                scope.target,
                                &candidate.canonical_path,
                                &associated,
                                None,
                                confirmation,
                            ) {
                                FamilyOutcome::Keep(reason) => {
                                    action.result = "skipped_protected".to_owned();
                                    action.message = Some(format!("kept on re-check: {reason}"));
                                }
                                // The moves are not a permission: rows, local
                                // guards and closure evidence are re-read now,
                                // after any movement, because a new active owner
                                // or a newly linked open issue that appeared
                                // during the moves must keep the directory.
                                _ => match post_move_recheck(
                                    storage,
                                    runner,
                                    api,
                                    scope,
                                    repo_key,
                                    pinned_active,
                                    source,
                                    candidate,
                                    true,
                                    provider_scope,
                                    explicit_forgejo,
                                ) {
                                    Err(reason) => {
                                        action.result = "skipped_protected".to_owned();
                                        action.message =
                                            Some(format!("kept on re-check: {reason}"));
                                    }
                                    Ok(()) => apply_removal(
                                        storage,
                                        runner,
                                        api,
                                        scope,
                                        repo_key,
                                        pinned_active,
                                        source,
                                        candidate,
                                        provider_scope,
                                        explicit_forgejo,
                                        &mut action,
                                    ),
                                },
                            }
                        }
                    }
                }
            }
        }
    }
    action
}

/// The preview's read-only probe of the removal stage's evidence. It shares
/// the assessment with the removal stage, so the blockers it reports are the
/// blockers the removal would meet.
fn preview_family(
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    candidate: &DirectoryCandidate,
) -> FamilyOutcome {
    let associated = associated_sessions(scope.rows, &candidate.canonical_path);
    sessions::finalize_family(
        api,
        CleanupMode::Preview,
        scope.target,
        &candidate.canonical_path,
        &associated,
        None,
    )
}

/// The bounded preview message: the directory is removable, and this is how
/// many of the family's idle sessions the removal would return to the main
/// checkout first.
/// The preview's read-only cache detail: the exact root `.cache` the removal
/// would delete, with its bounded entry count. `None` when the worktree is
/// clean or cannot be inspected; the preview never mutates either way.
fn preview_cache_detail(
    runner: &dyn WorktreeRunner,
    candidate: &DirectoryCandidate,
) -> Option<String> {
    match crate::worktree::cache::inspect(runner, &candidate.canonical_path) {
        Ok(crate::worktree::cache::CacheInspection::CacheOnly(inventory)) => Some(format!(
            "would delete cache-only .cache ({} entries, no tracked files)",
            inventory.len()
        )),
        _ => None,
    }
}

fn dry_run_message(sessions: usize) -> String {
    match sessions {
        0 => "dry-run: would call git worktree remove".to_owned(),
        1 => "dry-run: would return 1 idle OpenCode session to the main checkout, then call git worktree remove".to_owned(),
        many => format!(
            "dry-run: would return {many} idle OpenCode sessions to the main checkout, then call git worktree remove"
        ),
    }
}

/// Re-evaluate one directory against fresh rows and the uncached running-session
/// answer. Returns the associated sessions of the directory when the fresh
/// proof still allows removal, or the keep reason when the directory must
/// survive.
#[allow(clippy::too_many_arguments)]
fn recheck(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    repo_key: Option<&str>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
) -> Result<(Vec<String>, Option<crate::worktree::cache::CacheInventory>), String> {
    let fresh = match list_all_for_repo(storage, scope.identity) {
        Ok(rows) => rows,
        Err(error) => return Err(format!("could not re-read leases: {}", error.message)),
    };
    let paths = CanonicalPaths::default();
    if paths.resolve(Path::new(&candidate.worktree_path)) != candidate.canonical_path {
        return Err("the directory path changed its target".to_owned());
    }
    {
        let fresh_scope = PruneScope {
            identity: scope.identity,
            repo_path: scope.repo_path,
            rows: &fresh,
            target: scope.target,
        };
        let mut fresh_group = group_rows(&fresh, &paths)
            .into_iter()
            .find(|group| group.canonical_path == candidate.canonical_path)
            .unwrap_or_else(|| DirectoryGroup {
                canonical_path: candidate.canonical_path.clone(),
                worktree_path: candidate.worktree_path.clone(),
                branch: candidate.branch.clone(),
                status: candidate.status.clone(),
                heartbeat_at: candidate.heartbeat_at,
                lease_ids: Vec::new(),
                issues: Vec::new(),
                rows: Vec::new(),
                scoped: false,
                scoped_lease_ids: Vec::new(),
                scoped_lease_keys: Vec::new(),
            });
        let fresh_scopes = crate::worktree::lease_scope::scope_map_for_repo_with_key(
            storage,
            scope.identity,
            repo_key,
        )
        .map_err(|error| format!("could not re-read lease scopes: {}", error.message))?;
        attach_scope_proof(&mut fresh_group, &fresh_scopes);
        ensure_group_scope(candidate.scoped, provider_scope, &fresh_group, source)?;
        source.invalidate(scope.repo_path, &fresh_group.issues);
        let mut associated: Vec<String> = fresh_group
            .rows
            .iter()
            .map(|row| row.session.trim().to_owned())
            .filter(|id| crate::worktree::opencode::is_opencode_session(id))
            .collect();
        associated.sort();
        associated.dedup();
        let classification = classify_group(
            runner,
            api,
            &fresh_scope,
            pinned_active,
            source,
            &fresh_group,
            &paths,
            &fresh_scopes,
            provider_scope,
            explicit_forgejo,
        );
        match classification.disposition {
            PruneDisposition::Removable => Ok((associated, classification.cache)),
            other => Err(recheck_reason(other)),
        }
    }
}

fn ensure_group_scope(
    was_scoped: bool,
    provider_scope: Option<(&str, &str)>,
    group: &DirectoryGroup,
    source: &dyn IssueStateSource,
) -> Result<(), String> {
    if was_scoped && !group.scoped {
        return Err("scoped provider lease scope evidence disappeared".to_owned());
    }
    if group.scoped && !scoped_group_matches(provider_scope, group) {
        return Err("scoped provider lease requires provider-aware cleanup".to_owned());
    }
    // The explicit selection filters a durable task group before any remote
    // read or ownership mutation: a task the selection does not match keeps
    // its directory instead of being redirected.
    if let Some(key) = group_scope(group)
        && key.task_id.is_some()
        && let Some(context) = source.selection()
    {
        let instance = key.instance.as_deref().unwrap_or_default();
        match crate::cli::task_route::selection_matches_scope(
            context,
            &key.provider,
            instance,
            &key.project,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(
                    "the explicit provider selection does not match the durable task".to_owned(),
                );
            }
            Err(error) => {
                return Err(format!(
                    "the explicit provider selection is invalid: {}",
                    error
                        .json()
                        .get("message")
                        .and_then(|value| value.as_str())
                        .unwrap_or("the selection could not be checked")
                ));
            }
        }
    }
    Ok(())
}

fn scope_error_summary(scope: &PruneScope<'_>, reason: &str) -> PruneSummary {
    let paths = CanonicalPaths::default();
    let actions: Vec<PruneAction> = group_rows(scope.rows, &paths)
        .into_iter()
        .map(|group| PruneAction {
            worktree_path: group.worktree_path,
            branch: group.branch,
            status: group.status,
            heartbeat_at: group.heartbeat_at,
            lease_ids: group.lease_ids,
            issues: group.issues,
            prunable: false,
            result: "skipped_protected".to_owned(),
            message: Some(reason.to_owned()),
            relocated_children: Vec::new(),
            planned_children: Vec::new(),
        })
        .collect();
    PruneSummary {
        scanned: actions.len(),
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_active: 0,
        kept_unknown: 0,
        kept_protected: actions.len(),
        kept_issue: 0,
        failed: 0,
        dry_run: false,
        stale_days: 0,
        actions,
    }
}

fn recheck_reason(disposition: PruneDisposition) -> String {
    match disposition {
        PruneDisposition::Removable => String::new(),
        PruneDisposition::SkippedActive => "an active lease re-appeared".to_owned(),
        PruneDisposition::SkippedUnknownStatus => "a lease status became unrecognised".to_owned(),
        PruneDisposition::SkippedProtected(reason) | PruneDisposition::SkippedIssue(reason) => {
            reason
        }
        PruneDisposition::MissingDirectory => "the directory vanished".to_owned(),
    }
}

/// The post-movement re-verification: after the family has been relocated,
/// the complete lease rows, the local guards, and the directory's issue
/// closures are re-read, so a new active owner or a newly linked open issue
/// that appeared during the (possibly slow) moves keeps the directory.
/// The pre-move recheck is not enough on its own.
#[allow(clippy::too_many_arguments)]
fn post_move_recheck(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    repo_key: Option<&str>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    compare_cache: bool,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
) -> Result<(), String> {
    let fresh = match list_all_for_repo(storage, scope.identity) {
        Ok(rows) => rows,
        Err(error) => return Err(format!("could not re-read leases: {}", error.message)),
    };
    let paths = CanonicalPaths::default();
    if paths.resolve(Path::new(&candidate.worktree_path)) != candidate.canonical_path {
        return Err("the directory path changed its target".to_owned());
    }
    let fresh_scope = PruneScope {
        identity: scope.identity,
        repo_path: scope.repo_path,
        rows: &fresh,
        target: scope.target,
    };
    let mut fresh_group = group_rows(&fresh, &paths)
        .into_iter()
        .find(|group| group.canonical_path == candidate.canonical_path)
        .ok_or_else(|| {
            format!(
                "the candidate's lease row is gone after the moves ({})",
                candidate.worktree_path
            )
        })?;
    let fresh_scopes = crate::worktree::lease_scope::scope_map_for_repo_with_key(
        storage,
        scope.identity,
        repo_key,
    )
    .map_err(|error| format!("could not re-read lease scopes: {}", error.message))?;
    attach_scope_proof(&mut fresh_group, &fresh_scopes);
    ensure_group_scope(candidate.scoped, provider_scope, &fresh_group, source)?;
    source.invalidate(scope.repo_path, &fresh_group.issues);
    // The classification re-reads the running-session answer, the closure
    // evidence and the local guards against the post-move rows; the host
    // vacancy itself was just proven and is not re-derived here.
    let classification = classify_group(
        runner,
        api,
        &fresh_scope,
        pinned_active,
        source,
        &fresh_group,
        &paths,
        &fresh_scopes,
        provider_scope,
        explicit_forgejo,
    );
    if compare_cache && classification.cache != candidate.cache {
        return Err("the root .cache inventory changed after the moves".to_owned());
    }
    match classification.disposition {
        PruneDisposition::Removable => {}
        other => return Err(recheck_reason(other)),
    }
    if !compare_cache {
        // The post-cache revalidation must see a fresh complete occupancy
        // listing, including an idle occupant no lease associates, and never
        // move a session. Any hosted, foreign, incomplete, or unreadable
        // answer keeps the directory.
        let associated = associated_sessions(&fresh, &candidate.canonical_path);
        if let FamilyOutcome::Keep(reason) = sessions::finalize_family(
            api,
            CleanupMode::Verify,
            scope.target,
            &candidate.canonical_path,
            &associated,
            None,
        ) {
            return Err(reason);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_removal(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    repo_key: Option<&str>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    provider_scope: Option<(&str, &str)>,
    explicit_forgejo: bool,
    action: &mut PruneAction,
) {
    let worktree_path = PathBuf::from(&candidate.worktree_path);
    // The shared cache-only clearance runs after every gate, immediately
    // before the no-force removal: it compares the initial baseline, deletes
    // only the exact root `.cache`, and re-runs Git status afterwards.
    let removed_cache = match crate::worktree::cache::clear_cache_only(
        runner,
        &worktree_path,
        candidate.cache.as_ref(),
    ) {
        Ok(count) => count,
        Err(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(format!("kept on re-check: {reason}"));
            return;
        }
    };
    // Clearing the cache can take a while, so the complete local, lease,
    // closure, and fresh host evidence is re-read once more before the
    // no-force removal. A new active owner, reopened issue, or occupant keeps
    // the directory; the cache is never rolled back.
    if let Err(reason) = post_move_recheck(
        storage,
        runner,
        api,
        scope,
        repo_key,
        pinned_active,
        source,
        candidate,
        false,
        provider_scope,
        explicit_forgejo,
    ) {
        action.result = "skipped_protected".to_owned();
        action.message = Some(format!("kept after cache cleanup: {reason}"));
        return;
    }
    match worktree_remove(runner, scope.repo_path, &worktree_path) {
        Ok(()) => {
            let mut flip_failed = false;
            for lease_id in &candidate.lease_ids {
                if let Err(error) = update_status(storage, lease_id, LEASE_STATUS_RELEASED) {
                    action.prunable = true;
                    action.result = "failed".to_owned();
                    action.message = Some(format!(
                        "worktree removed but lease flip failed: {}",
                        error.message
                    ));
                    flip_failed = true;
                    break;
                }
            }
            if !flip_failed {
                action.prunable = true;
                action.result = "pruned".to_owned();
                action.message = Some(match removed_cache {
                    Some(count) => format!(
                        "removed cache-only .cache ({count} entries); git worktree remove succeeded"
                    ),
                    None => "git worktree remove succeeded".to_owned(),
                });
            }
        }
        Err(error) => {
            report_local_warnings(
                "worktree prune",
                Some(format!(
                    "git worktree remove failed for {}: {}",
                    worktree_path.display(),
                    error.message
                )),
            );
            action.result = "failed".to_owned();
            action.message = Some(format!("worktree_remove error: {}", error.message));
        }
    }
}

fn reload_statuses(
    storage: &Storage,
    lease_ids: &[String],
    action: &mut PruneAction,
) -> Result<(), String> {
    for lease_id in lease_ids {
        match load_lease(storage, lease_id) {
            Ok(Some(updated)) => action.status = updated.status,
            Ok(None) => {
                return Err(format!("lease row '{lease_id}' disappeared"));
            }
            Err(error) => return Err(error.message),
        }
    }
    Ok(())
}

fn summarize(
    actions: Vec<PruneAction>,
    scanned: usize,
    mode: PruneMode,
    stale_days: u32,
) -> PruneSummary {
    let mut summary = PruneSummary {
        scanned,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_active: 0,
        kept_unknown: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
        dry_run: mode.is_report(),
        stale_days,
        actions: Vec::with_capacity(actions.len()),
    };
    for action in actions {
        match action.result.as_str() {
            "pruned" => summary.removed += 1,
            "prunable" => summary.eligible += 1,
            "absent" => summary.absent += 1,
            "skipped_active" => summary.kept_active += 1,
            "skipped_unknown" => summary.kept_unknown += 1,
            "skipped_protected" => summary.kept_protected += 1,
            "skipped_issue" => summary.kept_issue += 1,
            "failed" => summary.failed += 1,
            _ => {}
        }
        summary.actions.push(action);
    }
    summary
}

/// The set of canonical directory paths that held an `active` lease at
/// invocation start. A stale recovery later in the same call must never make
/// one of these removable.
pub(crate) fn pinned_active_paths(rows: &[LeaseRow]) -> BTreeSet<String> {
    rows.iter()
        .filter(|row| row.status == LEASE_STATUS_ACTIVE)
        .map(|row| {
            canonical_or_self(Path::new(&row.worktree_path))
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[cfg(test)]
mod path_reuse_tests {
    use super::*;
    use crate::cli::issue_closure::test_support::ScriptedIssueStateSource;
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;
    use crate::worktree::{GitOutput, WorktreeError};

    #[test]
    fn scan_grouping_and_guards_share_one_resolution_per_path() {
        struct Runner;
        impl WorktreeRunner for Runner {
            fn run(&self, args: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
                Ok(GitOutput {
                    status: 0,
                    stdout: match args {
                        ["rev-parse", "--git-dir"] => "/fixture/.git/worktrees/linked".to_owned(),
                        ["status", "--porcelain"] => String::new(),
                        _ => panic!("unexpected Git call: {args:?}"),
                    },
                })
            }
        }
        let root = crate::test_scratch::root().join(format!(
            "scan-paths-{}-{}",
            std::process::id(),
            crate::worktree::now_unix_secs()
        ));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        for name in ["one", "two"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let rows: Vec<LeaseRow> = (0..20)
            .map(|n| LeaseRow {
                lease_id: format!("lease-{n}"),
                repo_identity: "/fixture/.git".to_owned(),
                issue: 7,
                session: "cli".to_owned(),
                checkout_path: root.display().to_string(),
                worktree_path: root
                    .join(if n % 2 == 0 { "one" } else { "two" })
                    .display()
                    .to_string(),
                branch: "branch".to_owned(),
                status: LEASE_STATUS_RETAINED.to_owned(),
                created_at: 1,
                heartbeat_at: 1,
                release_reason: None,
            })
            .collect();
        let target = TargetResolver::new(&root, "/fixture/.git");
        let scope = PruneScope {
            identity: "/fixture/.git",
            repo_path: &root,
            rows: &rows,
            target: &target,
        };
        let paths = CanonicalPaths::default();
        let api = FakeOpenCodeApi::empty();
        let candidates = scan_with_paths(
            &Runner,
            &api,
            &scope,
            &BTreeSet::new(),
            &ScriptedIssueStateSource::closed(),
            &paths,
        );
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.disposition == PruneDisposition::Removable)
        );
        assert_eq!(
            paths.resolutions(),
            4,
            "20 lease rows plus shared main-checkout comparisons resolve only 2 directories, 1 git-dir and 1 identity"
        );
        assert_eq!(api.list_calls(), 0);
        assert!(api.moves().is_empty());
    }
}

#[cfg(test)]
mod scope_proof_tests {
    use super::*;

    fn group(lease_ids: &[&str], scoped_ids: &[&str]) -> DirectoryGroup {
        DirectoryGroup {
            canonical_path: PathBuf::from("/tmp/shared"),
            worktree_path: "/tmp/shared".to_owned(),
            branch: "phasegent/shared".to_owned(),
            status: "retained".to_owned(),
            heartbeat_at: 0,
            lease_ids: lease_ids.iter().map(|id| (*id).to_owned()).collect(),
            issues: vec![7, 8],
            rows: Vec::new(),
            scoped: !scoped_ids.is_empty(),
            scoped_lease_ids: scoped_ids.iter().map(|id| (*id).to_owned()).collect(),
            scoped_lease_keys: scoped_ids
                .iter()
                .map(|_| ScopedLeaseKey {
                    provider: "forgejo".to_owned(),
                    project: "owner/repo".to_owned(),
                    task_id: None,
                    instance: None,
                })
                .collect(),
        }
    }

    #[test]
    fn mixed_scope_proof_keeps() {
        let source = crate::cli::issue_closure::test_support::ScriptedIssueStateSource::closed();
        let mixed = group(&["legacy-7", "scoped-8"], &["scoped-8"]);
        assert!(
            ensure_group_scope(true, Some(("forgejo", "owner/repo")), &mixed, &source).is_err(),
            "a mixed legacy/scoped directory must keep"
        );
        let matching = group(&["scoped-7", "scoped-8"], &["scoped-7", "scoped-8"]);
        assert!(
            ensure_group_scope(true, Some(("forgejo", "owner/repo")), &matching, &source).is_ok()
        );
        let missing = group(&["legacy-7"], &[]);
        assert!(
            ensure_group_scope(true, Some(("forgejo", "owner/repo")), &missing, &source).is_err(),
            "a disappeared scope proof must keep"
        );
    }

    #[test]
    fn a_task_group_keeps_when_the_explicit_selection_does_not_match_it() {
        use crate::cli::issue_closure::test_support::ScriptedIssueStateSource;
        let mut task_group = group(&["task-7"], &["task-7"]);
        task_group.scoped_lease_keys = vec![ScopedLeaseKey {
            provider: "forgejo".to_owned(),
            project: "owner/repo".to_owned(),
            task_id: Some(7),
            instance: Some("https://forgejo.test/".to_owned()),
        }];
        let matching = ScriptedIssueStateSource::closed();
        assert!(
            ensure_group_scope(true, None, &task_group, &matching).is_ok(),
            "a durable task authorizes its own routing"
        );
        let foreign = crate::cli::provider_context::ProviderContext::from_parts(
            Some(crate::providers::ProviderKind::Forgejo),
            Some("https://other.test/"),
            Some("owner/repo"),
            None,
            None,
        );
        let source = crate::cli::issue_closure::test_support::ScriptedIssueStateSource::closed();
        assert!(
            ensure_group_scope(true, None, &task_group, &source).is_ok(),
            "an unrelated selection never filters a group it does not name"
        );
        let _ = foreign;
    }
}

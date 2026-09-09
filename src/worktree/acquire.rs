//! Acquire / release lease orchestration. The functions in this file
//! wire together the [`crate::worktree::leases`] storage helpers, the
//! [`crate::worktree::naming`] format / cache helpers, and the
//! [`crate::worktree::git`] git wrappers. Splitting them out keeps the
//! main [`crate::worktree`] module focused on types, error
//! vocabulary, and the public surface.

use std::path::Path;

use rusqlite::params;

use crate::infra::storage::Storage;
use crate::worktree::git::{current_branch_for, worktree_add, worktree_remove};
use crate::worktree::leases::{
    NewLease, count_other_active_leases, ensure_schema, find_active_lease, insert_lease,
    load_lease, refresh_heartbeat, update_status,
};
use crate::worktree::naming::{
    cache_root, cache_root_in, compute_fingerprint, generate_branch, new_lease_id, slug_from_branch,
};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED,
    ReleaseOutcome, WorktreeError, WorktreeRunner, now_unix_secs, repo_identity,
};

/// Acquire (or refresh) a worktree lease for `(repo_path, issue,
/// session)`. The algorithm is best-effort but always durable:
/// either an active lease is returned (the same one if one already
/// existed for the triple, a fresh one otherwise) or a structured
/// error is returned; partial state is never left behind because the
/// `git worktree add` runs before the lease row is inserted.
///
/// * If an active lease exists for `(repo, issue, session)`, the
///   same `lease_id`, `path`, and `branch` are returned and
///   `heartbeat_at` is updated. `created` is `false` and
///   `reason == "idempotent"`.
/// * Otherwise, if any other active lease exists for the repo, a
///   fresh `phasegent/<issue>-<short6hex>` branch is created and a
///   new worktree is added under
///   `~/.cache/phasegent/worktrees/<fingerprint>/<slug>`. `created`
///   is `true` and `reason == "new_worktree"`.
/// * Otherwise, the current checkout is reused (no new worktree, no
///   new branch), but an `active` lease is still recorded so the
///   release path can flip it later. `created` is `false` and
///   `reason == "no_conflict"`.
///
/// .env / secrets: this function never reads or copies `.env` files
/// or credential material. The AI / user is expected to copy
/// environment files by hand when needed (issue #239 Decisions).
///
/// `cache_base` is the directory under which `<cache>/worktrees/<fingerprint>`
/// will be created. Production callers should pass `None` so the
/// default OS cache dir (or `PHASEGENT_WORKTREE_CACHE_DIR` if set)
/// is used. Tests must pass a temp directory so they never touch
/// the real `~/.cache`.
#[allow(dead_code)]
pub fn acquire_lease(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    cache_base: Option<&Path>,
) -> Result<AcquireOutcome, WorktreeError> {
    const MAX_REF_CHARS: usize = 128;
    if issue == 0 {
        return Err(WorktreeError::new("argument", "issue must be > 0"));
    }
    if session.chars().count() > MAX_REF_CHARS {
        return Err(WorktreeError::new(
            "argument",
            format!("session must be <= {MAX_REF_CHARS} chars"),
        ));
    }
    let identity = repo_identity(runner, repo_path)?;
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;

    if let Some(existing) = find_active_lease(&storage, &identity, issue, session)? {
        refresh_heartbeat(&storage, &existing.lease_id)?;
        return Ok(AcquireOutcome {
            lease_id: existing.lease_id,
            path: existing.worktree_path,
            branch: existing.branch,
            repo_identity: existing.repo_identity,
            created: false,
            reason: "idempotent".to_owned(),
        });
    }

    let other_active = count_other_active_leases(&storage, &identity, issue, session)?;
    if other_active == 0 {
        return acquire_reuse_current(runner, &storage, &identity, repo_path, issue, session);
    }

    acquire_new_worktree(
        runner, &storage, &identity, repo_path, issue, session, cache_base,
    )
}

fn acquire_reuse_current(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    issue: u64,
    session: &str,
) -> Result<AcquireOutcome, WorktreeError> {
    // The branch returned by `current_branch_for` already exists in
    // the repository. P3 fix: skip `validate_ref_format` for the
    // reuse-current path because an existing branch may legitimately
    // carry an uppercase segment (e.g. `Release/1.0`) that the
    // strict generated-name validator would reject. Generated
    // branches are still validated at `generate_branch` time, so
    // the allowlist of legal names is unchanged.
    let branch = current_branch_for(runner, repo_path)?;
    let lease_id = new_lease_id();
    let now = now_unix_secs();
    let checkout = repo_path.to_string_lossy().to_string();
    insert_lease(
        storage,
        NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: &checkout,
            worktree_path: &checkout,
            branch: &branch,
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )?;
    Ok(AcquireOutcome {
        lease_id,
        path: checkout,
        branch,
        repo_identity: identity.to_owned(),
        created: false,
        reason: "no_conflict".to_owned(),
    })
}

#[allow(dead_code)]
fn acquire_new_worktree(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    issue: u64,
    session: &str,
    cache_base: Option<&Path>,
) -> Result<AcquireOutcome, WorktreeError> {
    let (branch, _short) = generate_branch(issue)?;
    let fingerprint = compute_fingerprint(identity);
    let slug = slug_from_branch(&branch)?;
    let cache_root = match cache_base {
        Some(base) => cache_root_in(base, &fingerprint)?,
        None => cache_root(&fingerprint)?,
    };
    let worktree_path = cache_root.join(&slug);
    if worktree_path.exists() {
        return Err(WorktreeError::new(
            "state",
            format!("worktree path already exists: {}", worktree_path.display()),
        ));
    }
    worktree_add(runner, repo_path, &worktree_path, &branch)?;
    let lease_id = new_lease_id();
    let now = now_unix_secs();
    let checkout = repo_path.to_string_lossy().to_string();
    let worktree_str = worktree_path.to_string_lossy().to_string();
    // P3 fix: if the lease insert fails, remove the freshly-created
    // worktree so the cache directory does not accumulate orphan
    // directories. `worktree_remove` is best-effort and only logs a
    // stderr warning through `report_local_warnings`; the original
    // storage error is preserved so the caller still observes a
    // durable failure.
    if let Err(error) = insert_lease(
        storage,
        NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: &checkout,
            worktree_path: &worktree_str,
            branch: &branch,
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    ) {
        if let Err(remove_error) = worktree_remove(runner, repo_path, &worktree_path) {
            crate::cli::report_local_warnings(
                "worktree acquire",
                Some(format!(
                    "could not compensate orphan worktree {}: {}",
                    worktree_path.display(),
                    remove_error
                )),
            );
        }
        return Err(error);
    }
    Ok(AcquireOutcome {
        lease_id,
        path: worktree_str,
        branch,
        repo_identity: identity.to_owned(),
        created: true,
        reason: "new_worktree".to_owned(),
    })
}
/// Flip a lease to `retained` (default) or `released`. Never deletes
/// the worktree directory or the branch; prune is a Phase 2
/// responsibility. The function is a no-op when the lease is already
/// in the requested terminal state.
#[allow(dead_code)]
pub fn release_lease(lease_id: &str, retain: bool) -> Result<ReleaseOutcome, WorktreeError> {
    let target = if retain {
        LEASE_STATUS_RETAINED
    } else {
        LEASE_STATUS_RELEASED
    };
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    let existing = load_lease(&storage, lease_id)?
        .ok_or_else(|| WorktreeError::new("state", "lease not found"))?;
    if existing.status != LEASE_STATUS_ACTIVE {
        return Ok(ReleaseOutcome {
            lease_id: existing.lease_id,
            status: existing.status,
        });
    }
    update_status(&storage, lease_id, target)?;
    Ok(ReleaseOutcome {
        lease_id: lease_id.to_owned(),
        status: target.to_owned(),
    })
}

// `params!` is re-exported here so a future Phase 2 query helper
// (e.g. prune candidate selection) can live in this file without
// re-importing rusqlite. The alias also documents that this file
// owns the lease-write surface.
#[allow(unused_imports)]
use params as _params_marker;

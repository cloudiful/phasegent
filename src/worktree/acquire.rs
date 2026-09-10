//! Acquire / release lease orchestration. The functions in this file
//! wire together the [`crate::worktree::leases`] storage helpers, the
//! [`crate::worktree::naming`] format / cache helpers, and the
//! [`crate::worktree::git`] git wrappers. Splitting them out keeps the
//! main [`crate::worktree`] module focused on types, error
//! vocabulary, and the public surface.

use std::path::Path;

use rusqlite::params;

use crate::branch_context::{ProcessGitRunner, read_issue_id};
use crate::infra::storage::Storage;
use crate::worktree::git::{current_branch_for, is_clean, worktree_add, worktree_remove};
use crate::worktree::leases::{
    NewLease, count_other_active_leases, ensure_schema, find_active_lease, insert_lease,
    list_for_repo, load_lease, refresh_heartbeat, update_status,
};
use crate::worktree::naming::{
    cache_root, cache_root_in, compute_fingerprint, generate_branch, new_lease_id, slug_from_branch,
};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED,
    ReleaseOutcome, WorktreeError, WorktreeRunner, now_unix_secs, repo_identity,
};

/// Canonical global-setting / environment name for the worktree
/// auto-isolation switch (issue #247).
pub const WORKTREE_AUTO_SETTING: &str = "PHASEGENT_WORKTREE_AUTO";

/// Resolve the effective `worktree-auto` switch.
///
/// Precedence is `PHASEGENT_WORKTREE_AUTO` (environment) → TOML overlay
/// (which has no `worktree_auto` field, so this layer is a no-op today)
/// → SQLite `global_setting` → built-in default `false`. The
/// env-over-SQLite chain matches every other non-secret global setting
/// (`crate::notifications::config::resolve_field`); an invalid literal
/// is a structured `config` error rather than a silent `false`.
pub fn resolve_worktree_auto(storage: &Storage) -> Result<bool, WorktreeError> {
    let raw = match std::env::var(WORKTREE_AUTO_SETTING) {
        Ok(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(WorktreeError::new(
                "config",
                format!("could not read {WORKTREE_AUTO_SETTING}: {error}"),
            ));
        }
    };
    let raw = match raw {
        Some(raw) => Some(raw),
        None => storage
            .load_global_setting(WORKTREE_AUTO_SETTING)
            .map_err(|error| WorktreeError::new("storage", error))?,
    };
    match raw {
        None => Ok(false),
        Some(raw) => crate::config_write::parse_bool_literal(&raw).ok_or_else(|| {
            WorktreeError::new(
                "config",
                format!("invalid {WORKTREE_AUTO_SETTING} '{raw}'; expected true or false"),
            )
        }),
    }
}

/// Acquire (or refresh) a worktree lease for `(repo_path, issue,
/// session)`. The algorithm is best-effort but always durable:
/// either an active lease is returned (the same one if one already
/// existed for the triple, a fresh one otherwise) or a structured
/// error is returned; partial state is never left behind because the
/// `git worktree add` runs before the lease row is inserted.
///
/// The decision table (issue #246, gated by issue #247) is evaluated in
/// order. Rules 2, 3, and 4 only create a new worktree when isolation is
/// enabled (`isolate || auto`, i.e. `--isolate` or `worktree-auto` true);
/// when it is disabled they reuse the current checkout with
/// `reason == "no_conflict"` and a conflict warning naming the trigger,
/// while still recording the lease for bookkeeping. The default is
/// disabled, so acquire never implicitly creates a branch or directory.
///
/// 1. An active lease exists for `(repo, issue, session)` — the same
///    `lease_id`, `path`, and `branch` are returned and `heartbeat_at`
///    is updated. `created` is `false` and `reason == "idempotent"`.
/// 2. The checkout is dirty (`is_clean` probe on `repo_path`) and its
///    branch is bound to a *different* issue — when isolation is enabled
///    a fresh `phasegent/<issue>-<short6hex>` branch and worktree are
///    created so the new task never lands in a dirty tree that belongs
///    to another task. `created` is `true`, `reason == "new_worktree"`.
/// 3. The checkout is dirty and bound to *this* issue, but another
///    active session already holds a lease for `(repo, issue)` — when
///    isolation is enabled a new worktree is created because parallel
///    sessions must not share a dirty tree. If no such lease exists (the
///    dirty tree is likely a crashed predecessor's own work) the
///    decision falls through.
/// 4. Any other active lease exists for the repo — when isolation is
///    enabled a fresh `phasegent/<issue>-<short6hex>` branch is created
///    and a new worktree is added under
///    `~/.cache/phasegent/worktrees/<fingerprint>/<slug>`. `created`
///    is `true` and `reason == "new_worktree"`.
/// 5. Otherwise the current checkout is reused (no new worktree, no
///    new branch), but an `active` lease is still recorded so the
///    release path can flip it later. `created` is `false` and
///    `reason == "no_conflict"`. When that checkout is dirty and
///    carries no branch binding, a best-effort warning is recorded in
///    `AcquireOutcome::warnings` and emitted on stderr.
///
/// The binding read mirrors `hooks.rs` `bound_issue_id`: a detached
/// HEAD or any lookup failure is treated as *unbound* and never
/// errors the acquire — it falls through with a warning instead.
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
    isolate: bool,
    auto: bool,
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
    // Effective creation gate (issue #247): isolation happens only when
    // the per-call `--isolate` flag or the resolved `worktree-auto`
    // switch asks for it. The default is off, so a conflict reuses the
    // current checkout and warns instead of silently creating a branch
    // and directory.
    let creation_allowed = isolate || auto;
    let identity = repo_identity(runner, repo_path)?;
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;

    // Rule 1: idempotent home-coming for the same triple. The dirty
    // probe is deliberately skipped here so a re-entering session is
    // never misjudged against its own tree.
    if let Some(existing) = find_active_lease(&storage, &identity, issue, session)? {
        refresh_heartbeat(&storage, &existing.lease_id)?;
        return Ok(AcquireOutcome {
            lease_id: existing.lease_id,
            path: existing.worktree_path,
            branch: existing.branch,
            repo_identity: existing.repo_identity,
            created: false,
            reason: "idempotent".to_owned(),
            warnings: Vec::new(),
        });
    }

    let mut warnings: Vec<String> = Vec::new();

    // Dirty probe. A probe failure is best-effort: it is surfaced as a
    // warning and the decision falls back to lease evidence only
    // (dirty is treated as unknown) so acquire never hard-errors on a
    // git status hiccup.
    let clean = match is_clean(runner, repo_path) {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!(
                "dirty probe failed ({}); falling back to lease-based decisions",
                error.message
            ));
            true
        }
    };
    let dirty = !clean;

    // The branch binding is only resolved when the checkout is dirty:
    // a clean tree cannot be "contaminated", so rules 2/3 never apply
    // and the extra git calls are skipped.
    let (bound, binding_failure) = if dirty {
        resolve_bound_issue(runner, repo_path)
    } else {
        (None, None)
    };

    // Rules 2 and 3: dirty-tree triggers. Each trigger creates an
    // isolated worktree only when isolation is enabled; otherwise it
    // records a conflict warning and falls through to the reuse path so
    // the caller stays in the current checkout (default-off behaviour).
    if dirty {
        match bound {
            Some(bound_issue) if bound_issue != issue => {
                if creation_allowed {
                    warnings.push(format!(
                        "checkout is dirty and bound to issue {bound_issue}; \
                         acquiring issue {issue} in an isolated worktree"
                    ));
                    return with_warnings(
                        acquire_new_worktree(
                            runner, &storage, &identity, repo_path, issue, session, cache_base,
                        ),
                        warnings,
                    );
                }
                warnings.push(format!(
                    "checkout is dirty and bound to issue {bound_issue}; worktree \
                     auto-isolation is disabled (no --isolate, worktree-auto=false); \
                     reusing the current checkout"
                ));
            }
            Some(_) => {
                if active_lease_for_issue_other_session(&storage, &identity, issue, session)? {
                    if creation_allowed {
                        warnings.push(format!(
                            "checkout is dirty and another active session holds a lease for issue \
                             {issue}; parallel sessions must not share a dirty tree"
                        ));
                        return with_warnings(
                            acquire_new_worktree(
                                runner, &storage, &identity, repo_path, issue, session, cache_base,
                            ),
                            warnings,
                        );
                    }
                    warnings.push(format!(
                        "checkout is dirty and another active session holds a lease for issue \
                         {issue}; worktree auto-isolation is disabled (no --isolate, \
                         worktree-auto=false); reusing the current checkout"
                    ));
                }
            }
            None => {
                if let Some(message) = binding_failure.as_ref() {
                    warnings.push(message.clone());
                }
            }
        }
    }

    // Rule 4: any other active lease for the repo would create a new
    // worktree when isolation is enabled. With isolation disabled the
    // current checkout is reused and the trigger is named in a warning.
    let other_active = count_other_active_leases(&storage, &identity, issue, session)?;
    if other_active > 0 {
        if creation_allowed {
            return with_warnings(
                acquire_new_worktree(
                    runner, &storage, &identity, repo_path, issue, session, cache_base,
                ),
                warnings,
            );
        }
        warnings.push(
            "another active lease exists for this repository; worktree auto-isolation is \
             disabled (no --isolate, worktree-auto=false); reusing the current checkout"
                .to_owned(),
        );
    }

    // Rule 5: reuse the current checkout. A dirty checkout that is not
    // bound to any issue gets a warning (there is no task evidence for
    // the dirt, so we do not create a worktree on suspicion, but the
    // operator should know the reused tree is not pristine).
    if dirty && bound.is_none() && binding_failure.is_none() {
        warnings.push(
            "checkout is dirty and not bound to an issue; reusing it (no task evidence of a \
             conflict)"
                .to_owned(),
        );
    }
    with_warnings(
        acquire_reuse_current(runner, &storage, &identity, repo_path, issue, session),
        warnings,
    )
}

/// Resolve the issue the current branch is bound to, following the
/// `hooks.rs` `bound_issue_id` pattern but never erroring: a detached
/// HEAD or any config lookup failure is treated as *unbound* and
/// returned as a warning string so the caller falls through the
/// decision table instead of failing the acquire.
fn resolve_bound_issue(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
) -> (Option<u64>, Option<String>) {
    let branch = match current_branch_for(runner, repo_path) {
        Ok(branch) => branch,
        Err(error) => {
            return (
                None,
                Some(format!(
                    "could not resolve current branch ({}); treating checkout as unbound",
                    error.message
                )),
            );
        }
    };
    let git_runner = ProcessGitRunner::in_directory(repo_path.to_path_buf());
    match read_issue_id(&git_runner, &branch) {
        Ok(bound) => (bound, None),
        Err(error) => (
            None,
            Some(format!(
                "could not read binding for branch '{branch}' ({}); treating checkout as unbound",
                error.message
            )),
        ),
    }
}

/// True when an *active* lease exists for `(repo, issue)` under a
/// session different from ours. Only active rows count: a released or
/// retained predecessor is not a live parallel session, so the caller
/// falls through to reuse its own tree.
fn active_lease_for_issue_other_session(
    storage: &Storage,
    identity: &str,
    issue: u64,
    session: &str,
) -> Result<bool, WorktreeError> {
    Ok(list_for_repo(storage, identity)?.iter().any(|row| {
        row.status == LEASE_STATUS_ACTIVE && row.issue == issue && row.session != session
    }))
}

/// Attach best-effort warnings to a successful outcome and forward each
/// one to stderr so stdout (and the CLI JSON envelope) stays clean for
/// consumers that key on `created`. Errors pass through untouched.
fn with_warnings(
    result: Result<AcquireOutcome, WorktreeError>,
    warnings: Vec<String>,
) -> Result<AcquireOutcome, WorktreeError> {
    let outcome = result?;
    for warning in &warnings {
        crate::cli::report_local_warnings("worktree acquire", Some(warning.clone()));
    }
    Ok(AcquireOutcome {
        warnings,
        ..outcome
    })
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
        warnings: Vec::new(),
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
        warnings: Vec::new(),
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

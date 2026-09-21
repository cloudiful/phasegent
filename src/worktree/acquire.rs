//! Acquire / release lease orchestration. The functions in this file
//! wire together the [`crate::worktree::leases`] storage helpers, the
//! [`crate::worktree::naming`] format / cache helpers, and the
//! [`crate::worktree::git`] git wrappers. Splitting them out keeps the
//! main [`crate::worktree`] module focused on types, error
//! vocabulary, and the public surface.

use std::path::{Path, PathBuf};

use crate::branch_context::{ProcessGitRunner, read_issue_id};
use crate::infra::storage::Storage;
use crate::worktree::git::{current_branch_for, is_clean, worktree_add, worktree_remove};
use crate::worktree::leases::{
    NewLease, count_other_active_leases, ensure_schema, find_active_lease, heartbeat_active_lease,
    insert_lease, list_for_repo, load_lease, record_release_reason, refresh_heartbeat,
    retain_active_leases_for_issue, update_status,
};
use crate::worktree::naming::{
    cache_root, cache_root_in, compute_fingerprint, generate_branch, new_lease_id, slug_from_branch,
};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    MAX_SESSION_CHARS, ReleaseOutcome, WorktreeError, WorktreeRunner, now_unix_secs, repo_identity,
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
/// The decision table (issue #246, default flipped by issue #436) is
/// evaluated in order. Rules 2, 3, and 4 create a new worktree **by
/// default**: a dirty checkout or any other active lease for the repo
/// means the current checkout must not be reused, so the incoming task
/// gets an isolated `phasegent/<issue>-<short6hex>` branch and a
/// worktree under
/// `~/.cache/phasegent/worktrees/<fingerprint>/<slug>` instead of
/// colliding with a foreign tree or the `(repo, worktree_path)` lease
/// index. `--isolate` forces a fresh branch/worktree (issue #509):
/// `isolate || auto` (`--isolate` or the resolved `worktree-auto`
/// switch) skips every reuse path and acquires an isolated worktree
/// directly; there is deliberately no reuse fallback for a confirmed
/// conflict trigger or an explicit isolation request.
///
/// The dirty probe feeding rules 2/3/5 is a three-state value
/// (`Clean` / `Dirty` / `Unknown`, issue 305 Task 4). A failed
/// `git status` yields `Unknown` and never counts as `Clean`: with
/// `--isolate` or `worktree-auto` enabled a fresh worktree is created,
/// and otherwise the current checkout is reused with an explicit
/// warning. Explicit isolation additionally covers the `Clean` path,
/// so the switches force a fresh worktree on every checkout state.
///
/// 1. An active lease exists for `(repo, issue, session)` — the same
///    `lease_id`, `path`, and `branch` are returned and `heartbeat_at`
///    is updated. `created` is `false` and `reason == "idempotent"`.
///    This home-coming is checked before the explicit `--isolate` /
///    `worktree-auto` gate below, so it is unaffected by isolation.
/// 2. The checkout is dirty (`is_clean` probe on `repo_path`) and its
///    branch is bound to a *different* issue — a fresh worktree is
///    created so the new task never lands in a dirty tree that belongs
///    to another task. `created` is `true`, `reason == "new_worktree"`.
/// 3. The checkout is dirty and bound to *this* issue, but another
///    active session already holds a lease for `(repo, issue)` — a new
///    worktree is created because parallel sessions must not share a
///    dirty tree. If no such lease exists (the dirty tree is likely a
///    crashed predecessor's own work) the decision falls through.
/// 4. Any other active lease exists for the repo — a fresh worktree is
///    created. `created` is `true` and `reason == "new_worktree"`.
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
/// On every successful outcome (including the idempotent and reuse
/// paths) [`finalize_checkout`] best-effort binds `issue` to the
/// acquired checkout's branch and installs the managed commit hooks,
/// so one acquire command leaves the operator ready to work. Both
/// steps reuse the canonical helpers unchanged and degrade to
/// warnings: the lease is already durable, so a local binding or hook
/// failure must never turn a successful acquire into an error.
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
    // Explicit isolation request (issues #247, #509): `--isolate` or the
    // resolved `worktree-auto` switch forces a fresh branch/worktree on
    // every checkout state. Rules 2, 3, and 4 already isolate by default
    // (issue #436); this gate additionally skips every reuse path below.
    let explicit_isolation = isolate || auto;
    let identity = repo_identity(runner, repo_path)?;
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;

    // Rule 1: idempotent home-coming for the same triple. The dirty
    // probe is deliberately skipped here so a re-entering session is
    // never misjudged against its own tree. The post-acquire lifecycle
    // still runs so a re-entering session converges on a bound branch
    // and installed hooks.
    if let Some(existing) = find_active_lease(&storage, &identity, issue, session)? {
        refresh_heartbeat(&storage, &existing.lease_id)?;
        return with_warnings(
            Ok(AcquireOutcome {
                lease_id: existing.lease_id,
                path: existing.worktree_path,
                branch: existing.branch,
                repo_identity: existing.repo_identity,
                created: false,
                reason: "idempotent".to_owned(),
                warnings: Vec::new(),
            }),
            issue,
            Vec::new(),
        );
    }

    let mut warnings: Vec<String> = Vec::new();

    // Dirty probe. The result is a three-state value so a `git status`
    // failure is `Unknown` instead of being silently folded into
    // `clean` (issue 305 Task 4). A probe failure is still best-effort:
    // it never hard-errors the acquire.
    let (dirty_state, probe_failure) = probe_dirty_state(runner, repo_path);
    let dirty = dirty_state == DirtyState::Dirty;

    // Unknown checkout state (`git status` failed): the tree may hold
    // work we cannot see, so it must not be reused silently. With
    // `--isolate`/`worktree-auto` a fresh worktree is created; otherwise
    // the current checkout is reused with an explicit warning.
    if dirty_state == DirtyState::Unknown {
        let detail = probe_failure.unwrap_or_else(|| "git status failed".to_owned());
        if explicit_isolation {
            warnings.push(format!(
                "{detail}; worktree auto-isolation is enabled, creating an isolated worktree \
                 because the checkout state is unknown"
            ));
            return with_warnings(
                acquire_new_worktree(
                    runner, &storage, &identity, repo_path, issue, session, cache_base,
                ),
                issue,
                warnings,
            );
        }
        warnings.push(format!(
            "{detail}; worktree auto-isolation is disabled (no --isolate, worktree-auto=false); \
             reusing the current checkout despite the unknown dirty state"
        ));
    }

    // The branch binding is only resolved for a confirmed dirty
    // checkout: a clean tree cannot be "contaminated", and an unknown
    // probe must not infer a binding from an untrusted status result.
    let (bound, binding_failure) = if dirty {
        resolve_bound_issue(runner, repo_path)
    } else {
        (None, None)
    };

    // Rules 2 and 3: dirty-tree triggers. Both isolate by default
    // (issue #436): reusing a dirty tree would mix two tasks' work, so
    // the incoming task gets its own worktree instead. `--isolate`
    // remains accepted as the explicit opt-in for the same outcome.
    if dirty {
        match bound {
            Some(bound_issue) if bound_issue != issue => {
                warnings.push(format!(
                    "checkout is dirty and bound to issue {bound_issue}; acquiring issue \
                     {issue} in an isolated worktree ({AUTO_ISOLATION_DEFAULT_NOTE})"
                ));
                return with_warnings(
                    acquire_new_worktree(
                        runner, &storage, &identity, repo_path, issue, session, cache_base,
                    ),
                    issue,
                    warnings,
                );
            }
            Some(_) => {
                if active_lease_for_issue_other_session(&storage, &identity, issue, session)? {
                    warnings.push(format!(
                        "checkout is dirty and another active session holds a lease for issue \
                         {issue}; acquiring issue {issue} in an isolated worktree \
                         ({AUTO_ISOLATION_DEFAULT_NOTE})"
                    ));
                    return with_warnings(
                        acquire_new_worktree(
                            runner, &storage, &identity, repo_path, issue, session, cache_base,
                        ),
                        issue,
                        warnings,
                    );
                }
            }
            None => {
                if let Some(message) = binding_failure.as_ref() {
                    warnings.push(message.clone());
                }
            }
        }
    }

    // Rule 4: any other active lease for the repo isolates by default
    // (issue #436) so two sessions never race for one checkout path (the
    // `(repo_identity, worktree_path)` lease index rejects that reuse).
    let other_active = count_other_active_leases(&storage, &identity, issue, session)?;
    if other_active > 0 {
        warnings.push(format!(
            "another active lease exists for this repository; acquiring issue {issue} in an \
             isolated worktree ({AUTO_ISOLATION_DEFAULT_NOTE})"
        ));
        return with_warnings(
            acquire_new_worktree(
                runner, &storage, &identity, repo_path, issue, session, cache_base,
            ),
            issue,
            warnings,
        );
    }

    // Explicit isolation request (--isolate / worktree-auto, issue #509):
    // the operator asked for a separate worktree, so never reuse the
    // current checkout. Rule 1 idempotent home-coming above is unaffected.
    // Placed after the conflict triggers (Unknown/dirty Rules 2-4) so those
    // paths keep their specific warnings; the clean reuse path below is the
    // one this gate fixes. Every path with the flag still ends in a fresh
    // worktree.
    if explicit_isolation {
        warnings.push(format!(
            "explicit isolation requested (--isolate or worktree-auto); acquiring issue \
             {issue} in an isolated worktree"
        ));
        return with_warnings(
            acquire_new_worktree(
                runner, &storage, &identity, repo_path, issue, session, cache_base,
            ),
            issue,
            warnings,
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
        issue,
        warnings,
    )
}

/// Warning suffix shared by every conflict trigger that isolates by
/// default (issue #436). Kept short so the stderr payload stays readable
/// and the wording names both the new default and the retained flag.
const AUTO_ISOLATION_DEFAULT_NOTE: &str = "auto-isolation now defaults on for a dirty checkout or an active lease; \
     --isolate remains accepted as the explicit opt-in";

/// Three-state result of the checkout dirty probe.
///
/// `Unknown` represents a `git status` failure: the checkout state
/// cannot be trusted, so acquire must not treat it as `Clean` or
/// `Dirty`. This replaces the old `bool` fallback that folded probe
/// errors into `clean` (issue 305 Task 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirtyState {
    Clean,
    Dirty,
    Unknown,
}

/// Map the provider-free [`is_clean`] probe onto [`DirtyState`].
///
/// A probe error becomes `Unknown` plus a bounded warning string; it is
/// never an acquire error, because a git hiccup must not block reuse
/// when isolation is off, and must not silently reuse a possibly-dirty
/// tree when isolation is on.
fn probe_dirty_state(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
) -> (DirtyState, Option<String>) {
    match is_clean(runner, repo_path) {
        Ok(true) => (DirtyState::Clean, None),
        Ok(false) => (DirtyState::Dirty, None),
        Err(error) => (
            DirtyState::Unknown,
            Some(format!(
                "dirty probe failed ({}); git status could not determine the checkout state",
                error.message
            )),
        ),
    }
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

/// Attach best-effort warnings to a successful outcome, run the
/// post-acquire local lifecycle, and forward every warning to stderr so
/// stdout (and the CLI JSON envelope) stays clean for consumers that key
/// on `created`. Errors pass through untouched — the lifecycle never
/// runs when acquire itself failed.
fn with_warnings(
    result: Result<AcquireOutcome, WorktreeError>,
    issue: u64,
    warnings: Vec<String>,
) -> Result<AcquireOutcome, WorktreeError> {
    let outcome = result?;
    let mut warnings = warnings;
    warnings.extend(finalize_checkout(issue, &outcome.path));
    for warning in &warnings {
        crate::cli::report_local_warnings("worktree acquire", Some(warning.clone()));
    }
    Ok(AcquireOutcome {
        warnings,
        ..outcome
    })
}

/// Post-acquire local lifecycle (issue #436): bind `issue` to the
/// acquired checkout's branch and install the managed commit hooks, so a
/// single `worktree acquire` leaves the operator ready to work.
///
/// Both steps reuse the canonical helpers unchanged
/// ([`crate::branch_context::bind`] and
/// [`crate::lifecycle::auto_install_hooks`]), so their semantics are
/// preserved exactly: an existing binding to a different issue is never
/// overwritten (the conflict message is surfaced as a warning and names
/// `--replace`), a detached HEAD or a local git failure degrades to a
/// warning, and hooks are installed only when the checkout has a
/// resolvable origin. The lease row is already durable when this runs,
/// so nothing here may turn a successful acquire into a failure, and no
/// branch, lease row, or checkout is ever deleted.
fn finalize_checkout(issue: u64, checkout: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let workdir = PathBuf::from(checkout);
    let runner = ProcessGitRunner::in_directory(workdir.clone());
    if let Err(error) = crate::branch_context::bind(&runner, issue, false) {
        warnings.push(format!(
            "issue {issue} was not bound to the acquired checkout: {}",
            error.message
        ));
    }
    if let Some(origin) = crate::lifecycle::origin_identity(&runner) {
        let outcome = crate::lifecycle::auto_install_hooks(&runner, &workdir, &origin);
        if let Some(warning) = outcome.warning() {
            warnings.push(warning);
        }
    }
    warnings
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
    release_lease_inner(lease_id, retain, None)
}

/// Forced release with a recorded operator justification
/// (`worktree release --force --reason`). Behaves like
/// `release_lease` for the state transition, but persists `reason`
/// on the row so the override stays attributable in `worktree
/// list`. Lease rows are never deleted; a no-op on an
/// already-terminal lease records nothing.
pub fn release_lease_forced(
    lease_id: &str,
    retain: bool,
    reason: &str,
) -> Result<ReleaseOutcome, WorktreeError> {
    release_lease_inner(lease_id, retain, Some(reason))
}

fn release_lease_inner(
    lease_id: &str,
    retain: bool,
    reason: Option<&str>,
) -> Result<ReleaseOutcome, WorktreeError> {
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
            forced: false,
            reason: None,
        });
    }
    update_status(&storage, lease_id, target)?;
    if let Some(reason) = reason {
        record_release_reason(&storage, lease_id, reason)?;
    }
    Ok(ReleaseOutcome {
        lease_id: lease_id.to_owned(),
        status: target.to_owned(),
        forced: reason.is_some(),
        reason: reason.map(str::to_owned),
    })
}

/// Refresh the heartbeat of an `active` lease the caller owns.
///
/// The update is gated on `status = 'active' AND session = <session>` in
/// one SQL statement, so a non-owner session can never extend a lease and
/// a stale recovery that already flipped the row makes the heartbeat fail
/// with a structured `state` conflict instead of resurrecting it (issue
/// 305 Task 2).
pub fn heartbeat_lease(lease_id: &str, session: &str, now: i64) -> Result<LeaseRow, WorktreeError> {
    if session.trim().is_empty() {
        return Err(WorktreeError::new("argument", "session must not be empty"));
    }
    if session.chars().count() > MAX_SESSION_CHARS {
        return Err(WorktreeError::new(
            "argument",
            format!("session must be <= {MAX_SESSION_CHARS} chars"),
        ));
    }
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    if heartbeat_active_lease(&storage, lease_id, session, now)? {
        return load_lease(&storage, lease_id)?
            .ok_or_else(|| WorktreeError::new("state", "lease disappeared during heartbeat"));
    }
    match load_lease(&storage, lease_id)? {
        None => Err(WorktreeError::new("state", "lease not found")),
        Some(row) if row.status != LEASE_STATUS_ACTIVE => Err(WorktreeError::new(
            "state",
            format!("lease is not active (status '{}')", row.status),
        )),
        Some(row) => Err(WorktreeError::new(
            "state",
            format!(
                "lease is owned by session '{}', not '{}'",
                row.session, session
            ),
        )),
    }
}

/// Release every `active` lease for `(repo_identity, issue)` to
/// `retained`, across every session, recording `reason`, and return the
/// flipped row count.
///
/// This is the issue-close lifecycle hook (issue 305 Task 3, widened to
/// all sessions by issue 537 Phase 2): closing an issue releases every
/// active lease it owns so no second session is left waiting for stale
/// pruning. The caller invokes it only after the remote provider
/// confirmed the close, so a failed remote close never mutates local
/// lease state. Leases owned by another issue or repository are
/// untouched, and only `active` rows transition — terminal rows remain
/// as audit records. The worktree directory and branch are never
/// deleted.
pub fn release_active_leases_for_issue(
    repo_identity: &str,
    issue: u64,
    reason: &str,
) -> Result<u64, WorktreeError> {
    if issue == 0 {
        return Err(WorktreeError::new("argument", "issue must be > 0"));
    }
    let mut storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    retain_active_leases_for_issue(&mut storage, repo_identity, issue, reason, now_unix_secs())
}

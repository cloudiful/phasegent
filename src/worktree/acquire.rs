//! Acquire / release lease orchestration. The functions in this file
//! wire together the [`crate::worktree::leases`] storage helpers, the
//! [`crate::worktree::naming`] format / cache helpers, and the
//! [`crate::worktree::git`] git wrappers. Splitting them out keeps the
//! main [`crate::worktree`] module focused on types, error
//! vocabulary, and the public surface.

use std::path::{Path, PathBuf};

use crate::branch_context::ProcessGitRunner;
use crate::infra::storage::Storage;
use crate::worktree::git::{
    current_branch_for, is_clean, ref_resolves_to_commit, worktree_add_from, worktree_remove,
};
use crate::worktree::leases::{
    NewLease, ensure_schema, find_active_lease, find_active_lease_for_path, heartbeat_active_lease,
    insert_lease, load_lease, record_release_reason, refresh_heartbeat,
    retain_active_leases_for_issue, update_status,
};
use crate::worktree::naming::{
    cache_root, cache_root_in, compute_fingerprint, generate_branch, new_lease_id, slug_from_branch,
};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    MAX_SESSION_CHARS, ReleaseOutcome, WorktreeError, WorktreeRunner, now_unix_secs, repo_identity,
};

/// Options that shape one [`acquire_lease_with`] call beyond the
/// `(repo, issue, session)` triple. The policy surface is one coherent
/// reuse/isolate pair (issue 651 P3): `--isolate` forces a fresh
/// worktree, `--reuse` states the reuse preference explicitly, and the
/// default is reuse-current-unless-the-target-path-is-actively-occupied.
/// The obsolete boolean `worktree-auto` switch and the implicit
/// `reuse_only` gate were removed; a leftover `PHASEGENT_WORKTREE_AUTO`
/// row or environment value is inert data and is never consulted.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcquireOptions<'a> {
    /// Directory under which `<cache>/worktrees/<fingerprint>` is
    /// created. `None` uses the OS cache dir (or
    /// `PHASEGENT_WORKTREE_CACHE_DIR` when set).
    pub cache_base: Option<&'a Path>,
    /// `--isolate`: force a fresh worktree even on an otherwise reusing
    /// checkout state (issue #509). Mutually exclusive with `reuse`;
    /// setting both is a structured `argument` error.
    pub isolate: bool,
    /// Explicit `--base REF` request. `Some` never reuses the current
    /// checkout after the idempotent home-coming and instead creates the
    /// new worktree/branch from `REF` rather than `HEAD`.
    pub base: Option<&'a str>,
    /// The reuse preference (the implicit `issue create` / `issue bind`
    /// hook always sets this; direct API callers opt in per call). A
    /// path conflict that would otherwise call for a fresh worktree
    /// returns actionable `isolation` guidance instead of creating one
    /// (issue 616); the preference never bypasses an active lease on
    /// the target checkout path. The explicit `worktree acquire`
    /// command runs with this `false` — its `--reuse` spelling states
    /// the same default at the parser layer — keeping the default
    /// conflict isolation.
    pub reuse: bool,
}

/// Test-facing entry point: an acquire with no explicit `--base`,
/// delegating to [`acquire_lease_with`]. It keeps the explicit
/// `worktree acquire` contract, so creation is allowed on a conflict
/// unless `reuse` states the reuse preference.
#[allow(dead_code)]
pub fn acquire_lease(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    cache_base: Option<&Path>,
    isolate: bool,
    reuse: bool,
) -> Result<AcquireOutcome, WorktreeError> {
    acquire_lease_with(
        runner,
        repo_path,
        issue,
        session,
        AcquireOptions {
            cache_base,
            isolate,
            base: None,
            reuse,
        },
    )
}

/// Acquire (or refresh) a worktree lease for `(repo_path, issue,
/// session)`. The algorithm is best-effort but always durable:
/// either an active lease is returned (the same one if one already
/// existed for the triple, a fresh one otherwise) or a structured
/// error is returned; partial state is never left behind because the
/// `git worktree add` runs before the lease row is inserted.
///
/// The decision table (issue 651 P2, lease-first) is evaluated in
/// order. Only an active lease on the target checkout path itself
/// isolates: the incoming task gets a dedicated
/// `phasegent/<issue>-<short6hex>` branch and a worktree under
/// `~/.cache/phasegent/worktrees/<fingerprint>/<slug>` instead of
/// colliding with the occupant on the `(repo, worktree_path)` lease
/// index. Dirty state and branch ownership never decide — they are
/// advisory context only (see below), so a dirty tree, an unbound
/// branch, or a branch linked to a historical issue all reuse the
/// current checkout when its path is free. `--isolate` forces a fresh
/// branch/worktree (issue #509): it skips every reuse path and
/// acquires an isolated worktree directly; there is deliberately no
/// reuse fallback for an explicit isolation request. `--reuse` states
/// the reuse preference explicitly and is mutually exclusive with
/// `--isolate`; it never bypasses an occupied checkout path.
///
/// [`AcquireOptions::reuse`] is the reuse preference: the implicit
/// `issue create` / `issue bind` hook (issue 616) always sets it, and
/// direct API callers opt in per call. (The CLI `--reuse` spelling
/// states the same default at the parser layer; the default table
/// never bypasses an occupied path with or without it.) A path
/// conflict never creates a directory by itself for a `reuse` caller
/// and instead returns a structured `isolation` error naming
/// `phasegent worktree acquire --issue N --isolate`. The idempotent
/// home-coming (rule 1) and the safe reuse path stay available.
///
/// The dirty probe feeding the advisory warning is a three-state value
/// (`Clean` / `Dirty` / `Unknown`, issue 305 Task 4). A failed
/// `git status` yields `Unknown` and never counts as `Clean`: with
/// `--isolate` a fresh worktree is created, and otherwise the current
/// checkout is reused with an explicit warning. Explicit isolation
/// additionally covers the `Clean` path, so the flag forces a fresh
/// worktree on every checkout state.
///
/// 1. An active lease exists for `(repo, issue, session)` — the same
///    `lease_id`, `path`, and `branch` are returned and `heartbeat_at`
///    is updated. `created` is `false` and `reason == "idempotent"`.
///    This home-coming is checked before the explicit `--isolate`
///    gate below, so it is unaffected by isolation.
/// 2. An active lease exists for `(repo, worktree_path ==
///    repo_path)` — the target checkout is occupied by another live
///    session, so a fresh worktree is created. `created` is `true`,
///    `reason == "new_worktree"`, and the warning names the occupying
///    session, issue, and lease. A `reuse` caller gets the
///    `isolation` guidance error instead. Only `active` rows count: a
///    released or retained predecessor is history, never a conflict.
///    An active lease on any *other* path (a linked worktree, a stale
///    checkout elsewhere) never blocks a free primary checkout.
/// 3. Otherwise the current checkout is reused (no new worktree, no
///    new branch), but an `active` lease is still recorded so the
///    release path can flip it later. `created` is `false` and
///    `reason == "no_conflict"`. When the checkout is dirty or its
///    state is unknown, a best-effort advisory warning is recorded in
///    `AcquireOutcome::warnings` and emitted on stderr; the probe
///    never decides, never stashes, never resets, and never mutates
///    branches.
///
/// Branch ownership is deliberately not consulted: durable links and
/// legacy Git bindings remain available to explicit branch/status
/// surfaces, but acquire never guesses ownership from them (issue
/// 651). A branch linked to a historical issue therefore reuses
/// exactly like an unbound one.
///
/// On every successful outcome (including the idempotent and reuse
/// paths) [`finalize_checkout`] best-effort binds `issue` to the
/// acquired checkout's branch and installs the managed commit hooks,
/// so one acquire command leaves the operator ready to work. Both
/// steps reuse the canonical helpers unchanged and degrade to
/// warnings: the lease is already durable, so a local binding or hook
/// failure must never turn a successful acquire into an error.
///
/// An explicit `AcquireOptions::base` short-circuits the decision table
/// after Rule 1: the same `(repo, issue, session)` still returns its
/// idempotent lease, but a fresh acquire creates the new
/// branch/worktree from `REF` instead of `HEAD` and never reuses the
/// current checkout. The ref is validated read-only first, so a bad
/// base fails with a structured `git` error and writes nothing.
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
pub fn acquire_lease_with(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    options: AcquireOptions<'_>,
) -> Result<AcquireOutcome, WorktreeError> {
    let AcquireOptions {
        cache_base,
        isolate,
        base,
        reuse,
    } = options;
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
    // Coherent reuse/isolate pair (issue 651 P3): `--isolate` forces a
    // fresh branch/worktree while `--reuse` states the reuse preference
    // explicitly. Passing both is contradictory, so it fails fast with
    // a structured `argument` error before any storage or git work; the
    // CLI parser rejects the same combination with a usage error.
    if isolate && reuse {
        return Err(WorktreeError::new(
            "argument",
            "explicit --isolate and --reuse are mutually exclusive; pass one or neither",
        ));
    }
    // Explicit isolation request (issue #509): `--isolate` forces a
    // fresh branch/worktree on every checkout state. The path-scoped
    // conflict below isolates for callers that may create; this gate
    // additionally skips every reuse path below.
    let explicit_isolation = isolate;
    // Creation gate (issue 616): the implicit `issue create` / `issue
    // bind` hook runs with the reuse preference, so a path conflict
    // resolves to guidance instead of a fresh worktree. The explicit
    // `worktree acquire` command keeps creating on a path conflict
    // unless `--reuse` states the preference.
    let may_create = !reuse;
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

    // Explicit `--base REF` request (issue 595): the idempotent
    // home-coming above still wins, but for a fresh triple an explicit
    // base never reuses the current checkout. A fresh branch/worktree is
    // created from `REF` instead of `HEAD`, and `acquire_new_worktree`
    // validates the ref before creating a directory or a lease row so a
    // bad base leaves no half state behind.
    if let Some(base_ref) = base {
        warnings.push(format!(
            "explicit --base '{base_ref}' requested; acquiring issue {issue} in a fresh worktree \
             from '{base_ref}'"
        ));
        return with_warnings(
            acquire_new_worktree(
                runner,
                &storage,
                &identity,
                repo_path,
                issue,
                session,
                cache_base,
                Some(base_ref),
            ),
            issue,
            warnings,
        );
    }

    // Dirty probe. The result is a three-state value so a `git status`
    // failure is `Unknown` instead of being silently folded into
    // `clean` (issue 305 Task 4). A probe failure is still best-effort:
    // it never hard-errors the acquire.
    let (dirty_state, probe_failure) = probe_dirty_state(runner, repo_path);

    // Unknown checkout state (`git status` failed): the tree may hold
    // work we cannot see, so it must not be reused silently. With
    // `--isolate` a fresh worktree is created; otherwise the current
    // checkout is reused with an explicit warning.
    if dirty_state == DirtyState::Unknown {
        let detail = probe_failure.unwrap_or_else(|| "git status failed".to_owned());
        if explicit_isolation {
            warnings.push(format!(
                "{detail}; explicit --isolate requested, creating an isolated worktree \
                 because the checkout state is unknown"
            ));
            return with_warnings(
                acquire_new_worktree(
                    runner, &storage, &identity, repo_path, issue, session, cache_base, base,
                ),
                issue,
                warnings,
            );
        }
        warnings.push(format!(
            "{detail}; reusing the current checkout despite the unknown dirty state (no \
             --isolate; an unknown state never counts as clean)"
        ));
    }

    // Advisory only (issue 651 P2): a dirty checkout never decides.
    // The warning stays so the orchestrator sees the reused tree is
    // not pristine, but branch ownership is not consulted at all —
    // durable links and legacy bindings never reach this table.
    if dirty_state == DirtyState::Dirty {
        warnings.push(
            "checkout is dirty; reusing it (dirty state is advisory only and never forces \
              isolation)"
                .to_owned(),
        );
    }

    // Rule 2 (issue 651 P2): the only automatic isolation trigger is
    // an active lease on the target checkout path itself. Rule 1
    // already returned our own triple, so any row this lookup finds
    // belongs to someone else and the checkout is genuinely occupied:
    // reusing it would collide on the active-only
    // `(repo_identity, worktree_path)` index. An active lease on any
    // *other* path (a linked worktree, a checkout elsewhere) never
    // blocks a free primary, and terminal history never conflicts. A
    // `reuse` caller may not create (issue 616), so it receives
    // actionable `isolation` guidance instead. The error trigger stays
    // compact so the bounded message never truncates the explicit
    // command; the full occupant detail (including the lease id) goes
    // to the warning.
    let checkout = repo_path.to_string_lossy().to_string();
    if let Some(occupant) = find_active_lease_for_path(&storage, &identity, &checkout)? {
        let trigger = format!(
            "checkout occupied by session '{}' (issue {})",
            occupant.session, occupant.issue
        );
        if !may_create {
            return Err(isolation_required(issue, &trigger));
        }
        warnings.push(format!(
            "{trigger} (lease {}); acquiring issue {issue} in an isolated worktree \
             ({PATH_CONFLICT_NOTE})",
            occupant.lease_id
        ));
        return with_warnings(
            acquire_new_worktree(
                runner, &storage, &identity, repo_path, issue, session, cache_base, base,
            ),
            issue,
            warnings,
        );
    }

    // Explicit isolation request (--isolate, issue #509):
    // the operator asked for a separate worktree, so never reuse the
    // current checkout. Rule 1 idempotent home-coming above is unaffected.
    // Placed after the conflict trigger (Unknown/path-conflict) so those
    // paths keep their specific warnings; the reuse path below is the
    // one this gate fixes. Every path with the flag still ends in a fresh
    // worktree.
    if explicit_isolation {
        warnings.push(format!(
            "explicit isolation requested (--isolate); acquiring issue {issue} in an isolated \
             worktree"
        ));
        return with_warnings(
            acquire_new_worktree(
                runner, &storage, &identity, repo_path, issue, session, cache_base, base,
            ),
            issue,
            warnings,
        );
    }

    // Rule 3: reuse the current checkout. Advisory dirty/unknown
    // warnings were queued above; nothing here stashes, resets, or
    // mutates branches.
    with_warnings(
        acquire_reuse_current(runner, &storage, &identity, repo_path, issue, session),
        issue,
        warnings,
    )
}

/// Warning suffix shared by the path-conflict trigger for a caller
/// that may create (issue 651 P2). Kept short so the stderr payload
/// stays readable and the wording names both the safety boundary and
/// the retained flag.
const PATH_CONFLICT_NOTE: &str = "only an active lease on this checkout path forces isolation; \
     --isolate remains accepted as the explicit opt-in";

/// Error kind for a conflict that only an explicit isolation opt-in may
/// resolve (issue 616): the caller asked for reuse / bookkeeping while the
/// checkout is not safe to reuse and automatic creation is off.
pub(crate) const ISOLATION_REQUIRED_KIND: &str = "isolation";

/// True when `error` is the [`ISOLATION_REQUIRED_KIND`] conflict, so an
/// implicit caller can surface its actionable guidance while every other
/// failure keeps the caller's existing degradation.
pub(crate) fn is_isolation_required(error: &WorktreeError) -> bool {
    error.kind == ISOLATION_REQUIRED_KIND
}

/// Actionable guidance for a `reuse` conflict (issue 616): name the
/// trigger, state that creation is opt-in, and give the explicit command
/// that creates a dedicated worktree. The message stays inside the bounded
/// error length so the command is never truncated.
fn isolation_required(issue: u64, trigger: &str) -> WorktreeError {
    WorktreeError::new(
        ISOLATION_REQUIRED_KIND,
        format!(
            "{trigger}; isolation is opt-in: keep the current path only if it is safe, or run \
             `phasegent worktree acquire --issue {issue} --isolate`"
        ),
    )
}

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

/// Validate an explicit `--base REF` before any worktree is created.
/// `git rev-parse --verify --quiet <ref>^{commit}` is read-only and
/// network-free, so a typo or a missing ref fails with a structured
/// `git` error and no branch, directory, or lease row is written.
fn validate_base_ref(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    reference: &str,
) -> Result<(), WorktreeError> {
    match ref_resolves_to_commit(runner, repo_path, reference) {
        Ok(true) => Ok(()),
        Ok(false) => Err(WorktreeError::new(
            "git",
            format!("base ref '{reference}' does not resolve to a commit"),
        )),
        Err(error) => Err(WorktreeError::new(
            "git",
            format!(
                "base ref '{reference}' could not be resolved: {}",
                error.message
            ),
        )),
    }
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
fn acquire_new_worktree(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    issue: u64,
    session: &str,
    cache_base: Option<&Path>,
    base: Option<&str>,
) -> Result<AcquireOutcome, WorktreeError> {
    // Validate an explicit base before any directory or lease work: a bad
    // ref must fail locally and leave no half state behind.
    if let Some(base_ref) = base {
        validate_base_ref(runner, repo_path, base_ref)?;
    }
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
    worktree_add_from(
        runner,
        repo_path,
        &worktree_path,
        &branch,
        base.unwrap_or("HEAD"),
    )?;
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

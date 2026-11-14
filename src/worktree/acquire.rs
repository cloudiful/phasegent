//! Worktree directory acquisition.
//!
//! The registry records physical directories and their task/session
//! associations. Acquisition takes the per-repository operation lock before
//! reading the registry or touching Git, so cooperating acquires, switches,
//! cleanups, and removals serialize from the fresh check through the short
//! registry transaction.

use std::path::{Path, PathBuf};

use crate::git_runner::ProcessGitRunner;
use crate::infra::storage::Storage;
use crate::worktree::git::{
    current_branch_for, is_clean, ref_resolves_to_commit, worktree_add_from, worktree_remove,
};
use crate::worktree::occupancy::{self, ForeignIssueState};
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::operation_lock;
use crate::worktree::registry::{self, NewAssociation, NewDirectory};
use crate::worktree::{
    AcquireOutcome, MAX_SESSION_CHARS, WorktreeError, WorktreeRunner, WorktreeScope, now_unix_secs,
    repo_identity,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct AcquireOptions<'a> {
    pub cache_base: Option<&'a Path>,
    pub isolate: bool,
    pub base: Option<&'a str>,
    pub reuse: bool,
}

#[allow(dead_code)]
pub fn acquire_directory(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    cache_base: Option<&Path>,
    isolate: bool,
    reuse: bool,
) -> Result<AcquireOutcome, WorktreeError> {
    acquire_directory_with(
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

#[allow(dead_code)]
pub fn acquire_directory_with(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    options: AcquireOptions<'_>,
) -> Result<AcquireOutcome, WorktreeError> {
    validate_acquire(issue, session, options)?;
    let identity = repo_identity(runner, repo_path)?;
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    registry::ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    acquire_directory_inner(
        runner,
        &storage,
        AcquireRequest {
            identity: &identity,
            repo_path,
            issue,
            session,
            options,
            association: None,
            foreign_state: None,
            host_api: None,
        },
    )
}

#[allow(dead_code)]
pub(crate) fn acquire_directory_with_foreign_state(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: &str,
    options: AcquireOptions<'_>,
    foreign_state: Option<&dyn ForeignIssueState>,
    host_api: Option<&dyn OpenCodeApi>,
) -> Result<AcquireOutcome, WorktreeError> {
    validate_acquire(issue, session, options)?;
    let identity = repo_identity(runner, repo_path)?;
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    registry::ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    acquire_directory_inner(
        runner,
        &storage,
        AcquireRequest {
            identity: &identity,
            repo_path,
            issue,
            session,
            options,
            association: None,
            foreign_state,
            host_api,
        },
    )
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn acquire_directory_seeded(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    issue: u64,
    session: &str,
    options: AcquireOptions<'_>,
    foreign_state: Option<&dyn ForeignIssueState>,
    host_api: Option<&dyn OpenCodeApi>,
) -> Result<AcquireOutcome, WorktreeError> {
    validate_acquire(issue, session, options)?;
    acquire_directory_inner(
        runner,
        storage,
        AcquireRequest {
            identity,
            repo_path,
            issue,
            session,
            options,
            association: None,
            foreign_state,
            host_api,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn acquire_directory_scoped(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    session: &str,
    options: AcquireOptions<'_>,
    scope: &WorktreeScope,
    foreign_state: Option<&dyn ForeignIssueState>,
    host_api: Option<&dyn OpenCodeApi>,
) -> Result<AcquireOutcome, WorktreeError> {
    let issue = scope.external_id();
    validate_acquire(issue, session, options)?;
    let association = association_for_scope(scope);
    acquire_directory_inner(
        runner,
        storage,
        AcquireRequest {
            identity,
            repo_path,
            issue,
            session,
            options,
            association: Some(association),
            foreign_state,
            host_api,
        },
    )
}

fn association_for_scope(scope: &WorktreeScope) -> NewAssociation {
    match scope.task_id() {
        Some(task_id) => NewAssociation::Task {
            task_id,
            issue: scope.external_id(),
            provider: scope.provider().to_owned(),
            project: scope.project().to_owned(),
            external_id: scope.external_id(),
        },
        None => NewAssociation::Legacy {
            issue: scope.external_id(),
            provider: scope.provider().to_owned(),
            project: scope.project().to_owned(),
            external_id: scope.external_id(),
        },
    }
}

fn association_for(issue: u64, scope: Option<&NewAssociation>) -> NewAssociation {
    scope.cloned().unwrap_or(NewAssociation::Unscoped { issue })
}

fn validate_acquire(
    issue: u64,
    session: &str,
    options: AcquireOptions<'_>,
) -> Result<(), WorktreeError> {
    if issue == 0 {
        return Err(WorktreeError::new("argument", "issue must be > 0"));
    }
    if session.trim().is_empty() || session.chars().count() > MAX_SESSION_CHARS {
        return Err(WorktreeError::new(
            "argument",
            format!("session must be 1..={MAX_SESSION_CHARS} chars"),
        ));
    }
    if options.isolate && options.reuse {
        return Err(WorktreeError::new(
            "argument",
            "explicit --isolate and --reuse are mutually exclusive; pass one or neither",
        ));
    }
    Ok(())
}

struct AcquireRequest<'a> {
    identity: &'a str,
    repo_path: &'a Path,
    issue: u64,
    session: &'a str,
    options: AcquireOptions<'a>,
    association: Option<NewAssociation>,
    foreign_state: Option<&'a dyn ForeignIssueState>,
    host_api: Option<&'a dyn OpenCodeApi>,
}

fn acquire_directory_inner(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    request: AcquireRequest<'_>,
) -> Result<AcquireOutcome, WorktreeError> {
    let AcquireRequest {
        identity,
        repo_path,
        issue,
        session,
        options,
        association,
        foreign_state,
        host_api,
    } = request;
    // Every path-keyed read and write below names the actual checkout root, so
    // acquiring from a subdirectory reuses that checkout's record instead of
    // registering the nested path as a separate directory.
    let root = crate::worktree::git::checkout_root(runner, repo_path)?;
    let repo_path = root.as_path();
    let association = association_for(issue, association.as_ref());
    let lock = operation_lock::acquire(identity)?;
    let _ = lock;

    let mut warnings: Vec<String> = Vec::new();
    let explicit_isolation = options.isolate;

    // A base ref only chooses the start point of an explicitly requested
    // separate worktree. It never creates one by itself.
    if options.base.is_some() && !explicit_isolation {
        return Err(WorktreeError::new(
            "argument",
            "worktree acquire --base requires explicit --isolate; a base ref never creates a worktree by itself",
        ));
    }

    if let Some(existing) = record_for_own_session(storage, identity, &association, session)? {
        let existing_canonical = existing.canonical_path();
        let current_canonical = crate::worktree::canonical_or_self(repo_path);
        // An explicit isolation request never reuses the original checkout.
        // An already-created exact isolated target may be reused idempotently.
        let reusable = !explicit_isolation || existing_canonical != current_canonical;
        if reusable {
            // A registry row preserves absent evidence; it is not proof that
            // the directory still exists or still belongs to this repository.
            // Validate the physical target before answering a success.
            reusable_directory_is_valid(runner, identity, &existing)?;
            let protection = occupancy::foreign_protection_reason(
                repo_path,
                &existing,
                &association,
                foreign_state,
                host_api,
                &[session.to_owned()],
            );
            match protection {
                None => {
                    registry::attach_association(
                        storage,
                        &existing.directory_id,
                        identity,
                        now_unix_secs(),
                        session,
                        &association,
                    )?;
                    return with_warnings(
                        Ok(AcquireOutcome {
                            directory_id: existing.directory_id,
                            path: existing.worktree_path,
                            branch: existing.branch,
                            repo_identity: existing.repo_identity,
                            created: false,
                            reason: "idempotent".to_owned(),
                            warnings: Vec::new(),
                        }),
                        warnings,
                    );
                }
                Some(protection) if !explicit_isolation => {
                    return Err(isolation_required(issue, &protection));
                }
                Some(_) => {
                    // A requested separate tree bypasses the conflicting
                    // original checkout; the new target is validated below.
                }
            }
        }
    }

    if explicit_isolation {
        warnings.push(format!(
            "explicit isolation requested (--isolate); acquiring issue {issue} in an isolated \
             worktree"
        ));
        return with_warnings(
            acquire_new_worktree(
                runner,
                storage,
                identity,
                repo_path,
                issue,
                session,
                options.cache_base,
                options.base,
                &association,
            ),
            warnings,
        );
    }

    // A session bound to another task is not evidence that this issue owns the
    // session or its directory. Evaluate every registration that names this
    // session on its full identity, including a record that also carries our
    // own exact association: the rest of that directory is still foreign.
    for record in registry::load_records(storage, Some(identity))? {
        if !record.has_session(session) {
            continue;
        }
        if let Some(protection) = occupancy::foreign_protection_reason(
            repo_path,
            &record,
            &association,
            foreign_state,
            host_api,
            &[session.to_owned()],
        ) {
            return Err(isolation_required(issue, &protection));
        }
    }

    let (dirty_state, probe_failure) = probe_dirty_state(runner, repo_path);
    if dirty_state == DirtyState::Unknown {
        let detail = probe_failure.unwrap_or_else(|| "git status failed".to_owned());
        warnings.push(format!(
            "{detail}; reusing the current checkout despite the unknown dirty state (no \
             --isolate; an unknown state never counts as clean)"
        ));
    }
    if dirty_state == DirtyState::Dirty {
        warnings.push(
            "checkout is dirty; reusing it (dirty state is advisory only and never forces \
              isolation)"
                .to_owned(),
        );
    }

    match registry::record_for_path(storage, identity, repo_path)? {
        Some(occupant) => {
            if let Some(protection) = occupancy::foreign_protection_reason(
                repo_path,
                &occupant,
                &association,
                foreign_state,
                host_api,
                &[session.to_owned()],
            ) {
                // Isolation is always the caller's explicit choice. A conflict
                // is surfaced with the exact command instead of quietly
                // creating a worktree.
                return Err(isolation_required(issue, &protection));
            }
        }
        // An empty registry is not a free checkout. Ask the connected host
        // about the actual current directory before reusing it.
        None => {
            if let Some(protection) = occupancy::unregistered_protection_reason(
                host_api,
                repo_path,
                &[session.to_owned()],
            ) {
                return Err(isolation_required(issue, &protection));
            }
        }
    }

    with_warnings(
        acquire_reuse_current(runner, storage, identity, repo_path, session, &association),
        warnings,
    )
}

pub(crate) const ISOLATION_REQUIRED_KIND: &str = "isolation";

pub(crate) fn is_isolation_required(error: &WorktreeError) -> bool {
    error.kind == ISOLATION_REQUIRED_KIND
}

fn isolation_required(issue: u64, trigger: &str) -> WorktreeError {
    WorktreeError::new(
        ISOLATION_REQUIRED_KIND,
        format!(
            "{trigger}; isolation is opt-in: keep the current path only if it is safe, or run \
             `phasegent worktree acquire --issue {issue} --isolate`"
        ),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirtyState {
    Clean,
    Dirty,
    Unknown,
}

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

fn with_warnings(
    result: Result<AcquireOutcome, WorktreeError>,
    warnings: Vec<String>,
) -> Result<AcquireOutcome, WorktreeError> {
    let outcome = result?;
    let mut warnings = warnings;
    warnings.extend(finalize_checkout(&outcome.path));
    for warning in &warnings {
        crate::cli::report_local_warnings("worktree acquire", Some(warning.clone()));
    }
    Ok(AcquireOutcome {
        warnings,
        ..outcome
    })
}

fn finalize_checkout(checkout: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let workdir = PathBuf::from(checkout);
    let runner = ProcessGitRunner::in_directory(workdir.clone());
    if let Some(origin) = crate::lifecycle::origin_identity(&runner) {
        let outcome = crate::lifecycle::auto_install_hooks(&runner, &workdir, &origin);
        if let Some(warning) = outcome.warning() {
            warnings.push(warning);
        }
    }
    warnings
}

/// Prove an idempotently reusable registration still names this repository's
/// live Git worktree. A row whose directory was removed, replaced by a plain
/// directory, or repurposed for another repository is preserved as evidence
/// but never returned as a successful placement.
fn reusable_directory_is_valid(
    runner: &dyn WorktreeRunner,
    expected_identity: &str,
    existing: &crate::worktree::DirectoryRecord,
) -> Result<(), WorktreeError> {
    let path = PathBuf::from(&existing.worktree_path);
    if !path.exists() {
        return Err(WorktreeError::new(
            "state",
            format!(
                "the registered worktree {} is missing; its evidence is preserved and reuse is                  refused. Restore the worktree, or prune the registration after verifying it",
                path.display()
            ),
        ));
    }
    if !crate::worktree::git::is_inside_work_tree(runner, &path)? {
        return Err(WorktreeError::new(
            "state",
            format!(
                "the registered path {} is not a Git worktree; refusing to reuse it as a \
                 placement",
                path.display()
            ),
        ));
    }
    let found = repo_identity(runner, &path)?;
    if !crate::worktree::same_directory(Path::new(&found), Path::new(expected_identity)) {
        return Err(WorktreeError::new(
            "state",
            format!(
                "the registered path {} belongs to another repository ({found}); refusing to \
                 reuse it as a placement",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn record_for_own_session(
    storage: &Storage,
    identity: &str,
    association: &NewAssociation,
    session: &str,
) -> Result<Option<crate::worktree::DirectoryRecord>, WorktreeError> {
    Ok(registry::load_records(storage, Some(identity))?
        .into_iter()
        .rev()
        .find(|record| {
            record.has_session(session) && occupancy::owns_association(association, record)
        }))
}

fn acquire_reuse_current(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    identity: &str,
    repo_path: &Path,
    session: &str,
    association: &NewAssociation,
) -> Result<AcquireOutcome, WorktreeError> {
    let branch = current_branch_for(runner, repo_path)?;
    let checkout = repo_path.to_string_lossy().to_string();
    let canonical = crate::worktree::canonical_or_self(repo_path)
        .to_string_lossy()
        .into_owned();
    let now = now_unix_secs();
    if let Some(existing) = registry::record_for_path(storage, identity, repo_path)? {
        registry::attach_association(
            storage,
            &existing.directory_id,
            identity,
            now,
            session,
            association,
        )?;
        return Ok(AcquireOutcome {
            directory_id: existing.directory_id,
            path: existing.worktree_path,
            branch: existing.branch,
            repo_identity: existing.repo_identity,
            created: false,
            reason: "no_conflict".to_owned(),
            warnings: Vec::new(),
        });
    }
    let directory_id = registry::stable_directory_id(identity, &canonical);
    registry::insert_directory(
        storage,
        &NewDirectory {
            directory_id: &directory_id,
            repo_identity: identity,
            checkout_path: &checkout,
            worktree_path: &checkout,
            branch: &branch,
            created_at: now,
            session,
            associations: std::slice::from_ref(association),
        },
    )?;
    Ok(AcquireOutcome {
        directory_id,
        path: checkout,
        branch,
        repo_identity: identity.to_owned(),
        created: false,
        reason: "no_conflict".to_owned(),
        warnings: Vec::new(),
    })
}

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
    association: &NewAssociation,
) -> Result<AcquireOutcome, WorktreeError> {
    if let Some(base_ref) = base {
        validate_base_ref(runner, repo_path, base_ref)?;
    }
    let (branch, _short) = crate::worktree::generate_branch(issue)?;
    let fingerprint = crate::worktree::compute_fingerprint(identity);
    let slug = crate::worktree::slug_from_branch(&branch)?;
    let cache_root = match cache_base {
        Some(base) => crate::worktree::cache_root_in(base, &fingerprint)?,
        None => crate::worktree::cache_root(&fingerprint)?,
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
    let checkout = repo_path.to_string_lossy().to_string();
    let worktree_str = worktree_path.to_string_lossy().to_string();
    let canonical = crate::worktree::canonical_or_self(&worktree_path)
        .to_string_lossy()
        .into_owned();
    let directory_id = registry::stable_directory_id(identity, &canonical);
    let now = now_unix_secs();
    if let Err(error) = registry::insert_directory(
        storage,
        &NewDirectory {
            directory_id: &directory_id,
            repo_identity: identity,
            checkout_path: &checkout,
            worktree_path: &worktree_str,
            branch: &branch,
            created_at: now,
            session,
            associations: std::slice::from_ref(association),
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
        directory_id,
        path: worktree_str,
        branch,
        repo_identity: identity.to_owned(),
        created: true,
        reason: "new_worktree".to_owned(),
        warnings: Vec::new(),
    })
}

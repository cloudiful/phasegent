//! Centralized single-number task routing (issue 1201 P6).
//!
//! A numbered command first looks for a durable task identity in the local
//! store, read-only. A verified route builds a dispatcher pinned to the
//! stored instance/project with the current role credential, so changing the
//! default provider never redirects a bound task. No route keeps the legacy
//! resolution path unchanged.

use std::collections::BTreeSet;

use crate::branch_links::store::LinkParams;
use crate::branch_links::{IssueKey, checkout_root, read_origin_url, resolve_repo_key};
use crate::cli::provider_context::ProviderContext;
use crate::git_runner::ProcessGitRunner;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineConfig;
use crate::providers::forgejo::ForgejoConfig;
use crate::providers::{IssueProvider, ProviderDispatcher, ProviderKind};
use crate::task_identity::{
    TaskBinding, TaskKey, add_repo_membership, add_repo_membership_with, binding_for_task_id,
    binding_for_task_id_on, bindings_for_repo_number, bindings_for_repo_number_with,
    canonical_instance, ensure_binding, ensure_binding_with,
};

pub(crate) struct ResolvedTaskRoute {
    pub(crate) binding: TaskBinding,
    pub(crate) provider: ProviderDispatcher,
}

/// The current repository's task for one number, read on an open store.
///
/// The branch reference wins over the repository membership, exactly like
/// the single-number route, so an acquisition books the same task the
/// numbered commands would use. Several memberships for one number are an
/// ambiguity, never a first-row guess.
fn resolve_task_for_repo_in(
    storage: &Storage,
    number: u64,
) -> Result<Option<TaskBinding>, PhasegentError> {
    if !storage
        .table_exists("task_bindings")
        .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    let Some((repo_key, branch)) = repo_context() else {
        return Ok(None);
    };
    let refs = branch_task_refs(&storage.connection, &repo_key, &branch)?;
    for (row_number, id) in refs {
        if row_number != number {
            continue;
        }
        let binding = binding_for_task_id(storage, id)
            .map_err(PhasegentError::config)?
            .ok_or_else(|| {
                config_error(format!(
                    "branch task reference {id} has no durable task binding"
                ))
            })?;
        if binding.key().external_number() != Some(number) {
            return Err(config_error(format!(
                "branch link for number {number} references a different durable task; \
                 refusing to redirect the requested number"
            )));
        }
        return Ok(Some(binding));
    }
    if !storage
        .table_exists("task_repo_memberships")
        .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    let candidates =
        bindings_for_repo_number(storage, &repo_key, number).map_err(PhasegentError::config)?;
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => Err(config_error(AMBIGUOUS)),
    }
}

/// Every durable task for one number inside one physical repository.
///
/// The repository is named by the caller, so a global pass never reads the
/// current checkout's branch as another repository's proof. The lookup runs
/// on the caller's own connection, so a pass reads the very store it owns
/// and never a second, differently-scoped database.
pub(crate) fn resolve_number_in_repo(
    connection: &rusqlite::Connection,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, PhasegentError> {
    bindings_for_repo_number_with(connection, repo_key, number).map_err(PhasegentError::config)
}

/// One durable task by its opaque local id.
///
/// The lookup runs on the caller's own connection, so a pass reads the very
/// store it owns.
pub(crate) fn resolve_by_task_id(
    connection: &rusqlite::Connection,
    task_id: i64,
) -> Result<Option<TaskBinding>, PhasegentError> {
    binding_for_task_id_on(connection, task_id).map_err(PhasegentError::config)
}

/// The timer admission decision for one issue number.
///
/// The decision is scoped to the current physical repository: a durable task
/// of another repository never picks a dispatcher and never blocks a
/// supported local flow. Because the timer ledger stores no task id, a
/// decision is current-context consistency, never proof of the run's start
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimerTaskRoute {
    /// No durable task in the current repository: the legacy provider path
    /// is unchanged.
    Legacy,
    /// The current repository's task is Local: the run stays local and is
    /// never projected to a remote provider.
    Local,
    /// The current repository's task is Redmine: the time entry is pinned to
    /// this instance and project.
    Redmine { instance: String, project: String },
}

impl TimerTaskRoute {
    /// Whether the decision leaves the operation on the legacy provider path
    /// because the current repository carries no durable task.
    pub(crate) fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }
}

/// Resolve the timer route of one issue number, read-only.
///
/// * No durable task in the current repository keeps the legacy provider
///   path unchanged.
/// * A Local task stays local: the run is never projected remotely.
/// * A Redmine task pins the time entry to its own instance and project. The
///   configured endpoint must still attest that instance, so a changed
///   default never repins the bound task and the role credential never
///   reaches another instance.
/// * A Forgejo task has no time tracking and refuses before any ledger
///   write, as does an ambiguous current-repository task or durable
///   evidence that spans more than one Redmine scope.
pub(crate) fn resolve_timer_route(issue: u64) -> Result<TimerTaskRoute, PhasegentError> {
    let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
    let Some(storage) = storage else {
        return Ok(TimerTaskRoute::Legacy);
    };
    if !storage
        .table_exists("task_bindings")
        .map_err(PhasegentError::config)?
    {
        return Ok(TimerTaskRoute::Legacy);
    }
    let current = resolve_task_for_repo_in(&storage, issue)?;
    // Multi-instance attribution is unsupported: the ledger stores no task
    // id, so conflicting durable Redmine evidence refuses instead of
    // selecting an endpoint.
    if redmine_scopes(&storage, issue)?.len() > 1 {
        return Err(config_error(
            "the timer issue has durable Redmine tasks on more than one scope; the timer ledger \
             stores no task id, so the run cannot be attributed to one of them",
        ));
    }
    let Some(binding) = current else {
        return Ok(TimerTaskRoute::Legacy);
    };
    let key = binding.key();
    match key.provider() {
        "forgejo" => Err(PhasegentError::not_supported("forgejo", "time tracking")),
        "local" => Ok(TimerTaskRoute::Local),
        "redmine" => {
            if let Some(trusted) =
                crate::auth::redmine_api_base_read_only(&storage).map_err(PhasegentError::config)?
            {
                let normalized =
                    normalize_instance(ProviderKind::Redmine, &trusted, key.project())?;
                if normalized != key.instance() {
                    return Err(config_error(format!(
                        "configured Redmine endpoint does not match the stored task instance; \
                         refusing to send the role credential (task {})",
                        key.instance()
                    )));
                }
            }
            Ok(TimerTaskRoute::Redmine {
                instance: key.instance().to_owned(),
                project: key.project().to_owned(),
            })
        }
        other => Err(config_error(format!(
            "durable task has unsupported provider '{other}'"
        ))),
    }
}

/// The distinct Redmine scopes of one number, read-only.
///
/// Only Redmine scopes count: a Local or Forgejo task of the same number is
/// a different provider, not a competing Redmine endpoint.
fn redmine_scopes(
    storage: &Storage,
    issue: u64,
) -> Result<BTreeSet<(String, String)>, PhasegentError> {
    let bindings = crate::task_identity::bindings_for_number(&storage.connection, issue)
        .map_err(PhasegentError::config)?;
    let mut scopes = BTreeSet::new();
    for binding in bindings {
        let key = binding.key();
        if key.provider() == "redmine" {
            scopes.insert((key.instance().to_owned(), key.project().to_owned()));
        }
    }
    Ok(scopes)
}

fn config_error(message: impl Into<String>) -> PhasegentError {
    PhasegentError::config(message.into())
}

fn normalize_instance(
    kind: ProviderKind,
    endpoint: &str,
    project: &str,
) -> Result<String, PhasegentError> {
    canonical_instance(kind, Some(endpoint), project).map_err(config_error)
}

fn now_secs() -> i64 {
    crate::worktree::now_unix_secs().max(1)
}

/// The checkout's durable repository key plus current branch, if any.
fn repo_context() -> Option<(String, String)> {
    let runner = ProcessGitRunner::new();
    let origin = read_origin_url(&runner);
    let resolved = resolve_repo_key(origin.as_deref(), &checkout_root(&runner)).ok()?;
    let branch = crate::git_runner::current_branch(&runner).ok()?;
    Some((resolved.key, branch))
}

fn branch_task_refs(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
) -> Result<Vec<(u64, i64)>, PhasegentError> {
    crate::branch_links::reads::branch_task_refs(connection, repo_key, branch).map_err(config_error)
}

fn binding_matches_context(
    binding: &TaskBinding,
    context: &ProviderContext,
) -> Result<bool, PhasegentError> {
    let key = binding.key();
    fields_match_context(key.provider(), key.instance(), key.project(), context)
}

/// Whether an explicit provider selection matches one durable task scope.
///
/// The selection is a filter, never an override: a task it does not match
/// keeps its own group instead of being redirected to another provider,
/// project or instance.
pub(crate) fn selection_matches_scope(
    context: &ProviderContext,
    provider: &str,
    instance: &str,
    project: &str,
) -> Result<bool, PhasegentError> {
    fields_match_context(provider, instance, project, context)
}

/// Whether the explicit selection matches one canonical task scope.
fn fields_match_context(
    provider: &str,
    instance: &str,
    project: &str,
    context: &ProviderContext,
) -> Result<bool, PhasegentError> {
    if let Some(kind) = context.provider
        && kind.as_str() != provider
    {
        return Ok(false);
    }
    match provider {
        "forgejo" => {
            if context.project_id.is_some() || context.close_status_id.is_some() {
                return Err(config_error(
                    "Forgejo does not support --project-id or --close-status-id",
                ));
            }
            if let Some(repository) = context.repository.as_deref()
                && repository != project
            {
                return Ok(false);
            }
            if let Some(base) = context.api_base.as_deref() {
                let normalized = normalize_instance(ProviderKind::Forgejo, base, project)?;
                if normalized != instance {
                    return Ok(false);
                }
            }
        }
        "redmine" => {
            if let Some(selected) = context.project_id.as_deref()
                && selected != project
            {
                return Ok(false);
            }
            if let Some(base) = context.api_base.as_deref() {
                let normalized = normalize_instance(ProviderKind::Redmine, base, project)?;
                if normalized != instance {
                    return Ok(false);
                }
            }
        }
        "local" => {
            let has_remote_selector = context.api_base.is_some()
                || context.repository.is_some()
                || context.project_id.is_some();
            if has_remote_selector {
                return Ok(false);
            }
        }
        _ => {}
    }
    Ok(true)
}

const AMBIGUOUS: &str = "multiple durable tasks match this number in the repository; pass --provider, \
     --repository/--project-id or --api-base to select one";

/// Resolve one numbered command through the durable identity store.
///
/// Returns `Ok(None)` when the store has no identity tables (legacy store) or
/// no binding matches, so the caller keeps the existing default/legacy path.
pub(crate) fn resolve_single_number(
    role: Role,
    context: &ProviderContext,
    number: u64,
) -> Result<Option<ResolvedTaskRoute>, PhasegentError> {
    let Some(binding) = resolve_number_binding(context, number)? else {
        return Ok(None);
    };
    let provider = pinned_provider(role, &binding, context, false)?;
    Ok(Some(ResolvedTaskRoute { binding, provider }))
}

/// The durable task of one number for a caller that needs only the identity.
///
/// The matching rules are the same ones [`resolve_single_number`] applies —
/// the branch task reference first, then a unique repository membership, both
/// filtered by the complete raw explicit context — but no credential is
/// resolved and no provider is built, so a purely local caller (a worktree
/// acquisition) never needs a role token to prove which task owns the number.
/// `Ok(None)` means the repository holds no task for the number and the
/// caller keeps the legacy path.
pub(crate) fn resolve_number_task(
    context: &ProviderContext,
    number: u64,
) -> Result<Option<TaskBinding>, PhasegentError> {
    resolve_number_binding(context, number)
}

fn resolve_number_binding(
    context: &ProviderContext,
    number: u64,
) -> Result<Option<TaskBinding>, PhasegentError> {
    let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
    let Some(storage) = storage else {
        return Ok(None);
    };
    if !storage
        .table_exists("task_bindings")
        .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }

    let mut candidates: Vec<TaskBinding> = Vec::new();
    let repo = repo_context();
    if let Some((repo_key, branch)) = repo.as_ref() {
        let refs = branch_task_refs(&storage.connection, repo_key, branch)?;
        for (row_number, id) in refs {
            if row_number != number {
                continue;
            }
            let binding = binding_for_task_id(&storage, id)
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    config_error(format!(
                        "branch task reference {id} has no durable task binding"
                    ))
                })?;
            if binding.key().external_number() != Some(number) {
                return Err(config_error(format!(
                    "branch link for number {number} references a different durable task;                      refusing to redirect the requested number"
                )));
            }
            candidates.push(binding);
        }
        if candidates.len() > 1 {
            return Err(config_error(AMBIGUOUS));
        }
    }

    if candidates.is_empty()
        && storage
            .table_exists("task_repo_memberships")
            .map_err(PhasegentError::config)?
        && let Some((repo_key, _)) = repo.as_ref()
    {
        candidates =
            bindings_for_repo_number(&storage, repo_key, number).map_err(PhasegentError::config)?;
        if candidates.len() > 1 {
            let explicit = context.provider.is_some()
                || context.repository.is_some()
                || context.project_id.is_some()
                || context.api_base.is_some();
            let mut selected = Vec::new();
            for candidate in candidates {
                if binding_matches_context(&candidate, context)? {
                    selected.push(candidate);
                }
            }
            if !explicit || selected.is_empty() {
                return Err(config_error(AMBIGUOUS));
            }
            if selected.len() > 1 {
                return Err(config_error(AMBIGUOUS));
            }
            candidates = selected;
        }
    }

    let Some(binding) = candidates.into_iter().next() else {
        return Ok(None);
    };
    if !binding_matches_context(&binding, context)? {
        return Err(config_error(format!(
            "explicit provider/endpoint/project does not match durable task {}; \
             refusing to redirect the bound task",
            binding.key().external_id()
        )));
    }
    Ok(Some(binding))
}

/// The pinned provider of one exact durable task.
///
/// The task's own stored instance and project are the only sources of the
/// target. The role credential is used only when the configured endpoint
/// provenance matches that instance, so a task whose instance this role does
/// not serve keeps without a request and without a token leaving the
/// process. An explicit provider, endpoint or project selection that
/// disagrees with the task is refused instead of redirecting it.
///
/// `Ok(None)` means the task id has no durable binding at all: the caller
/// keeps that reference as unproven rather than dispatching it.
pub(crate) fn pinned_provider_for_task(
    role: Role,
    connection: &rusqlite::Connection,
    task_id: i64,
    context: &ProviderContext,
) -> Result<Option<(TaskBinding, ProviderDispatcher)>, PhasegentError> {
    let Some(binding) = resolve_by_task_id(connection, task_id)? else {
        return Ok(None);
    };
    let provider = provider_for_binding(role, &binding, context)?;
    Ok(Some((binding, provider)))
}

/// The pinned provider of one already-resolved durable task.
///
/// The context check is the same one [`pinned_provider_for_task`] applies, so
/// a caller that already read the binding still refuses an explicit selection
/// that disagrees with it instead of redirecting the task.
pub(crate) fn provider_for_binding(
    role: Role,
    binding: &TaskBinding,
    context: &ProviderContext,
) -> Result<ProviderDispatcher, PhasegentError> {
    provider_for_binding_with(role, binding, context, false)
}

/// The read-only form of [`provider_for_binding`]: the same binding, context
/// and provenance checks, but a Local task opens only an existing store and a
/// missing store keeps instead of creating or seeding one.
///
/// The prune composition uses this so a read-only candidate proof never
/// initializes the separate Local database. Every other provider and every
/// close/sync caller keeps the write-capable [`provider_for_binding`].
pub(crate) fn provider_for_binding_read_only(
    role: Role,
    binding: &TaskBinding,
    context: &ProviderContext,
) -> Result<ProviderDispatcher, PhasegentError> {
    provider_for_binding_with(role, binding, context, true)
}

fn provider_for_binding_with(
    role: Role,
    binding: &TaskBinding,
    context: &ProviderContext,
    local_read_only: bool,
) -> Result<ProviderDispatcher, PhasegentError> {
    if !binding_matches_context(binding, context)? {
        return Err(config_error(format!(
            "explicit provider/endpoint/project does not match durable task {}; \
             refusing to redirect the bound task",
            binding.key().external_id()
        )));
    }
    pinned_provider(role, binding, context, local_read_only)
}

/// The durable tasks of one number inside one physical repository, read on
/// the caller's own connection.
///
/// The membership is the only proof: a number whose task lives in another
/// repository is never resolved through a global number lookup.
pub(crate) fn tasks_for_repo_number(
    connection: &rusqlite::Connection,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, PhasegentError> {
    resolve_number_in_repo(connection, repo_key, number)
}

fn pinned_provider(
    role: Role,
    binding: &TaskBinding,
    context: &ProviderContext,
    local_read_only: bool,
) -> Result<ProviderDispatcher, PhasegentError> {
    let key = binding.key();
    match key.provider() {
        "forgejo" => {
            let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
            let stored = storage
                .as_ref()
                .ok_or_else(|| config_error("Forgejo role configuration is not present"))?
                .load_forgejo_config(role)
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    config_error(
                        "Forgejo role credential provenance is not configured for this role",
                    )
                })?;
            let stored_instance =
                normalize_instance(ProviderKind::Forgejo, &stored.api_base, &stored.repository)?;
            if stored_instance != key.instance() {
                return Err(config_error(format!(
                    "configured Forgejo endpoint does not match the stored task instance; \
                     refusing to send the role credential (task {}:{})",
                    key.instance(),
                    key.project()
                )));
            }
            let token = storage
                .as_ref()
                .unwrap()
                .load_credential(role, "forgejo")
                .map_err(PhasegentError::auth)?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| PhasegentError::auth("Forgejo role credential is not configured"))?;
            let config = ForgejoConfig::new(key.instance(), key.project())?;
            crate::providers::forgejo::ForgejoProvider::new(config, &token)
                .map(ProviderDispatcher::Forgejo)
        }
        "redmine" => {
            let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
            let storage = storage
                .as_ref()
                .ok_or_else(|| config_error("Redmine role configuration is not present"))?;
            let trusted = crate::auth::redmine_api_base_read_only(storage)
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    config_error("Redmine API base is not configured for credential provenance")
                })?;
            let normalized = normalize_instance(ProviderKind::Redmine, &trusted, key.project())?;
            if normalized != key.instance() {
                return Err(config_error(format!(
                    "configured Redmine endpoint does not match the stored task instance; \
                     refusing to send the role credential (task {})",
                    key.instance()
                )));
            }
            let close_status_id = match context.close_status_id.as_deref() {
                Some(value) => Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| config_error("Redmine close status id must be numeric"))?,
                ),
                None => crate::auth::load_redmine_config(role, storage)
                    .map_err(PhasegentError::config)?
                    .and_then(|config| config.close_status_id),
            };
            if close_status_id == Some(0) {
                return Err(config_error(
                    "Redmine close status id must be greater than zero",
                ));
            }
            let api_key =
                crate::auth::redmine_api_key(role, storage).map_err(PhasegentError::auth)?;
            let config = RedmineConfig {
                api_base: key.instance().to_owned(),
                project_id: Some(key.project().to_owned()),
                close_status_id,
            };
            crate::providers::config::RedmineProvider::new(config, api_key)
                .map(ProviderDispatcher::Redmine)
        }
        "local" => {
            let open = if local_read_only {
                crate::providers::local::LocalProvider::open_read_only
            } else {
                crate::providers::local::LocalProvider::open
            };
            open().map(ProviderDispatcher::local)
        }
        other => Err(config_error(format!(
            "durable task has unsupported provider '{other}'"
        ))),
    }
}

fn key_from_provider(
    provider: &ProviderDispatcher,
    number: u64,
) -> Result<TaskKey, PhasegentError> {
    match provider {
        ProviderDispatcher::Forgejo(forgejo) => {
            let key = TaskKey::new(
                "forgejo",
                forgejo.api_base(),
                forgejo.repository_scope(),
                &number.to_string(),
            )
            .map_err(PhasegentError::config)?;
            Ok(key)
        }
        ProviderDispatcher::Redmine(redmine) => {
            let project = redmine.config.project_id.as_deref().ok_or_else(|| {
                config_error("Redmine project is unresolved; cannot persist a task identity")
            })?;
            let key = TaskKey::new(
                "redmine",
                &redmine.config.api_base,
                project,
                &number.to_string(),
            )
            .map_err(PhasegentError::config)?;
            Ok(key)
        }
        ProviderDispatcher::Local(_) => {
            TaskKey::new("local", "local", "default", &number.to_string())
                .map_err(PhasegentError::config)
        }
    }
}

/// Persist the created issue's durable identity after a successful remote
/// create. Returns the binding; repo membership is added only when a physical
/// repository is identifiable.
pub(crate) fn persist_created_issue(
    provider: &ProviderDispatcher,
    number: u64,
) -> Result<(TaskBinding, bool), String> {
    let key = key_from_provider(provider, number).map_err(|error| {
        error
            .json()
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("could not derive the task identity")
            .to_owned()
    })?;
    let storage = Storage::open()?;
    let now = now_secs();
    let binding = ensure_binding(&storage, &key, now)?;
    let mut associated = false;
    if let Some((repo_key, _)) = repo_context() {
        add_repo_membership(&storage, &repo_key, binding.task_id(), now)?;
        associated = true;
    }
    Ok((binding, associated))
}

/// Verify an explicit provider binding without any local write. No provider
/// selection means the unchanged offline local-only bind path (`Ok(None)`).
pub(crate) fn preflight_explicit_issue(
    role: Role,
    context: &ProviderContext,
    number: u64,
) -> Result<Option<TaskKey>, PhasegentError> {
    let Some(kind) = context.provider else {
        return Ok(None);
    };
    let provider = match kind {
        ProviderKind::Local => crate::providers::local::LocalProvider::open()
            .map(ProviderDispatcher::local)
            .map_err(|error| PhasegentError::auth(error.to_string()))?,
        ProviderKind::Forgejo => {
            let config = ForgejoConfig::resolve(
                role,
                context.api_base.as_deref(),
                context.repository.as_deref(),
            )?;
            let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
            let stored = storage
                .as_ref()
                .ok_or_else(|| config_error("Forgejo role configuration is not present"))?
                .load_forgejo_config(role)
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    config_error(
                        "Forgejo role credential provenance is not configured for this role",
                    )
                })?;
            let selected_instance = normalize_instance(
                ProviderKind::Forgejo,
                config.api_base(),
                config.repository(),
            )?;
            let stored_instance =
                normalize_instance(ProviderKind::Forgejo, &stored.api_base, &stored.repository)?;
            if selected_instance != stored_instance {
                return Err(config_error(
                    "explicit Forgejo endpoint does not match the configured role instance; \
                     refusing to send it",
                ));
            }
            let token = storage
                .as_ref()
                .unwrap()
                .load_credential(role, "forgejo")
                .map_err(PhasegentError::auth)?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| PhasegentError::auth("Forgejo role credential is not configured"))?;
            crate::providers::forgejo::ForgejoProvider::new(config, &token)
                .map(ProviderDispatcher::Forgejo)?
        }
        ProviderKind::Redmine => {
            let project = context.project_id.as_deref().ok_or_else(|| {
                config_error(
                    "Redmine explicit bind requires --project-id because the project cannot be \
                     resolved uniquely",
                )
            })?;
            let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
            let storage = storage
                .as_ref()
                .ok_or_else(|| config_error("Redmine role configuration is not present"))?;
            let trusted = crate::auth::redmine_api_base_read_only(storage)
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    config_error("Redmine API base is not configured for credential provenance")
                })?;
            let api_base = match context.api_base.as_deref() {
                Some(explicit) => {
                    let normalized = normalize_instance(ProviderKind::Redmine, explicit, project)?;
                    let trusted_normalized =
                        normalize_instance(ProviderKind::Redmine, &trusted, project)?;
                    if normalized != trusted_normalized {
                        return Err(config_error(
                            "explicit --api-base does not match the configured Redmine endpoint; \
                             it cannot attest credential provenance",
                        ));
                    }
                    normalized
                }
                None => normalize_instance(ProviderKind::Redmine, &trusted, project)?,
            };
            let close_status_id = context
                .close_status_id
                .as_deref()
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| config_error("Redmine close status id must be numeric"))
                })
                .transpose()?;
            if close_status_id == Some(0) {
                return Err(config_error(
                    "Redmine close status id must be greater than zero",
                ));
            }
            let api_key =
                crate::auth::redmine_api_key(role, storage).map_err(PhasegentError::auth)?;
            let config = RedmineConfig {
                api_base,
                project_id: Some(project.to_owned()),
                close_status_id,
            };
            crate::providers::config::RedmineProvider::new(config, api_key)
                .map(ProviderDispatcher::Redmine)?
        }
    };

    // Verify the remote/local issue before any local write.
    let summary = provider.get_issue(number)?;
    if summary.number != number {
        return Err(config_error(
            "the verified issue number does not match the requested number",
        ));
    }
    let key = key_from_provider(&provider, number)?;
    if key.provider() == "redmine" {
        let project = summary.project.as_ref().ok_or_else(|| {
            config_error("the verified Redmine issue carries no project evidence")
        })?;
        let matches = project.id.to_string() == key.project()
            || project.identifier.as_deref() == Some(key.project())
            || project.name == key.project();
        if !matches {
            return Err(config_error(
                "the verified issue belongs to a different Redmine project; refusing to adopt it",
            ));
        }
    }
    Ok(Some(key))
}

/// Persist a fully preflighted explicit multi-bind in one immediate local
/// transaction: every binding, membership and branch task ref commits
/// together, or none do. No HTTP runs inside the transaction.
pub(crate) fn commit_adoption_set(
    storage: &Storage,
    repo_key: &str,
    branch: &str,
    keys: &[TaskKey],
    now: i64,
) -> Result<Vec<TaskBinding>, PhasegentError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .map_err(|error| config_error(format!("could not begin task adoption: {error}")))?;
    let mut bindings = Vec::with_capacity(keys.len());
    for key in keys {
        let number = key
            .external_number()
            .ok_or_else(|| config_error("adopted task key has a non-numeric external id"))?;
        let binding =
            ensure_binding_with(&transaction, key, now).map_err(PhasegentError::config)?;
        let existing = bindings_for_repo_number_with(&transaction, repo_key, number)
            .map_err(PhasegentError::config)?;
        if existing.iter().any(|candidate| {
            candidate.key().provider() == key.provider()
                && candidate.key().project() == key.project()
                && candidate.key().external_id() == key.external_id()
                && candidate.key() != key
        }) {
            return Err(config_error(
                "the same branch link already belongs to another immutable task instance; \
                 refusing to overwrite it",
            ));
        }
        add_repo_membership_with(&transaction, repo_key, binding.task_id(), now)
            .map_err(PhasegentError::config)?;
        let issue = IssueKey::from_number(key.provider(), key.project(), number)
            .map_err(PhasegentError::config)?;
        let link = LinkParams {
            repo_key,
            branch,
            issue: &issue,
            issue_number: number,
            source: "cli-bind",
            now,
        };
        crate::branch_links::store::link_with_task(&transaction, &link, Some(binding.task_id()))
            .map_err(PhasegentError::config)?;
        bindings.push(binding);
    }
    transaction
        .commit()
        .map_err(|error| config_error(format!("could not commit task adoption: {error}")))?;
    Ok(bindings)
}

/// Attach the created task id to the current branch link when the create
/// helper actually linked the created issue. Best-effort and never creates a
/// link of its own.
pub(crate) fn attach_created_task_to_branch(
    provider: &ProviderDispatcher,
    number: u64,
    task_id: i64,
) -> Result<(), String> {
    let (repo_key, branch) =
        repo_context().ok_or_else(|| "no physical repository or branch".to_owned())?;
    let key = key_from_provider(provider, number).map_err(|error| {
        error
            .json()
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("could not derive the task identity")
            .to_owned()
    })?;
    let storage = Storage::open()?;
    let rows = crate::branch_links::reads::issues_for_branch(
        &storage.connection,
        &repo_key,
        &branch,
        false,
        &crate::branch_links::UnknownState,
    )?;
    let Some(issue) = rows
        .iter()
        .find(|row| {
            row.issue.provider == key.provider()
                && row.issue.project == key.project()
                && row.issue_number == number
        })
        .map(|row| row.issue.clone())
    else {
        return Ok(());
    };
    let now = now_secs();
    let link = LinkParams {
        repo_key: &repo_key,
        branch: &branch,
        issue: &issue,
        issue_number: number,
        source: "issue-create",
        now,
    };
    crate::branch_links::store::link_with_task(&storage.connection, &link, Some(task_id))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_matching_rejects_cross_instance_and_mismatched_project() {
        let binding = TaskBinding::for_test(
            TaskKey::new("forgejo", "https://forgejo.test/root", "owner/repo", "7").unwrap(),
            1,
            10,
        );
        let base = ProviderContext::from_parts(
            Some(ProviderKind::Forgejo),
            Some("https://forgejo.test/root"),
            Some("owner/repo"),
            None,
            None,
        );
        assert!(binding_matches_context(&binding, &base).unwrap());
        let wrong_instance = ProviderContext::from_parts(
            Some(ProviderKind::Forgejo),
            Some("https://other.test/root"),
            Some("owner/repo"),
            None,
            None,
        );
        assert!(!binding_matches_context(&binding, &wrong_instance).unwrap());
        let wrong_project = ProviderContext::from_parts(
            Some(ProviderKind::Forgejo),
            None,
            Some("owner/other"),
            None,
            None,
        );
        assert!(!binding_matches_context(&binding, &wrong_project).unwrap());
    }
}

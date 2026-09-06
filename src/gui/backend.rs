//! Blocking task/status reads for the GUI boundary.
//!
//! Every entry point here is synchronous and runs via
//! `spawn_blocking` from the Tauri commands, so the async runtime
//! stays responsive. Provider access reuses the existing dispatch
//! (`search_issue_page` directly); the issue-index `block_on` bridge
//! is never called from the Tauri runtime.

use super::models::{
    BranchContextPayload, StatusPayload, StatusRequest, TaskEntry, TasksPayload, TasksRequest,
    TimerDto,
};
use super::validate::{
    bound_message, bound_title, now_fetched_at, parse_provider_optional, parse_role_with_default,
    sanitize_optional_url, validate_task_limit, validate_task_state,
};

#[allow(dead_code)]
fn redact_provider_error(error: crate::providers::forgejo::ForgejoError) -> String {
    let json = error.json();
    let kind = json
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("request");
    let message = json
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider request failed");
    // Provider messages never carry credential values; bound and strip
    // control chars so request URLs with userinfo cannot flood the UI.
    format!("{kind}: {}", bound_message(message))
}

#[allow(dead_code)]
fn build_dispatcher(
    role: crate::policy::Role,
    kind: crate::providers::config::ProviderKind,
) -> Result<crate::providers::ProviderDispatcher, String> {
    use crate::providers::config::ProviderKind;
    match kind {
        ProviderKind::Forgejo => {
            let config = crate::providers::forgejo::ForgejoConfig::resolve(role, None, None)
                .map_err(redact_provider_error)?;
            crate::providers::ProviderDispatcher::for_role(role, config)
                .map_err(redact_provider_error)
        }
        ProviderKind::Redmine => {
            let config = crate::providers::RedmineConfig::resolve(role, None, None, None)
                .map_err(redact_provider_error)?;
            crate::providers::ProviderDispatcher::redmine(role, config)
                .map_err(redact_provider_error)
        }
        ProviderKind::Gitlab => {
            let config = crate::providers::GitlabConfig::resolve(role, None, None)
                .map_err(redact_provider_error)?;
            crate::providers::ProviderDispatcher::gitlab(role, config)
                .map_err(redact_provider_error)
        }
    }
}

#[allow(dead_code)]
fn resolve_dispatcher(
    role: crate::policy::Role,
    explicit: Option<crate::providers::config::ProviderKind>,
) -> Result<
    (
        crate::providers::ProviderDispatcher,
        crate::providers::config::ProviderKind,
    ),
    String,
> {
    let kind =
        crate::providers::config::resolve_kind(role, explicit).map_err(redact_provider_error)?;
    let dispatcher = build_dispatcher(role, kind)?;
    Ok((dispatcher, kind))
}

#[allow(dead_code)]
fn branch_snapshot() -> (Option<String>, Option<u64>, Option<String>) {
    let runner = crate::branch_context::ProcessGitRunner::new();
    match crate::branch_context::status(&runner) {
        Ok(status) => (Some(status.branch), status.issue_id, None),
        Err(error) => (None, None, Some(bound_message(&error.message))),
    }
}

#[allow(dead_code)]
fn endpoint_for_role(
    role: crate::policy::Role,
    kind: crate::providers::config::ProviderKind,
    storage: &crate::infra::storage::Storage,
) -> Option<String> {
    use crate::providers::config::ProviderKind;
    let raw: Option<String> = match kind {
        ProviderKind::Forgejo => crate::auth::load_config(role, storage)
            .ok()
            .flatten()
            .and_then(|c| c.api_base),
        ProviderKind::Redmine => crate::auth::load_redmine_config(role, storage)
            .ok()
            .flatten()
            .and_then(|c| c.api_base),
        ProviderKind::Gitlab => crate::auth::load_gitlab_config(role, storage)
            .ok()
            .flatten()
            .and_then(|c| c.api_base),
    };
    raw.map(|v| crate::config_snapshot::sanitize_url(&v))
}

/// Branch context for the GUI (never touches network or secrets).
#[allow(dead_code)]
pub fn read_branch_context() -> Result<BranchContextPayload, String> {
    let (branch, issue_id, warning) = branch_snapshot();
    Ok(BranchContextPayload {
        branch,
        issue_id,
        warning,
    })
}

/// Bounded task list via the existing provider dispatch. Uses
/// `search_issue_page` directly; never calls the index `block_on`.
#[allow(dead_code)]
pub fn read_tasks_blocking(request: TasksRequest) -> Result<TasksPayload, String> {
    use crate::providers::IssueProvider;
    let role = parse_role_with_default(request.role.as_deref())?;
    let explicit = parse_provider_optional(request.provider.as_deref())?;
    let limit = validate_task_limit(request.limit)?;
    let state = validate_task_state(request.state.as_deref())?;
    let (branch, bound_issue, branch_warning) = branch_snapshot();
    let (provider, kind) = resolve_dispatcher(role, explicit)?;
    if !provider.supports(crate::policy::Capability::IssueSearch) {
        return Err(format!("{} does not support issue search", kind.as_str()));
    }
    let options = crate::providers::IssueSearchOptions {
        query: None,
        state,
        page: 1,
        limit,
        include_body: false,
        all: true,
    };
    options.validate().map_err(redact_provider_error)?;
    let page = provider
        .search_issue_page(&options)
        .map_err(redact_provider_error)?;
    let items = page
        .items
        .into_iter()
        .map(|summary| TaskEntry {
            number: summary.number,
            title: bound_title(&summary.title),
            state: bound_message(&summary.state),
            url: sanitize_optional_url(summary.html_url),
        })
        .collect::<Vec<_>>();
    Ok(TasksPayload {
        branch,
        bound_issue,
        provider: kind.as_str().to_owned(),
        role: role.as_str().to_owned(),
        total_count: page.total_count,
        has_more: page.has_more,
        items,
        data_source: "provider".to_owned(),
        fetched_at: now_fetched_at(),
        warning: branch_warning,
    })
}

/// Status payload: branch, bound issue, sanitised endpoint, timers.
/// Redmine status listing maps `not_supported` to a clean field.
#[allow(dead_code)]
pub fn read_status_blocking(request: StatusRequest) -> Result<StatusPayload, String> {
    use crate::providers::IssueProvider;
    use crate::providers::RedmineMetadataProvider;
    let role = parse_role_with_default(request.role.as_deref())?;
    let explicit = parse_provider_optional(request.provider.as_deref())?;
    let (branch, bound_issue, mut warning) = branch_snapshot();
    let (provider, kind) = resolve_dispatcher(role, explicit)?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    let endpoint = endpoint_for_role(role, kind, &storage);
    // Bound issue detail is best-effort; failures degrade, not fail.
    let (issue_title, issue_state, connection) = match bound_issue {
        None => (None, None, "connected".to_owned()),
        Some(number) => match provider.get_issue(number) {
            Ok(summary) => (
                Some(bound_title(&summary.title)),
                Some(bound_message(&summary.state)),
                "connected".to_owned(),
            ),
            Err(error) => {
                let message = redact_provider_error(error);
                warning = Some(match warning {
                    Some(existing) => format!("{existing}; {message}"),
                    None => message,
                });
                (None, None, "degraded".to_owned())
            }
        },
    };
    // Timers are local-only; failures degrade with a warning.
    let (running_timers, recent_timers) = match (
        storage.list_timer_runs(crate::infra::storage::TimerStatusFilter::Running, 20),
        storage.list_timer_runs(crate::infra::storage::TimerStatusFilter::All, 5),
    ) {
        (Ok(running), Ok(recent)) => (
            running.len(),
            recent
                .into_iter()
                .map(|run| TimerDto {
                    run_id: run.run_id,
                    issue: run.issue,
                    phase: bound_message(&run.phase),
                    role: bound_message(&run.role),
                    status: bound_message(&run.status),
                    sync_status: bound_message(&run.sync_status),
                    started_at: run.started_at,
                    finished_at: run.finished_at,
                })
                .collect(),
        ),
        _ => {
            warning = Some(match warning {
                Some(existing) => format!("{existing}; timer list unavailable"),
                None => "timer list unavailable".to_owned(),
            });
            (0, Vec::new())
        }
    };
    // Capability probe: Redmine lists statuses, others report cleanly.
    let statuses_unsupported = {
        match provider.list_issue_statuses() {
            Ok(_) => None,
            Err(error) if error.is_not_supported() => {
                let json = error.json();
                let message = json
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("issue status list is not supported");
                Some(bound_message(message))
            }
            Err(error) => {
                let message = redact_provider_error(error);
                warning = Some(match warning {
                    Some(existing) => format!("{existing}; {message}"),
                    None => message,
                });
                None
            }
        }
    };
    Ok(StatusPayload {
        branch,
        bound_issue,
        bound_issue_title: issue_title,
        bound_issue_state: issue_state,
        provider: kind.as_str().to_owned(),
        role: role.as_str().to_owned(),
        endpoint,
        connection,
        running_timers,
        recent_timers,
        fetched_at: now_fetched_at(),
        warning,
        statuses_unsupported,
    })
}

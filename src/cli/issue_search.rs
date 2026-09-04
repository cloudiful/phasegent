use crate::infra::issue_index_backend::{IssueIndexBackend, block_on};
use crate::policy::{Capability, Role};
use crate::providers::api::{IssueSearchItem, IssueSearchResult, IssueSummary};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexStore};
use crate::providers::index_store::{explicit_scope, lexical_scope_for_state, provider_scope};
use crate::providers::{IssueProvider, ProviderDispatcher, ProviderKind};

pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000)
}

/// Bounded warning text, truncated and never echoing URLs/credentials.
pub(crate) fn bound_warning(reason: &str) -> String {
    const MAX: usize = 300;
    let mut text = reason.trim().replace(['\n', '\r'], " ");
    while text.contains("  ") {
        text = text.replace("  ", " ");
    }
    if text.len() > MAX {
        let mut end = MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

pub(crate) fn doc_from_summary(
    scope_source: &str,
    scope_project: &str,
    summary: &IssueSummary,
    indexed_at: i64,
) -> Result<IssueIndexDocument, String> {
    let key = IssueIndexKey::new(
        scope_source.to_owned(),
        scope_project.to_owned(),
        summary.number.to_string(),
    )?;
    IssueIndexDocument::new(
        key,
        summary.number,
        summary.title.clone(),
        summary.body.clone(),
        summary.state.clone(),
        summary.html_url.clone(),
        None,
        indexed_at,
    )
}

/// Best-effort single-summary write-through after provider success.
/// Any open/upsert failure is a bounded warning, never a result change.
pub(crate) fn warm_single_summary(
    provider: &ProviderDispatcher,
    summary: &IssueSummary,
    operation: &str,
) {
    let scope = match provider_scope(provider) {
        Ok(scope) => scope,
        Err(reason) => {
            crate::cli::report_local_warnings(
                operation,
                Some(format!(
                    "index warm skipped: {}",
                    bound_warning(&reason.to_string())
                )),
            );
            return;
        }
    };
    let indexed_at = now_unix_secs();
    let doc = match doc_from_summary(&scope.source, &scope.project, summary, indexed_at) {
        Ok(doc) => doc,
        Err(reason) => {
            crate::cli::report_local_warnings(
                operation,
                Some(format!("index warm skipped: {}", bound_warning(&reason))),
            );
            return;
        }
    };
    let warning: Option<String> = block_on(async {
        let store = match IssueIndexBackend::open().await {
            Ok(store) => store,
            Err(reason) => return Some(format!("index warm skipped: {}", bound_warning(&reason))),
        };
        match store.upsert(&doc).await {
            Ok(()) => None,
            Err(reason) => Some(format!("index warm skipped: {}", bound_warning(&reason))),
        }
    });
    crate::cli::report_local_warnings(operation, warning);
}

/// Best-effort page write-through using full bodies from the single
/// `search_issue_page` request; output mapping stays compact.
pub(crate) fn warm_search_page(
    provider: &ProviderDispatcher,
    summaries: &[IssueSummary],
    operation: &str,
) {
    if summaries.is_empty() {
        return;
    }
    let scope = match provider_scope(provider) {
        Ok(scope) => scope,
        Err(reason) => {
            crate::cli::report_local_warnings(
                operation,
                Some(format!(
                    "index warm skipped: {}",
                    bound_warning(&reason.to_string())
                )),
            );
            return;
        }
    };
    let indexed_at = now_unix_secs();
    let mut docs = Vec::with_capacity(summaries.len());
    for summary in summaries {
        match doc_from_summary(&scope.source, &scope.project, summary, indexed_at) {
            Ok(doc) => docs.push(doc),
            Err(reason) => {
                crate::cli::report_local_warnings(
                    operation,
                    Some(format!("index warm skipped: {}", bound_warning(&reason))),
                );
                return;
            }
        }
    }
    let warning: Option<String> = block_on(async {
        let store = match IssueIndexBackend::open().await {
            Ok(store) => store,
            Err(reason) => return Some(format!("index warm skipped: {}", bound_warning(&reason))),
        };
        for doc in &docs {
            if let Err(reason) = store.upsert(doc).await {
                return Some(format!("index warm skipped: {}", bound_warning(&reason)));
            }
        }
        None
    });
    crate::cli::report_local_warnings(operation, warning);
}

/// Stale-fallback envelope: provider-fresh keys plus
/// `data_source: "local_index"` and `stale: true`. Items keep additive
/// `source`/`project`/`external_id` scope.
#[derive(serde::Serialize)]
struct LocalFallbackEnvelope {
    items: Vec<IssueSearchItem>,
    page: usize,
    limit: usize,
    total_count: Option<usize>,
    has_more: bool,
    data_source: String,
    stale: bool,
}

/// Transparent search with write-through and stale fallback, using one
/// `search_issue_page` request so no second network call is needed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_search_transparent(
    role: Role,
    explicit_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    options: crate::providers::IssueSearchOptions,
) -> i32 {
    // Each failure preserves the original error; fallback runs only for
    // non-empty queries, never for `not_supported`/argument errors.
    let resolved_kind = match resolve_kind(role, explicit_kind) {
        Ok(kind) => kind,
        Err(error) => {
            return fallback_or_provider_error(
                &error,
                &options,
                None,
                explicit_kind,
                repository,
                project_id,
            );
        }
    };
    // Redmine discovery/bootstrap errors are fallback-eligible.
    let mut effective_project = project_id.map(str::to_owned);
    let mut effective_close = close_status_id.map(str::to_owned);
    if resolved_kind == ProviderKind::Redmine && effective_project.is_none() {
        match crate::cli::project_resolution::resolve_redmine_project(
            role,
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(Some(discovered)) => {
                effective_project = Some(discovered);
            }
            Ok(None) => match crate::workflow::ensure_issue_workflow(
                role,
                api_base,
                repository,
                close_status_id,
            ) {
                Ok(state) => {
                    effective_project = Some(state.project_id);
                    effective_close = Some(state.close_status_id.to_string());
                }
                Err(error) => {
                    return fallback_or_provider_error(
                        &error,
                        &options,
                        None,
                        explicit_kind,
                        repository,
                        project_id,
                    );
                }
            },
            Err(error) => {
                return fallback_or_provider_error(
                    &error,
                    &options,
                    None,
                    explicit_kind,
                    repository,
                    project_id,
                );
            }
        }
    }
    let provider = match crate::cli::provider_for(
        role,
        Some(resolved_kind),
        api_base,
        repository,
        effective_project.as_deref(),
        effective_close.as_deref(),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            return fallback_or_provider_error(
                &error,
                &options,
                None,
                explicit_kind,
                repository,
                project_id,
            );
        }
    };
    if !provider.supports(Capability::IssueSearch) {
        return crate::cli::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            Capability::IssueSearch.operation(),
        ));
    }
    match provider.search_issue_page(&options) {
        Ok(page) => {
            let include_body = options.include_body;
            let output = IssueSearchResult {
                items: page
                    .items
                    .iter()
                    .cloned()
                    .map(|summary| IssueSearchItem::from_summary(summary, include_body))
                    .collect(),
                page: page.page,
                limit: page.limit,
                total_count: page.total_count,
                has_more: page.has_more,
            };
            // Warm with full bodies from the same single page.
            let summaries: Vec<IssueSummary> = page.items;
            let code = crate::cli::print_json(&output);
            warm_search_page(&provider, &summaries, "issue search");
            code
        }
        Err(error) => fallback_or_provider_error(
            &error,
            &options,
            Some(&provider),
            explicit_kind,
            repository,
            project_id,
        ),
    }
}

/// Stale local fallback, else the original provider error. Never calls
/// `resolve_kind`/provider construction again; scope prefers the live
/// dispatcher, then explicit CLI args, then global stale.
pub(crate) fn fallback_or_provider_error(
    original: &ForgejoError,
    options: &crate::providers::IssueSearchOptions,
    provider: Option<&ProviderDispatcher>,
    explicit_kind: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> i32 {
    if original.is_not_supported() || original.is_search_argument_error() {
        return crate::cli::provider_error(original.clone());
    }
    let query = match options.effective_query() {
        Some(query) => query.to_owned(),
        // Queryless `--all` has no lexical fallback.
        None => return crate::cli::provider_error(original.clone()),
    };
    // Scope: live dispatcher wins (filtered to provider/project), else
    // explicit CLI args without provider lookup, else global stale.
    let dispatcher_scope = provider.and_then(|dispatcher| provider_scope(dispatcher).ok());
    let explicit = explicit_scope(explicit_kind, repository, project_id);
    let scope_opt = dispatcher_scope.as_ref().or(explicit.as_ref());
    let lex_scope = lexical_scope_for_state(scope_opt, &options.state);
    let offset = options.page.saturating_sub(1).saturating_mul(options.limit);
    let limit = options.limit;
    let include_body = options.include_body;
    let page = options.page;
    let local: Result<crate::providers::index_store::IssueIndexSearchResult, String> =
        block_on(async {
            let store = match IssueIndexBackend::open().await {
                Ok(store) => store,
                Err(_) => return Err("local index unavailable".to_owned()),
            };
            store
                .lexical_search_scoped(&query, limit, offset, include_body, &lex_scope)
                .await
        });
    let local = match local {
        Ok(value) if !value.items.is_empty() => value,
        _ => return crate::cli::provider_error(original.clone()),
    };
    let items: Vec<IssueSearchItem> = local
        .items
        .iter()
        .map(|item| {
            if include_body {
                IssueSearchItem::from_local_parts(
                    item.source.clone(),
                    item.project.clone(),
                    item.external_id.clone(),
                    item.issue_number,
                    item.title.clone(),
                    item.state.clone(),
                    item.html_url.clone(),
                    item.body.clone().unwrap_or_default(),
                    true,
                )
            } else {
                // Bodies omitted: keep them omitted (no empty-string body).
                IssueSearchItem::from_local_parts(
                    item.source.clone(),
                    item.project.clone(),
                    item.external_id.clone(),
                    item.issue_number,
                    item.title.clone(),
                    item.state.clone(),
                    item.html_url.clone(),
                    String::new(),
                    false,
                )
            }
        })
        .collect();
    let total = local.total_count;
    let has_more = offset + items.len() < total;
    let envelope = LocalFallbackEnvelope {
        items,
        page,
        limit,
        total_count: Some(total),
        has_more,
        data_source: "local_index".to_owned(),
        stale: true,
    };
    eprintln!(
        "{}",
        serde_json::json!({
            "warning": {
                "operation": "issue search",
                "message": format!(
                    "provider search unavailable; returning {} stale local result(s)",
                    envelope.items.len()
                ),
            }
        })
    );
    crate::cli::print_json(&envelope)
}

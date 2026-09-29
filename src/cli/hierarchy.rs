use crate::command::HierarchyCommand;
use crate::policy::{Capability, Role};
use crate::providers::api::ProviderError;
use crate::providers::config::resolve_kind;
use crate::providers::hierarchy::HierarchyPage;
use crate::providers::{IssueProvider, ProviderKind};

/// Operation labels for the structured Local/Forgejo rejection. They match
/// the dispatcher hierarchy operations so provider and CLI surfaces stay
/// byte-identical on the same failure.
pub(crate) const HIERARCHY_GET_OPERATION: &str = "issue hierarchy get";
pub(crate) const HIERARCHY_UPDATE_OPERATION: &str = "issue hierarchy update";

/// Provider-native parent/child hierarchy. `get` projects the bounded
/// hierarchy view and is available to every non-admin role;
/// `set`/`unset` assign or clear the native parent and are
/// orchestrator-only. Redmine uses `parent_issue_id`; GitLab uses the Work
/// Item hierarchy widget with provider-resolved kinds. Local and Forgejo
/// reject with a structured not-supported error before any provider build
/// or network access. Hierarchy never reads or writes relations.
pub(crate) fn execute_hierarchy(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: HierarchyCommand,
) -> i32 {
    let (role, capability) = match &command {
        HierarchyCommand::Get { .. } => (super::required_role(role_value), Capability::IssueRead),
        HierarchyCommand::Set { .. } | HierarchyCommand::Unset { .. } => (
            super::required_role(role_value),
            Capability::IssueUpdateBody,
        ),
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    let operation = match &command {
        HierarchyCommand::Get { .. } => HIERARCHY_GET_OPERATION,
        HierarchyCommand::Set { .. } | HierarchyCommand::Unset { .. } => HIERARCHY_UPDATE_OPERATION,
    };
    // Local and Forgejo expose no native hierarchy surface; reject before
    // any provider build or network access so the structured not-supported
    // error is the only side effect.
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Redmine) | Ok(ProviderKind::Gitlab) => {}
        Ok(kind) => {
            return super::provider_error(ProviderError::not_supported(kind.as_str(), operation));
        }
        Err(error) => return super::provider_error(error),
    }
    let provider = match super::provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return super::provider_error(error),
    };
    if !provider.supports(capability) {
        return super::provider_error(ProviderError::not_supported(
            provider.kind().as_str(),
            operation,
        ));
    }
    match command {
        HierarchyCommand::Get { id } => match provider.get_hierarchy_page(id) {
            Ok(page) => super::print_json(&bound_page(page)),
            Err(error) => super::provider_error(error),
        },
        HierarchyCommand::Set { parent, child } => {
            match provider.set_hierarchy_parent_by_id(child, parent) {
                Ok(()) => super::print_json(
                    &serde_json::json!({"child": child, "parent": parent, "updated": true}),
                ),
                Err(error) => super::provider_error(error),
            }
        }
        HierarchyCommand::Unset { child } => match provider.unset_hierarchy_parent_by_id(child) {
            Ok(()) => super::print_json(
                &serde_json::json!({"child": child, "parent": null, "updated": true}),
            ),
            Err(error) => super::provider_error(error),
        },
    }
}

/// Cap the projected child list at the provider bound and surface the
/// truncation indicator instead of silently dropping children.
pub(crate) fn bound_page(page: HierarchyPage) -> HierarchyPage {
    HierarchyPage::bounded(page.node, page.children_truncated)
}

use crate::command::StatusCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::redmine::model::StatusNextReport;
use crate::providers::redmine::model::status::{STATUS_POLICY_SOURCE, structured_forbidden_json};
use crate::providers::{
    IssueProvider, ProviderDispatcher, ProviderKind, RedmineMetadataProvider, RedmineProvider,
};

/// Print an advance result, attaching the structured Phase 1 `Forbidden`
/// context (`current`/`target`/`allowed_next`/`policy_source`) when the
/// policy preflight rejects the transition. Every other outcome keeps
/// its legacy shape, so success JSON and non-policy errors stay
/// byte-compatible.
fn print_advance_result<T: serde::Serialize>(result: Result<T, ForgejoError>) -> i32 {
    if let Err(error) = &result
        && let Some(payload) = structured_forbidden_json(error)
    {
        return super::structured_error(payload, 1);
    }
    super::print_result(result)
}

/// Resolve a bare `status transition N` (empty-target sentinel from the
/// parser) to the policy first-allowed target. Returns the target name
/// or the terminal/advisory report when no route exists; the caller
/// turns the latter into a structured request error without any PUT.
fn auto_target_from_report(report: &StatusNextReport) -> Result<String, ()> {
    report
        .allowed_next
        .first()
        .map(|next| next.name.clone())
        .ok_or(())
}

/// Structured request error for a bare auto with no route (terminal or
/// advisory-custom status). No PUT is issued; exit 1 matches the
/// `Forbidden` preflight shape (additive `current`/`allowed_next` /
/// `policy_source`, no `target` because none was derived).
fn auto_no_route_error(report: &StatusNextReport) -> serde_json::Value {
    serde_json::json!({
        "kind": "request",
        "operation": "issue status advance",
        "message": format!(
            "no automatic transition: current status '{}' has no policy-allowed next; recovery: {}",
            report.current.name, report.recovery,
        ),
        "current": report.current.name,
        "allowed_next": Vec::<String>::new(),
        "policy_source": STATUS_POLICY_SOURCE,
    })
}

pub(crate) fn execute_status(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: StatusCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = Capability::IssueStatusRead;
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    // Status transitions drive the workflow lifecycle and are
    // orchestrator-owned, mirroring issue close; executors, reviewers,
    // and the admin bootstrap identity may not move an issue's status.
    // The check runs before any provider or network access so a denied
    // role fails fast with a structured permission error.
    // Phase 1-2 (issue 443): `status transition --to` and bare
    // `status transition` both parse to `Advance` (bare uses the
    // empty-target auto sentinel), so this guard covers the new
    // entry with no extra arm.
    if matches!(
        command,
        StatusCommand::Set { .. } | StatusCommand::Advance { .. }
    ) && role != Role::Orchestrator
    {
        return super::structured_error(
            serde_json::json!({
                "kind":"permission",
                "role":role.as_str(),
                "operation":"issue status update",
                "message":"issue status updates are orchestrator-only"
            }),
            3,
        );
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return super::provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        Ok(ProviderKind::Gitlab) => {}
        Ok(ProviderKind::Local) => {}
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
        return super::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        StatusCommand::List => super::print_result(provider.list_issue_statuses()),
        StatusCommand::Next { number } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                // Single-number scope guard (issue 394 P3 pre-read): GET-check
                // before the catalogue + issue reads so cross-project numbers
                // fail with a --project-id hint and never disclose status.
                if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                    role,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    crate::providers::ProviderKind::Redmine,
                    &redmine,
                    number,
                ) {
                    return super::provider_error(error);
                }
                super::print_result(redmine.status_next(number))
            }
            ProviderDispatcher::Local(local) => super::print_result(local.status_next(number)),
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status next",
            )),
        },
        StatusCommand::Advance { number, status } => match &provider {
            ProviderDispatcher::Redmine(redmine) => {
                // Single-number scope guard (issue 394 P3 pre-write): fails
                // before any PUT, including the NoOp path which still reads.
                if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                    role,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    provider.kind(),
                    redmine,
                    number,
                ) {
                    return super::provider_error(error);
                }
                let effective = if status.is_empty() {
                    // Phase 2 (issue 443) bare auto: policy first-allowed
                    // via `status_next`. Scope guard above already ran,
                    // so this read-then-PUT reuses the exact advance
                    // path with the derived target; stdout shape is
                    // identical to `advance --status <derived>`.
                    match redmine.status_next(number) {
                        Ok(report) => match auto_target_from_report(&report) {
                            Ok(target) => target,
                            Err(()) => {
                                return super::structured_error(auto_no_route_error(&report), 1);
                            }
                        },
                        Err(error) => return super::provider_error(error),
                    }
                } else {
                    status.clone()
                };
                let result = redmine.advance_issue_status(number, &effective);
                if result.is_ok() {
                    // Phase 3 write-side relation auto (issue 257):
                    // the trigger point for the parent-child
                    // `relates` auto-link is the successful status
                    // transition. The shared issue DTO is narrow
                    // and does not surface the parent linkage
                    // without a server fetch, so the call site
                    // passes `None` today; the helper stays
                    // idempotent and silent. The create path
                    // (cli/issue.rs) fires the helper with the
                    // resolved `parent_issue_id` and is the only
                    // branch that actually creates a relation in
                    // Phase 3. See Remaining in the audit note
                    // for the deferred lookup shape.
                    super::report_local_warnings(
                        "status advance",
                        crate::lifecycle_auto::auto_create_parent_child_relation(
                            &provider, number, None,
                        )
                        .warning(),
                    );
                    super::report_local_warnings(
                        "status advance",
                        crate::lifecycle_auto::auto_transition_timer(
                            number,
                            ProviderKind::Redmine,
                            &effective,
                        )
                        .warning(),
                    );
                }
                print_advance_result(result)
            }
            ProviderDispatcher::Local(local) => {
                // Phase 2 bare auto on the static local catalogue plus
                // the timer hook for Local parity (stderr-only, stdout
                // unchanged; Forgejo/GitLab arms below are untouched).
                let effective = if status.is_empty() {
                    match local.status_next(number) {
                        Ok(report) => match auto_target_from_report(&report) {
                            Ok(target) => target,
                            Err(()) => {
                                return super::structured_error(auto_no_route_error(&report), 1);
                            }
                        },
                        Err(error) => return super::provider_error(error),
                    }
                } else {
                    status.clone()
                };
                let result = local.advance_issue_status(number, &effective);
                if result.is_ok() {
                    super::report_local_warnings(
                        "status advance",
                        crate::lifecycle_auto::auto_transition_timer(
                            number,
                            ProviderKind::Local,
                            &effective,
                        )
                        .warning(),
                    );
                }
                print_advance_result(result)
            }
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status advance",
            )),
        },
        StatusCommand::Set { number, status } => match &provider {
            ProviderDispatcher::Gitlab(gitlab) => {
                let result = gitlab.set_workflow_status(number, &status);
                if result.is_ok() {
                    // See the `Advance` arm above for the
                    // Phase 3 relation-auto wiring rationale:
                    // the trigger lives at status transitions
                    // but the parent linkage is only resolvable
                    // through the create arm today. The helper
                    // stays silent here.
                    super::report_local_warnings(
                        "status set",
                        crate::lifecycle_auto::auto_create_parent_child_relation(
                            &provider, number, None,
                        )
                        .warning(),
                    );
                    super::report_local_warnings(
                        "status set",
                        crate::lifecycle_auto::auto_transition_timer(
                            number,
                            ProviderKind::Gitlab,
                            &status,
                        )
                        .warning(),
                    );
                }
                super::print_result(result)
            }
            ProviderDispatcher::Redmine(redmine) => {
                // Single-number scope guard (issue 394 P3 pre-write): fails
                // before the catalogue reads and the status PUT.
                if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                    role,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    provider.kind(),
                    redmine,
                    number,
                ) {
                    return super::provider_error(error);
                }
                let statuses = match redmine.list_issue_statuses() {
                    Ok(statuses) => statuses,
                    Err(error) => return super::provider_error(error),
                };
                let target = match RedmineProvider::select_status_by_value(&statuses, &status) {
                    Ok(target) => target,
                    Err(error) => return super::provider_error(error),
                };
                let result = redmine.set_issue_status(number, target.id);
                if result.is_ok() {
                    // See the `Advance` arm above for the
                    // Phase 3 relation-auto wiring rationale.
                    super::report_local_warnings(
                        "status set",
                        crate::lifecycle_auto::auto_create_parent_child_relation(
                            &provider, number, None,
                        )
                        .warning(),
                    );
                    super::report_local_warnings(
                        "status set",
                        crate::lifecycle_auto::auto_transition_timer(
                            number,
                            ProviderKind::Redmine,
                            &status,
                        )
                        .warning(),
                    );
                }
                super::print_result(result)
            }
            ProviderDispatcher::Local(local) => {
                let statuses = match local.list_issue_statuses() {
                    Ok(statuses) => statuses,
                    Err(error) => return super::provider_error(error),
                };
                let target = match RedmineProvider::select_status_by_value(&statuses, &status) {
                    Ok(target) => target,
                    Err(error) => return super::provider_error(error),
                };
                let result = local.set_issue_status(number, target.id);
                if result.is_ok() {
                    // Phase 2 timer parity for Local (stderr-only).
                    super::report_local_warnings(
                        "status set",
                        crate::lifecycle_auto::auto_transition_timer(
                            number,
                            ProviderKind::Local,
                            &status,
                        )
                        .warning(),
                    );
                }
                super::print_result(result)
            }
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status update",
            )),
        },
    }
}

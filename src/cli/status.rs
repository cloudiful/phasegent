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
        // GitLab: Phase 2 surfaces the static `WORKFLOW_LABELS`
        // catalogue through `list_issue_statuses`, so the shared
        // `status list` command now resolves the same eight
        // statuses the Redmine catalogue does. `set` still maps to a
        // managed workflow label update; the orchestrator-only guard
        // above already protects it.
        Ok(ProviderKind::Gitlab) => {}
        // Local flows through to the LocalProvider status catalogue;
        // forgejo stays not-supported, redmine/gitlab unchanged.
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
    // Phase 2 parity matrix (issue 257): the dispatcher now reports
    // `IssueStatusRead = true` for GitLab because the static
    // `WORKFLOW_LABELS` catalogue fills the parity row. Redmine and
    // Local remain native; Forgejo stays not-supported via the
    // explicit reject above.
    if !provider.supports(capability) {
        return super::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        StatusCommand::List => super::print_result(provider.list_issue_statuses()),
        // `next` and `advance` run the canonical policy against the
        // installation catalogue (Redmine) or the static local catalogue.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::providers::local::LocalProvider;

    fn tmp_db(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-cli-status-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("phasegent-local.sqlite3");
        (dir, db)
    }

    #[test]
    fn local_dispatch_routes_next_advance_and_set_arms() {
        let _lock = lock_workflow_tests();
        let (dir, db) = tmp_db("local-dispatch");
        let _guard = EnvGuard::set("PHASEGENT_LOCAL_DB_PATH", db.to_str().unwrap());
        // Phase 2 timer hooks write to the shared ledger; isolate it
        // so Local status moves never touch the operator's real DB.
        let ledger = dir.join("phasegent-ledger.sqlite3");
        let _ledger_guard = EnvGuard::set("PHASEGENT_DB_PATH", ledger.to_str().unwrap());
        // Seed one local issue so each status arm resolves a real row
        // instead of the not-found fallback.
        let provider = LocalProvider::open().unwrap();
        let number = provider.create_issue("Dispatch", "body").unwrap().number;

        // Each dispatcher arm must route `--provider local` through the
        // LocalProvider method and return success (exit 0), not the
        // not_supported fallback (exit 1).
        let next_exit = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Next { number },
        );
        assert_eq!(
            next_exit, 0,
            "status next must route through the ProviderDispatcher::Local arm"
        );

        let advance_exit = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: "In Progress".to_owned(),
            },
        );
        assert_eq!(
            advance_exit, 0,
            "status advance must route through the ProviderDispatcher::Local arm"
        );

        let set_exit = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Set {
                number,
                status: "Resolved".to_owned(),
            },
        );
        assert_eq!(
            set_exit, 0,
            "status set must route through the ProviderDispatcher::Local arm"
        );

        drop(_ledger_guard);
        drop(_guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn local_status_advance_fires_relation_auto_silently() {
        // Phase 3 relation auto: the helper is invoked at every
        // status set/advance success path. The Local provider has
        // no relation surface so the helper returns
        // `AutoRelationOutcome::Skipped` with no warning; this
        // test pins that contract by routing the same `Advance`
        // arm through the LocalProvider and asserting the
        // success exit is unchanged (the helper never poisons
        // stdout or the exit code).
        let _lock = lock_workflow_tests();
        let (dir, db) = tmp_db("rel-auto-silent");
        let _guard = EnvGuard::set("PHASEGENT_LOCAL_DB_PATH", db.to_str().unwrap());
        let ledger = dir.join("phasegent-ledger.sqlite3");
        let _ledger_guard = EnvGuard::set("PHASEGENT_DB_PATH", ledger.to_str().unwrap());
        let provider = LocalProvider::open().unwrap();
        let number = provider.create_issue("Phase3", "body").unwrap().number;
        let exit = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: "In Progress".to_owned(),
            },
        );
        assert_eq!(
            exit, 0,
            "status advance must remain successful even when relation auto is a silent skip"
        );

        drop(_ledger_guard);
        drop(_guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bare_transition_auto_routes_via_policy_first_allowed() {
        // Phase 2 (issue 443): bare `status transition N` (empty-target
        // sentinel) resolves via `status_next` first-allowed and reuses
        // the exact advance PUT path, so AI can move
        // `New -> In Progress -> In Review` without `set`/`advance`.
        // stdout shape matches `advance --status <derived>` by
        // construction; the timer hook stays stderr-only.
        let _lock = lock_workflow_tests();
        let (dir, db) = tmp_db("bare-auto");
        let _guard = EnvGuard::set("PHASEGENT_LOCAL_DB_PATH", db.to_str().unwrap());
        let ledger = dir.join("phasegent-ledger.sqlite3");
        let _ledger_guard = EnvGuard::set("PHASEGENT_DB_PATH", ledger.to_str().unwrap());
        let provider = LocalProvider::open().unwrap();
        let number = provider.create_issue("BareAuto", "body").unwrap().number;

        let first = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: String::new(),
            },
        );
        assert_eq!(first, 0, "bare auto New -> In Progress must succeed");
        let current = provider.status_next(number).unwrap().current.name;
        assert_eq!(current, "In Progress", "bare auto must take first-allowed");

        let second = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: String::new(),
            },
        );
        assert_eq!(second, 0, "bare auto In Progress -> In Review must succeed");
        let current = provider.status_next(number).unwrap().current.name;
        assert_eq!(current, "In Review");

        // Orchestrator-only guard covers the sentinel path too.
        let denied = execute_status(
            Some(Role::Executor),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: String::new(),
            },
        );
        assert_eq!(denied, 3, "bare auto must stay orchestrator-only");

        drop(_ledger_guard);
        drop(_guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bare_transition_on_terminal_status_fails_without_write() {
        // A bare auto on a terminal status (Closed) has no
        // first-allowed target: it must fail with exit 1 and leave
        // the remote status untouched (no PUT, no close climb).
        let _lock = lock_workflow_tests();
        let (dir, db) = tmp_db("bare-terminal");
        let _guard = EnvGuard::set("PHASEGENT_LOCAL_DB_PATH", db.to_str().unwrap());
        let ledger = dir.join("phasegent-ledger.sqlite3");
        let _ledger_guard = EnvGuard::set("PHASEGENT_DB_PATH", ledger.to_str().unwrap());
        let provider = LocalProvider::open().unwrap();
        let number = provider
            .create_issue("BareTerminal", "body")
            .unwrap()
            .number;
        // Walk the canonical path to Closed: New -> In Progress ->
        // In Review -> Resolved -> Closed (direct New -> Closed is a
        // policy Forbidden, so the setup must follow allowed edges).
        for target in ["In Progress", "In Review", "Resolved", "Closed"] {
            assert_eq!(
                execute_status(
                    Some(Role::Orchestrator),
                    Some(ProviderKind::Local),
                    None,
                    None,
                    None,
                    None,
                    StatusCommand::Advance {
                        number,
                        status: target.to_owned(),
                    },
                ),
                0,
                "setup advance to {target} must succeed"
            );
        }
        let exit = execute_status(
            Some(Role::Orchestrator),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            StatusCommand::Advance {
                number,
                status: String::new(),
            },
        );
        assert_eq!(exit, 1, "bare auto on Closed must fail without a route");
        let current = provider.status_next(number).unwrap().current.name;
        assert_eq!(current, "Closed", "terminal bare must not write");

        drop(_ledger_guard);
        drop(_guard);
        let _ = std::fs::remove_dir_all(dir);
    }
}

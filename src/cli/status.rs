use crate::command::StatusCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{
    IssueProvider, ProviderDispatcher, ProviderKind, RedmineMetadataProvider, RedmineProvider,
};

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
        // GitLab: list is unsupported (no native status enum), but
        // `set` maps to a managed workflow label update; the
        // orchestrator-only guard above already protects it.
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
    // GitLab exposes `IssueStatusRead` so the dispatch does not bail
    // out on the capability check; the per-command branch below
    // decides whether the call is supported. Redmine still uses the
    // capability surface for the actual work.
    if !provider.supports(capability) && !matches!(provider, ProviderDispatcher::Gitlab(_)) {
        return super::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        StatusCommand::List => match provider {
            ProviderDispatcher::Gitlab(_) => {
                super::provider_error(ForgejoError::not_supported("gitlab", "issue status list"))
            }
            _ => super::print_result(provider.list_issue_statuses()),
        },
        // `next` and `advance` run the canonical policy against the
        // installation catalogue (Redmine) or the static local catalogue.
        StatusCommand::Next { number } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                super::print_result(redmine.status_next(number))
            }
            ProviderDispatcher::Local(local) => super::print_result(local.status_next(number)),
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status next",
            )),
        },
        StatusCommand::Advance { number, status } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                let result = redmine.advance_issue_status(number, &status);
                if result.is_ok() {
                    super::report_local_warnings(
                        "status advance",
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
                let result = local.advance_issue_status(number, &status);
                super::print_result(result)
            }
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status advance",
            )),
        },
        StatusCommand::Set { number, status } => match provider {
            ProviderDispatcher::Gitlab(gitlab) => {
                let result = gitlab.set_workflow_status(number, &status);
                if result.is_ok() {
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
        let dir = std::env::temp_dir().join(format!(
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

        drop(_guard);
        let _ = std::fs::remove_dir_all(dir);
    }
}

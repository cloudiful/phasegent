//! Registry occupancy conflict policy for safe switching.
//!
//! Switching the primary checkout preserves the registry ownership policy: a
//! registered directory on this path that belongs to another *still-open*
//! task, an exact other legacy scope, or a fresh foreign host occupant
//! conflicts. A backstop compares the caller's complete task or legacy
//! identity, never a bare issue number or session. A confirmed closed
//! historical task does not veto the switch, while an unresolved closure or
//! host answer keeps the checkout untouched. Our own exact association
//! re-enters idempotently. Storage or identity failures fail closed.

use crate::infra::storage::Storage;
use crate::lifecycle::IssueSwitchParams;
use crate::worktree::occupancy;
use crate::worktree::registry::{self, NewAssociation};

/// A registered association on this checkout owned by another complete task
/// or legacy identity and not confirmed closed conflicts with switching it.
/// Our own exact association re-entering is idempotent. Storage or identity
/// failures fail closed.
pub(super) fn switch_conflict(
    storage: &Storage,
    identity: &str,
    repo_key: &str,
    params: &IssueSwitchParams<'_>,
    issue_id: u64,
    branch: &str,
    current: &str,
) -> Option<String> {
    let record = match registry::record_for_path(storage, identity, params.repo_path) {
        Ok(record) => record,
        Err(error) => {
            return Some(format!(
                "linked '{branch}' but could not read the directory registry ({}); leaving \
                 '{current}' untouched",
                error.message
            ));
        }
    };
    let Some(record) = record else {
        // No registry row is not a free checkout: the connected host is asked
        // about the actual current directory before its branch is switched.
        let own_sessions: Vec<String> = params.session.map(str::to_owned).into_iter().collect();
        let protection = occupancy::unregistered_protection_reason(
            params.host_api,
            params.repo_path,
            &own_sessions,
        )?;
        return Some(format!(
            "linked '{branch}' but {protection}; leaving '{current}' untouched — isolate with \
             `phasegent worktree acquire --issue {issue_id} --isolate`"
        ));
    };
    let own = match own_association(storage, repo_key, params, issue_id) {
        Ok(own) => own,
        Err(reason) => {
            return Some(format!(
                "linked '{branch}' but could not prove the checkout's task identity ({reason}); \
                 leaving '{current}' untouched"
            ));
        }
    };
    let protection = occupancy::foreign_protection_reason(
        params.repo_path,
        &record,
        &own,
        params.foreign_state,
        params.host_api,
        &params
            .session
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>(),
    )?;
    let other_issue = record.issue_numbers().first().copied().unwrap_or_default();
    let other_session = record.sessions.first().cloned().unwrap_or_default();
    Some(format!(
        "linked '{branch}' but the checkout is registered for issue {other_issue} \
         (session '{other_session}', directory {}; {protection}); leaving '{current}' untouched — \
         isolate with `phasegent worktree acquire --issue {issue_id} --isolate`",
        record.directory_id
    ))
}

fn own_association(
    storage: &Storage,
    repo_key: &str,
    params: &IssueSwitchParams<'_>,
    issue_id: u64,
) -> Result<NewAssociation, String> {
    let project = params.scope_project.unwrap_or_default();
    let bindings = crate::task_identity::bindings_for_repo_number(storage, repo_key, issue_id)
        .map_err(|error| format!("could not read durable task identity: {error}"))?;
    let mut matching = bindings.into_iter().filter(|binding| {
        let key = binding.key();
        key.provider() == params.scope_provider
            && key.project() == project
            && key.external_number() == Some(issue_id)
    });
    match (matching.next(), matching.next()) {
        (Some(binding), None) => Ok(NewAssociation::Task {
            task_id: binding.task_id(),
            issue: issue_id,
            provider: params.scope_provider.to_owned(),
            project: project.to_owned(),
            external_id: issue_id,
        }),
        (None, _) => Ok(NewAssociation::Legacy {
            issue: issue_id,
            provider: params.scope_provider.to_owned(),
            project: project.to_owned(),
            external_id: issue_id,
        }),
        (Some(_), Some(_)) => {
            Err("more than one durable task matches this number and scope".to_owned())
        }
    }
}

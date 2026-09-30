//! Issue-branch provisioning for create flows (issue 628 P4).
//!
//! Ensures the provider-scoped issue branch exists and records its
//! durable link, shared by the default create/link/switch flow and the
//! explicit `--branch` flow. Only local Git commands run here
//! (`show-ref`, `branch`); nothing is stashed, deleted, or fetched,
//! and no command touches the network.

use crate::branch_links::{
    IssueKey, LinkParams, UnknownState, checkout_root, ensure_schema, issues_for_branch, link,
    read_origin_url, resolve_repo_key,
};
use crate::git_runner::GitRunner;
use crate::infra::storage::Storage;

/// Ensure `branch` exists, creating it from `base` when missing.
/// `show-ref` statuses other than 0/1 and spawn failures become
/// actionable reasons; nothing is stashed, deleted, or fetched.
pub(crate) fn ensure_branch(
    git: &dyn GitRunner,
    issue_id: u64,
    branch: &str,
    base: &str,
) -> Result<(), String> {
    let branch_ref = format!("refs/heads/{branch}");
    match git.run(&["show-ref", "--verify", "--quiet", &branch_ref]) {
        Ok(result) if result.status == 0 => Ok(()),
        Ok(result) if result.status != 1 => Err(format!(
            "issue {issue_id} created; could not check branch '{branch}' (exit status {}); \
             run `phasegent issue bind {issue_id}` by hand",
            result.status
        )),
        Ok(_) => match git.run(&["branch", branch, base]) {
            Ok(result) if result.status == 0 => Ok(()),
            Ok(result) => Err(format!(
                "issue {issue_id} created; git branch '{branch}' from '{base}' failed with \
                 exit status {}; run `phasegent issue bind {issue_id}` by hand",
                result.status
            )),
            Err(error) => Err(format!("local branch creation failed: {}", error.message)),
        },
        Err(error) => Err(format!("local branch check failed: {}", error.message)),
    }
}

/// True when `branch` already exists locally.
pub(crate) fn branch_exists(git: &dyn GitRunner, branch: &str) -> bool {
    let branch_ref = format!("refs/heads/{branch}");
    git.run(&["show-ref", "--verify", "--quiet", &branch_ref])
        .is_ok_and(|result| result.status == 0)
}

/// Record the durable link for an issue branch that already exists (or
/// was just created). Idempotent: re-linking the same identity is a
/// no-op. Returns a collision note when other provider/project scopes
/// already link this branch, so shared branches stay explicit instead
/// of silently colliding across scopes; same-scope sharing is normal
/// many-to-many and stays quiet. A link-read failure after a
/// successful write degrades to no note rather than failing the
/// recorded link.
pub(crate) fn record_branch_link(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
    scope_provider: &str,
    project: &str,
    issue_id: u64,
    source: &str,
) -> Result<Option<String>, String> {
    let issue_key = IssueKey::new(scope_provider, project, issue_id.to_string())
        .map_err(|error| format!("cannot scope branch link: {error}"))?;
    link(
        connection,
        &LinkParams {
            repo_key,
            branch,
            issue: &issue_key,
            issue_number: issue_id,
            source,
            now: crate::worktree::now_unix_secs().max(1),
        },
    )
    .map_err(|error| format!("cannot record branch link: {error}"))?;
    Ok(collision_note(
        connection,
        repo_key,
        branch,
        scope_provider,
        project,
        issue_id,
    ))
}

/// Warning text when `branch` is already linked under other
/// provider/project scopes, or `None` when this identity stands
/// alone. Lists up to three other scopes so the operator sees exactly
/// what the branch is shared with.
fn collision_note(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
    scope_provider: &str,
    project: &str,
    issue_id: u64,
) -> Option<String> {
    let rows = issues_for_branch(connection, repo_key, branch, false, &UnknownState).ok()?;
    let mut others: Vec<(String, String, u64)> = Vec::new();
    for entry in &rows {
        if entry.issue.provider != scope_provider || entry.issue.project != project {
            others.push((
                entry.issue.provider.clone(),
                entry.issue.project.clone(),
                entry.issue_number,
            ));
        }
    }
    others.sort_unstable();
    others.dedup();
    if others.is_empty() {
        return None;
    }
    let mut listed: Vec<String> = others
        .iter()
        .take(3)
        .map(|(provider, project, number)| format!("{provider} {project} issue {number}"))
        .collect();
    if others.len() > listed.len() {
        listed.push("…".to_owned());
    }
    Some(format!(
        "branch '{branch}' is already linked to {} ({} other scope{}); issue {issue_id} \
         added as another link — shared branches carry every linked issue",
        listed.join(", "),
        others.len(),
        if others.len() == 1 { "" } else { "s" },
    ))
}

/// Inputs for [`link_explicit_branch`]: the explicit `--branch` target
/// needs provisioning and a durable link, but never a checkout switch,
/// so there is no cleanliness/default/lease gating and no session.
pub struct ExplicitLinkParams<'a> {
    pub issue_id: u64,
    pub branch: &'a str,
    pub base: Option<&'a str>,
    pub scope_provider: &'a str,
    pub scope_project: Option<&'a str>,
    /// Explicit `--repository` scoping: a mismatch skips silently (this
    /// checkout is not the target).
    pub explicit_repository: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExplicitLinkOutcome {
    Linked {
        branch: String,
        issue_id: u64,
        created: bool,
        /// Set when other provider/project scopes already link this
        /// branch: the link is still added (history is never replaced),
        /// but the sharing is surfaced instead of silent.
        shared: Option<String>,
    },
    Skipped {
        reason: String,
    },
    Warning {
        reason: String,
    },
    Blocked {
        reason: String,
    },
}

impl ExplicitLinkOutcome {
    /// Genuine problems and cross-scope sharing warn; deliberate skips
    /// and clean links stay silent so stdout JSON is unchanged.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Linked { shared, .. } => shared.clone().map(|note| super::super::bounded(&note)),
            Self::Warning { reason } | Self::Blocked { reason } => {
                Some(super::super::bounded(reason))
            }
            _ => None,
        }
    }
}

/// Create the explicit `--branch` target when missing and record its
/// durable provider/project-scoped link, including shared existing
/// branches (another link is added; history is never replaced). Never
/// switches the checkout and never touches local Git config. Best-effort:
/// no outcome fails the remote create that already succeeded.
pub fn link_explicit_branch(
    git: &dyn GitRunner,
    params: &ExplicitLinkParams<'_>,
) -> ExplicitLinkOutcome {
    let issue_id = params.issue_id;
    let branch = params.branch.trim();
    if issue_id == 0 {
        return ExplicitLinkOutcome::Warning {
            reason: "cannot link explicit branch: issue id must be greater than zero".to_owned(),
        };
    }
    if branch.is_empty() {
        return ExplicitLinkOutcome::Warning {
            reason: format!("cannot link issue {issue_id}: explicit branch name is empty"),
        };
    }
    if params.scope_provider.trim().is_empty() {
        return ExplicitLinkOutcome::Warning {
            reason: format!("cannot link issue {issue_id}: provider scope is empty"),
        };
    }
    let project = match params
        .scope_project
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(project) => project,
        // Same no-guess rule as the default path: without a project
        // scope the link step stops with guidance.
        None => {
            return ExplicitLinkOutcome::Blocked {
                reason: format!(
                    "cannot link issue {issue_id}: project scope is unknown; \
                     re-run create with an explicit project scope, or link by hand with \
                     `phasegent issue bind {issue_id}`"
                ),
            };
        }
    };
    if let Err(reason) = super::super::current_checkout_matches(git, params.explicit_repository) {
        return ExplicitLinkOutcome::Skipped { reason };
    }
    // Protected branches (the detected default, or a conventional
    // main/master with unknown cached HEAD) are never issue branches.
    // The CLI arm applies the same rule before calling this helper, and
    // the helper defends direct use alike.
    if let Err(reason) = crate::branch_links::identity::validate_not_protected_branch(
        branch,
        crate::branch_links::detect_default_branch(git).as_deref(),
    ) {
        return ExplicitLinkOutcome::Warning { reason };
    }
    let repo_key = match resolve_repo_key(read_origin_url(git).as_deref(), &checkout_root(git)) {
        Ok(resolved) => resolved.key,
        Err(error) => {
            return ExplicitLinkOutcome::Warning {
                reason: format!("cannot scope branch link: {error}"),
            };
        }
    };
    let storage = match Storage::open() {
        Ok(storage) => storage,
        Err(error) => {
            return ExplicitLinkOutcome::Warning {
                reason: format!("cannot record branch link: {error}"),
            };
        }
    };
    if let Err(error) = ensure_schema(&storage.connection) {
        return ExplicitLinkOutcome::Warning {
            reason: format!("cannot initialise branch link table: {error}"),
        };
    }
    let existed = branch_exists(git, branch);
    let base = params
        .base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("HEAD");
    if let Err(reason) = ensure_branch(git, issue_id, branch, base) {
        return ExplicitLinkOutcome::Warning { reason };
    }
    match record_branch_link(
        &storage.connection,
        &repo_key,
        branch,
        params.scope_provider,
        project,
        issue_id,
        "issue-create-explicit",
    ) {
        Ok(shared) => ExplicitLinkOutcome::Linked {
            branch: branch.to_owned(),
            issue_id,
            created: !existed,
            shared,
        },
        Err(reason) => ExplicitLinkOutcome::Warning { reason },
    }
}

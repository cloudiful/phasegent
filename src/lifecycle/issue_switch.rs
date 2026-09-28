//! Safe same-checkout issue-branch create/link/switch (issue 628 P4).
//!
//! The default `issue create` path creates the provider-scoped issue
//! branch when missing, records the durable link, and switches the
//! existing primary checkout onto it only when every gate holds: the
//! tree is clean, the current branch is the detected default (or the
//! explicit base), and no conflicting active lease owns this checkout.
//! Anything unsafe stays untouched with an actionable warning; dirty,
//! unknown, detached, and foreign-active states never stash, discard,
//! delete, or switch. Only local Git commands run here (`show-ref`,
//! `branch`, `checkout`, plus the read-only probes); default-branch
//! detection stays cached-only and no command touches the network.

use std::path::Path;

use crate::branch_context::GitRunner;
use crate::branch_links::{
    checkout_root, detect_default_branch, ensure_schema, read_origin_url, resolve_repo_key,
};
use crate::infra::storage::Storage;
use crate::worktree::{WorktreeRunner, is_clean};

#[path = "issue_switch/branch_name.rs"]
mod branch_name;
#[path = "issue_switch/switch_policy.rs"]
mod switch_policy;

// Explicit-branch provisioning is owned by the child module; the names
// stay reachable through this module for the lifecycle boundary.
pub use branch_name::{ExplicitLinkOutcome, ExplicitLinkParams, link_explicit_branch};

/// Inputs for [`create_link_and_switch`], bundled to stay under the
/// argument-count lint.
pub struct IssueSwitchParams<'a> {
    pub repo_path: &'a Path,
    pub issue_id: u64,
    pub branch: &'a str,
    pub base: Option<&'a str>,
    pub scope_provider: &'a str,
    pub scope_project: Option<&'a str>,
    /// Explicit `--repository` scoping: a mismatch skips silently like
    /// the legacy create-bind path (this checkout is not the target).
    pub explicit_repository: Option<&'a str>,
    /// Session booking the switch; an active lease under the same
    /// `(issue, session)` triple is ours, anything else live in the
    /// repository conflicts (repo-wide policy, mirroring acquire).
    pub session: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateSwitchOutcome {
    Switched {
        branch: String,
        issue_id: u64,
    },
    LinkedOnly {
        branch: String,
        issue_id: u64,
        reason: String,
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

impl CreateSwitchOutcome {
    /// Only genuine problems warn; deliberate skips and clean
    /// switches stay silent so stdout JSON is unchanged.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::LinkedOnly { reason, .. }
            | Self::Warning { reason }
            | Self::Blocked { reason } => Some(super::bounded(reason)),
            _ => None,
        }
    }
}

/// Create `params.branch` when missing, record its durable link, and
/// switch the checkout onto it when safe. Best-effort throughout: no
/// outcome fails the remote create that already succeeded.
pub fn create_link_and_switch(
    git: &dyn GitRunner,
    wt: &dyn WorktreeRunner,
    params: &IssueSwitchParams<'_>,
) -> CreateSwitchOutcome {
    let issue_id = params.issue_id;
    let branch = params.branch.trim();
    if issue_id == 0 {
        return CreateSwitchOutcome::Warning {
            reason: "cannot link or switch: issue id must be greater than zero".to_owned(),
        };
    }
    if branch.is_empty() {
        return CreateSwitchOutcome::Warning {
            reason: format!("cannot link or switch issue {issue_id}: branch name is empty"),
        };
    }
    if params.scope_provider.trim().is_empty() {
        return CreateSwitchOutcome::Warning {
            reason: format!("cannot link or switch issue {issue_id}: provider scope is empty"),
        };
    }
    let project = match params
        .scope_project
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(project) => project,
        // The provider-scoped create path must name its project: stop
        // with guidance instead of silently guessing one.
        None => {
            return CreateSwitchOutcome::Blocked {
                reason: format!(
                    "cannot link or switch issue {issue_id}: project scope is unknown; \
                     re-run create with an explicit project scope, or link by hand with \
                     `phasegent issue bind {issue_id}`"
                ),
            };
        }
    };
    if let Err(reason) = super::current_checkout_matches(git, params.explicit_repository) {
        return CreateSwitchOutcome::Skipped { reason };
    }
    let current = match crate::branch_context::current_branch(git) {
        Ok(branch) => branch,
        Err(error) if error.kind == "branch" => {
            return CreateSwitchOutcome::Warning {
                reason: format!(
                    "issue {issue_id} linked nowhere: HEAD is detached; switch to a named \
                     branch and run `phasegent issue bind {issue_id}` by hand"
                ),
            };
        }
        Err(error) => {
            return CreateSwitchOutcome::Warning {
                reason: format!("cannot resolve current branch: {}", error.message),
            };
        }
    };
    let repo_key = match resolve_repo_key(read_origin_url(git).as_deref(), &checkout_root(git)) {
        Ok(resolved) => resolved.key,
        Err(error) => {
            return CreateSwitchOutcome::Warning {
                reason: format!("cannot scope branch link: {error}"),
            };
        }
    };
    let storage = match Storage::open() {
        Ok(storage) => storage,
        Err(error) => {
            return CreateSwitchOutcome::Warning {
                reason: format!("cannot record branch link: {error}"),
            };
        }
    };
    if let Err(error) = ensure_schema(&storage.connection) {
        return CreateSwitchOutcome::Warning {
            reason: format!("cannot initialise branch link table: {error}"),
        };
    }
    let base = params
        .base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("HEAD");
    if let Err(reason) = branch_name::ensure_branch(git, issue_id, branch, base) {
        return CreateSwitchOutcome::Warning { reason };
    }
    let collision = match branch_name::record_branch_link(
        &storage.connection,
        &repo_key,
        branch,
        params.scope_provider,
        project,
        issue_id,
        "issue-create",
    ) {
        Ok(collision) => collision,
        Err(reason) => {
            return CreateSwitchOutcome::Warning { reason };
        }
    };
    // A branch shared with other scopes is linked but never
    // auto-switched: the checkout stays put with an explicit note
    // instead of silently colliding across scopes.
    if let Some(note) = collision {
        return CreateSwitchOutcome::LinkedOnly {
            branch: branch.to_owned(),
            issue_id,
            reason: format!(
                "{note} — leaving the checkout untouched; run `git switch {branch}` by hand \
                 once verified"
            ),
        };
    }
    if branch == current {
        return CreateSwitchOutcome::Switched {
            branch: branch.to_owned(),
            issue_id,
        };
    }
    // Switch gates: every probe is read-only, and any failure to
    // establish safety leaves the checkout untouched.
    match is_clean(wt, params.repo_path) {
        Ok(true) => {}
        Ok(false) => {
            return CreateSwitchOutcome::LinkedOnly {
                branch: branch.to_owned(),
                issue_id,
                reason: format!(
                    "checkout is dirty; linked '{branch}' but left the checkout untouched — \
                     clean the tree (or stash by hand, never automatically) and run \
                     `git switch {branch}`, or isolate with \
                     `phasegent worktree acquire --issue {issue_id} --isolate`"
                ),
            };
        }
        Err(error) => {
            return CreateSwitchOutcome::LinkedOnly {
                branch: branch.to_owned(),
                issue_id,
                reason: format!(
                    "checkout cleanliness is unknown ({}); linked '{branch}' but left the \
                     checkout untouched — run `git switch {branch}` by hand once verified",
                    error.message
                ),
            };
        }
    }
    // Switch only from the detected default branch (cached-only
    // detection, never a network query) or from the explicit base
    // when it names the current branch. Anything else stays put.
    let on_base = params
        .base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| value == current);
    let default = detect_default_branch(git);
    if !(default.as_deref() == Some(current.as_str()) || on_base) {
        return CreateSwitchOutcome::LinkedOnly {
            branch: branch.to_owned(),
            issue_id,
            reason: match default {
                Some(default) => format!(
                    "current branch '{current}' is not the detected default branch \
                     '{default}'; linked '{branch}' but left the checkout untouched — \
                     run `git switch {branch}` by hand"
                ),
                None => format!(
                    "default branch is unknown and '{current}' is not the explicit base; \
                     linked '{branch}' but left the checkout untouched — run \
                     `git switch {branch}` by hand once verified"
                ),
            },
        };
    }
    if let Some(reason) =
        switch_policy::lease_conflict(&storage, wt, params, issue_id, branch, &current)
    {
        return CreateSwitchOutcome::LinkedOnly {
            branch: branch.to_owned(),
            issue_id,
            reason,
        };
    }
    match git.run(&["checkout", branch]) {
        Ok(result) if result.status == 0 => CreateSwitchOutcome::Switched {
            branch: branch.to_owned(),
            issue_id,
        },
        Ok(result) => CreateSwitchOutcome::Warning {
            reason: format!(
                "linked '{branch}' but checkout failed with exit status {}; run \
                 `git switch {branch}` by hand",
                result.status
            ),
        },
        Err(error) => CreateSwitchOutcome::Warning {
            reason: format!("linked '{branch}' but checkout failed: {}", error.message),
        },
    }
}

//! Provider-scoped issue-branch create/link helpers.
//!
//! Split from `cli/issue.rs` so the default create/link/switch flow and
//! the explicit `--branch` durable links live in one cohesive module.
//! Every path is best-effort: warnings go to stderr, stdout JSON stays
//! byte-identical, and the remote create never fails because of a local
//! link step.

use crate::command::BranchOption;
use crate::git_runner::ProcessGitRunner;
use crate::providers::{ProviderDispatcher, ProviderKind};

/// Inputs for [`report_create_branch_links`], bundled to stay under the
/// argument-count lint.
pub(crate) struct CreateBranchLinks<'a> {
    pub provider: &'a ProviderDispatcher,
    pub provider_kind: ProviderKind,
    pub issue_number: u64,
    pub tracker: Option<&'a str>,
    pub branch: &'a BranchOption,
    pub base: Option<&'a str>,
    pub repository: Option<&'a str>,
    pub session: Option<&'a str>,
}

/// Durable link for an explicit `--branch` target. The scope is local
/// config only (same `provider_scope` as the default path); an
/// unresolvable scope warns instead of guessing.
fn link_explicit(
    provider: &ProviderDispatcher,
    issue_number: u64,
    branch_name: &str,
    base: Option<&str>,
    repository: Option<&str>,
) -> Option<String> {
    match crate::providers::index_store::provider_scope(provider) {
        Ok(scope) => crate::lifecycle::link_explicit_branch(
            &ProcessGitRunner::new(),
            &crate::lifecycle::ExplicitLinkParams {
                issue_id: issue_number,
                branch: branch_name,
                base,
                scope_provider: &scope.source,
                scope_project: Some(&scope.project),
                explicit_repository: repository,
            },
        )
        .warning(),
        Err(error) => {
            let body = error.json();
            let detail = body
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown provider scope");
            Some(format!(
                "cannot link issue {issue_number} to its explicit branch ({detail}); \
                 link by hand with `phasegent issue bind {issue_number}`"
            ))
        }
    }
}

/// Create/link the provider-scoped issue branch for a successful
/// `issue create`, reporting every outcome as a bounded stderr warning.
/// The default path (no `--branch`) creates and links for every
/// provider kind with a known scope and switches only when safe;
/// explicit `--branch` keeps its Redmine-only contract and records the
/// durable link (protected names refuse the link).
pub(crate) fn report_create_branch_links(params: &CreateBranchLinks<'_>) {
    let CreateBranchLinks {
        provider,
        provider_kind,
        issue_number,
        tracker,
        branch,
        base,
        repository,
        session,
    } = params;
    match branch {
        BranchOption::Unset => {
            // The default path creates and links the provider-scoped
            // issue branch and switches the primary checkout onto it
            // only when safe (clean, on the detected default branch,
            // no conflicting active lease).
            let name = crate::lifecycle::branch_name_for_issue(*tracker, *issue_number);
            let repo_path =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // The scope is derived from local provider configuration
            // only and never costs a provider request; when the scope
            // cannot be established the link step warns explicitly
            // instead of silently skipping or guessing.
            match crate::providers::index_store::provider_scope(provider) {
                Ok(scope) => crate::cli::report_local_warnings(
                    "issue create",
                    crate::lifecycle::create_link_and_switch(
                        &ProcessGitRunner::in_directory(repo_path.clone()),
                        &crate::worktree::ProcessWorktreeRunner::new(),
                        &crate::lifecycle::IssueSwitchParams {
                            repo_path: &repo_path,
                            issue_id: *issue_number,
                            branch: &name,
                            base: None,
                            scope_provider: &scope.source,
                            scope_project: Some(&scope.project),
                            explicit_repository: *repository,
                            session: *session,
                        },
                    )
                    .warning(),
                ),
                Err(error) => {
                    let body = error.json();
                    let detail = body
                        .get("message")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown provider scope");
                    crate::cli::report_local_warnings(
                        "issue create",
                        Some(format!(
                            "cannot link issue {issue_number} to its default branch ({detail}); \
                             link by hand with `phasegent issue bind {issue_number}`"
                        )),
                    );
                }
            }
        }
        BranchOption::Auto if *provider_kind == ProviderKind::Redmine => {
            let name = crate::lifecycle::branch_name_for_issue(*tracker, *issue_number);
            crate::cli::report_local_warnings(
                "issue create",
                link_explicit(provider, *issue_number, &name, *base, *repository),
            );
        }
        BranchOption::Named(name) if *provider_kind == ProviderKind::Redmine => {
            let runner = ProcessGitRunner::new();
            // Protected branches (the detected default, or a
            // conventional main/master with unknown cached HEAD) are
            // never issue branches: skip the durable link with guidance.
            if let Err(reason) = crate::branch_links::identity::validate_not_protected_branch(
                name,
                crate::branch_links::detect_default_branch(&runner).as_deref(),
            ) {
                crate::cli::report_local_warnings("issue create", Some(reason));
            } else {
                crate::cli::report_local_warnings(
                    "issue create",
                    link_explicit(provider, *issue_number, name, *base, *repository),
                );
            }
        }
        _ => {
            crate::cli::report_local_warnings(
                "issue create",
                Some("issue create --branch is Redmine-only; skipping branch creation".to_owned()),
            );
        }
    }
}

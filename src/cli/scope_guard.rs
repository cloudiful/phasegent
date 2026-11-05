use crate::branch_links::reads::branches_for_number;
use crate::branch_links::{UnknownState, issues_for_branch};
use crate::providers::api::PhasegentError;
use crate::providers::{ProviderDispatcher, ProviderKind};

pub(crate) fn verify_status_scope(
    provider: &ProviderDispatcher,
    explicit: Option<ProviderKind>,
    number: u64,
) -> Result<(), PhasegentError> {
    verify_issue_scope(provider, explicit, number)
}

/// The durable pre-HTTP scope check for any numbered issue operation:
/// an omitted provider on a Forgejo-bound branch, and a same-number
/// mutation target that is Forgejo-bound or conflicts with the resolved
/// scope, are rejected before the provider resolves credentials or any
/// HTTP request runs.
pub(crate) fn verify_issue_scope(
    provider: &ProviderDispatcher,
    explicit: Option<ProviderKind>,
    number: u64,
) -> Result<(), PhasegentError> {
    let runner = crate::git_runner::ProcessGitRunner::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // A checkout outside any Git work tree cannot carry branch links, so
    // its legacy non-repository behavior is preserved. Inside a work tree a
    // branch-discovery failure is never read as "no branch".
    let inside_work_tree =
        crate::worktree::is_inside_work_tree(&crate::worktree::ProcessWorktreeRunner::new(), &cwd)
            .map_err(|error| PhasegentError::config(error.message))?;
    let branch = if inside_work_tree {
        match crate::git_runner::current_branch(&runner) {
            Ok(branch) => Some(branch),
            // Detached HEAD has no branch-scoped links; the repository
            // fallback identity below still resolves for the number check.
            Err(error) if error.kind == "branch" => None,
            Err(error) => return Err(PhasegentError::config(error.message)),
        }
    } else {
        None
    };
    let origin = crate::branch_links::read_origin_url(&runner);
    let repo = crate::branch_links::resolve_repo_key(
        origin.as_deref(),
        &crate::branch_links::checkout_root(&runner),
    )
    .map_err(PhasegentError::config)?;
    let storage = crate::infra::storage::Storage::open().map_err(PhasegentError::config)?;
    crate::branch_links::ensure_schema(&storage.connection).map_err(PhasegentError::config)?;
    let selected = match provider {
        ProviderDispatcher::Forgejo(forgejo) => Some(("forgejo", forgejo.repository_scope())),
        ProviderDispatcher::Redmine(redmine) => redmine
            .config
            .project_id
            .as_deref()
            .map(|project| ("redmine", project)),
        ProviderDispatcher::Local(_) => Some(("local", "default")),
    };
    verify_links(
        &storage.connection,
        &repo.key,
        branch.as_deref(),
        number,
        explicit,
        selected,
    )
}

fn verify_links(
    connection: &rusqlite::Connection,
    repo: &str,
    branch: Option<&str>,
    number: u64,
    explicit: Option<ProviderKind>,
    selected: Option<(&str, &str)>,
) -> Result<(), PhasegentError> {
    if let Some(branch) = branch {
        let rows = issues_for_branch(connection, repo, branch, false, &UnknownState)
            .map_err(PhasegentError::config)?;
        if explicit.is_none() && rows.iter().any(|row| row.issue.provider == "forgejo") {
            return Err(PhasegentError::config(
                "branch has a Forgejo issue binding; pass --provider forgejo and its --repository",
            ));
        }
    }
    let rows = branches_for_number(connection, repo, number, false, &UnknownState)
        .map_err(PhasegentError::config)?;
    let forgejo_rows: Vec<_> = rows
        .iter()
        .filter(|row| row.issue.provider == "forgejo")
        .collect();
    if !forgejo_rows.is_empty() {
        let matching = explicit.is_some()
            && selected.is_some_and(|(provider, project)| {
                provider == "forgejo"
                    && forgejo_rows.iter().all(|row| {
                        row.issue.project == project && row.issue.external_id == number.to_string()
                    })
            });
        if !matching {
            return Err(PhasegentError::config(
                "issue number has a Forgejo binding in another or ambiguous scope; pass the matching --provider and --repository",
            ));
        }
    } else if selected.is_some_and(|(provider, _)| provider == "forgejo") && !rows.is_empty() {
        return Err(PhasegentError::config(
            "issue number has a Forgejo binding in another or ambiguous scope; pass the matching --provider and --repository",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branch_links::{IssueKey, LinkParams, link};

    #[test]
    fn scope_guard_reports_specific_omission_and_ambiguity_messages() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::branch_links::ensure_schema(&connection).unwrap();
        let issue = IssueKey::new("forgejo", "owner/repo", "7").unwrap();
        link(
            &connection,
            &LinkParams {
                repo_key: "repo",
                branch: "feat/7",
                issue: &issue,
                issue_number: 7,
                source: "test",
                now: 1,
            },
        )
        .unwrap();
        let omitted = verify_links(
            &connection,
            "repo",
            Some("feat/7"),
            7,
            None,
            Some(("redmine", "23")),
        )
        .unwrap_err();
        assert!(
            omitted.json()["message"]
                .as_str()
                .unwrap()
                .contains("Forgejo issue binding")
        );

        let second = IssueKey::new("forgejo", "other/repo", "7").unwrap();
        link(
            &connection,
            &LinkParams {
                repo_key: "repo",
                branch: "feat/7",
                issue: &second,
                issue_number: 7,
                source: "test",
                now: 1,
            },
        )
        .unwrap();
        let ambiguous = verify_links(
            &connection,
            "repo",
            Some("feat/7"),
            7,
            Some(ProviderKind::Forgejo),
            Some(("forgejo", "owner/repo")),
        )
        .unwrap_err();
        assert!(
            ambiguous.json()["message"]
                .as_str()
                .unwrap()
                .contains("another or ambiguous scope")
        );
    }

    #[test]
    fn status_scope_rejects_omission_and_wrong_repository_before_provider_calls() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::branch_links::ensure_schema(&connection).unwrap();
        let issue = IssueKey::new("forgejo", "owner/repo", "7").unwrap();
        link(
            &connection,
            &LinkParams {
                repo_key: "repo",
                branch: "feat/7",
                issue: &issue,
                issue_number: 7,
                source: "test",
                now: 1,
            },
        )
        .unwrap();
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                None,
                Some(("redmine", "23"))
            )
            .is_err()
        );
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                Some(ProviderKind::Forgejo),
                Some(("forgejo", "other/repo"))
            )
            .is_err()
        );
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                Some(ProviderKind::Forgejo),
                Some(("forgejo", "owner/repo"))
            )
            .is_ok()
        );
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                8,
                None,
                Some(("redmine", "23"))
            )
            .is_err()
        );
        assert!(
            verify_links(
                &connection,
                "other",
                Some("feat/7"),
                7,
                None,
                Some(("redmine", "23"))
            )
            .is_ok()
        );
    }
}

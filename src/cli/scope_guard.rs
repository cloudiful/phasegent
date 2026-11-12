use crate::branch_links::reads::branches_for_number;
use crate::branch_links::{UnknownState, issues_for_branch};
use crate::cli::provider_context::ProviderContext;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::ProviderDispatcher;
use crate::providers::api::PhasegentError;
use crate::providers::{ProviderKind, forgejo::ForgejoConfig, gitlab::GitlabConfig};

pub(crate) fn verify_status_scope(
    provider: &ProviderDispatcher,
    number: u64,
) -> Result<(), PhasegentError> {
    verify_issue_scope(provider, number)
}

/// The durable pre-HTTP scope check for any numbered issue operation.
///
/// The check runs on the *resolved* provider scope, so an omitted provider, an
/// explicit one and a configured default follow one rule set. A number whose
/// branch-link rows name the resolved scope is allowed; rows that name only
/// another scope refuse with a provider-neutral message; several distinct
/// scopes are allowed only when the resolved selection matches one of them;
/// malformed evidence (a row whose external id is not this number) refuses. No
/// rows means no evidence, which keeps normal default routing. A resolved
/// provider without a provable scope (for example Redmine without a project id)
/// also keeps default routing.
pub(crate) fn verify_issue_scope(
    provider: &ProviderDispatcher,
    number: u64,
) -> Result<(), PhasegentError> {
    let (repo_key, branch) = git_scope_evidence()?;
    let Some(storage) = existing_branch_link_store()? else {
        return Ok(());
    };
    let selected = resolved_scope(provider);
    verify_links(
        &storage.connection,
        &repo_key,
        branch.as_deref(),
        number,
        selected,
    )
}

/// The read-only preflight for an unbound numbered operation (issue 1201 P12).
///
/// It runs after route resolution and before any provider constructor, so a
/// conflicting binding refuses without opening a writable store, running
/// schema or migrations, or touching credentials. The raw selection plus the
/// existing role Forgejo row provide the scope evidence; a Redmine project
/// discovered later stays unresolved and defers to the post-construction
/// guard.
pub(crate) fn preflight_issue_scope(
    role: Role,
    context: &ProviderContext,
    number: u64,
) -> Result<(), PhasegentError> {
    let (repo_key, branch) = git_scope_evidence()?;
    let Some(storage) = existing_branch_link_store()? else {
        return Ok(());
    };
    let selected = resolved_scope_read_only(role, context)?;
    let selected = selected
        .as_ref()
        .map(|(provider, project)| (*provider, project.as_str()));
    verify_links(
        &storage.connection,
        &repo_key,
        branch.as_deref(),
        number,
        selected,
    )
}

/// The git identity and current branch of the checkout, read without any
/// store or provider access.
fn git_scope_evidence() -> Result<(String, Option<String>), PhasegentError> {
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
    Ok((repo.key, branch))
}

/// The existing store only, and only when it already carries the branch-link
/// table. A missing file or an older schema is no evidence and never gets
/// created or migrated by admission.
fn existing_branch_link_store() -> Result<Option<Storage>, PhasegentError> {
    let Some(storage) = Storage::open_existing_read_only().map_err(PhasegentError::config)? else {
        return Ok(None);
    };
    if !storage
        .table_exists("branch_issue_links")
        .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    Ok(Some(storage))
}

/// The `(provider, project)` scope a raw selection resolves to using only
/// existing-file reads: the shared kind resolver plus the stored role Forgejo
/// row, validated with the same pure constructor. `None` means the selection
/// carries no pre-construction scope evidence.
pub(crate) fn resolved_scope_read_only(
    role: Role,
    context: &ProviderContext,
) -> Result<Option<(&'static str, String)>, PhasegentError> {
    let kind = crate::providers::config::resolve_kind(role, context.provider)?;
    match kind {
        ProviderKind::Forgejo => {
            // The same selection helper the provider uses, so the preflight
            // and the eventual construction can never disagree about the
            // trusted instance or the per-operation repository.
            let config = ForgejoConfig::resolve(
                role,
                context.api_base.as_deref(),
                context.repository.as_deref(),
            )?;
            Ok(Some(("forgejo", config.repository().to_owned())))
        }
        ProviderKind::Gitlab => {
            let config = GitlabConfig::resolve(
                role,
                context.api_base.as_deref(),
                context.repository.as_deref(),
            )?;
            Ok(Some(("gitlab", config.repository().to_owned())))
        }
        ProviderKind::Redmine => Ok(context
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|project| !project.is_empty())
            .map(|project| ("redmine", project.to_owned()))),
        ProviderKind::Local => Ok(Some(("local", "default".to_owned()))),
    }
}

/// The resolved `(provider, project)` scope of a provider, when it has one.
fn resolved_scope(provider: &ProviderDispatcher) -> Option<(&'static str, &str)> {
    match provider {
        ProviderDispatcher::Forgejo(forgejo) => Some(("forgejo", forgejo.repository_scope())),
        ProviderDispatcher::Gitlab(gitlab) => Some(("gitlab", gitlab.repository_scope())),
        ProviderDispatcher::Redmine(redmine) => redmine
            .config
            .project_id
            .as_deref()
            .map(|project| ("redmine", project)),
        ProviderDispatcher::Local(_) => Some(("local", "default")),
    }
}

fn verify_links(
    connection: &rusqlite::Connection,
    repo: &str,
    branch: Option<&str>,
    number: u64,
    selected: Option<(&'static str, &str)>,
) -> Result<(), PhasegentError> {
    let Some(selected) = selected else {
        return Ok(());
    };
    if let Some(branch) = branch {
        let rows = issues_for_branch(connection, repo, branch, false, &UnknownState)
            .map_err(PhasegentError::config)?;
        ensure_rows_match(rows.iter().map(|row| &row.issue), selected, "branch")?;
    }
    let rows = branches_for_number(connection, repo, number, false, &UnknownState)
        .map_err(PhasegentError::config)?;
    if let Some(row) = rows
        .iter()
        .find(|row| row.issue.external_id != number.to_string())
    {
        return Err(PhasegentError::config(format!(
            "branch link evidence for issue {number} names external id '{}'; refusing to guess its provider scope",
            row.issue.external_id
        )));
    }
    ensure_rows_match(rows.iter().map(|row| &row.issue), selected, "issue number")
}

/// Whether the selected scope matches the branch-link rows that name this
/// subject. No rows keeps default routing; any other mismatch refuses before
/// a provider read.
fn ensure_rows_match<'a>(
    issues: impl Iterator<Item = &'a crate::branch_links::IssueKey>,
    selected: (&'static str, &str),
    subject: &str,
) -> Result<(), PhasegentError> {
    let mut scopes = std::collections::BTreeSet::new();
    for issue in issues {
        scopes.insert((issue.provider.as_str(), issue.project.as_str()));
    }
    if scopes.is_empty() {
        return Ok(());
    }
    if scopes.contains(&selected) {
        return Ok(());
    }
    let known = scopes
        .iter()
        .map(|(provider, project)| format!("{provider}/{project}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(PhasegentError::config(format!(
        "{subject} has a provider binding in another scope ({known}); select the matching provider and project/repository"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branch_links::{IssueKey, LinkParams, link};

    #[test]
    fn scope_guard_reports_specific_mismatch_and_ambiguity_messages() {
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
            Some(("redmine", "23")),
        )
        .unwrap_err();
        assert!(
            omitted.json()["message"]
                .as_str()
                .unwrap()
                .contains("another scope")
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
            Some(("forgejo", "matching/repo")),
        )
        .unwrap_err();
        assert!(
            ambiguous.json()["message"]
                .as_str()
                .unwrap()
                .contains("another scope")
        );
    }

    #[test]
    fn status_scope_rejects_mismatch_and_allows_the_matching_scope() {
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
                Some(("redmine", "23"))
            )
            .is_ok()
        );
        assert!(
            verify_links(&connection, "repo", Some("feat/7"), 7, None).is_ok(),
            "no provable scope keeps default routing"
        );
    }

    #[test]
    fn matching_one_of_several_scopes_is_allowed_and_malformed_ids_refuse() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::branch_links::ensure_schema(&connection).unwrap();
        for (provider, project, number) in [("redmine", "23", 7), ("forgejo", "owner/repo", 7)] {
            link(
                &connection,
                &LinkParams {
                    repo_key: "repo",
                    branch: "feat/7",
                    issue: &IssueKey::new(provider, project, number.to_string()).unwrap(),
                    issue_number: number,
                    source: "test",
                    now: 1,
                },
            )
            .unwrap();
        }
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                Some(("forgejo", "owner/repo"))
            )
            .is_ok()
        );
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                Some(("redmine", "23"))
            )
            .is_ok()
        );
        assert!(
            verify_links(
                &connection,
                "repo",
                Some("feat/7"),
                7,
                Some(("local", "default"))
            )
            .is_err()
        );

        link(
            &connection,
            &LinkParams {
                repo_key: "repo",
                branch: "feat/9",
                issue: &IssueKey::new("redmine", "23", "8").unwrap(),
                issue_number: 9,
                source: "test",
                now: 1,
            },
        )
        .unwrap();
        let malformed = verify_links(
            &connection,
            "repo",
            Some("feat/9"),
            9,
            Some(("redmine", "23")),
        )
        .unwrap_err();
        assert!(
            malformed.json()["message"]
                .as_str()
                .unwrap()
                .contains("external id")
        );
    }

    #[test]
    fn missing_and_old_stores_yield_no_evidence_without_creation_or_migration() {
        use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

        let _lock = lock_workflow_tests();
        let root = crate::test_scratch::root().join(format!(
            "scope-guard-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("missing.sqlite3");
        {
            let _env = EnvGuard::set("PHASEGENT_DB_PATH", missing.to_str().unwrap());
            assert!(
                existing_branch_link_store().unwrap().is_none(),
                "an absent store is no evidence"
            );
            assert!(!missing.exists(), "the guard never creates the store");
        }

        let old = root.join("old.sqlite3");
        let connection = rusqlite::Connection::open(&old).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE legacy_marker (value TEXT); \
                 INSERT INTO legacy_marker VALUES ('kept');",
            )
            .unwrap();
        drop(connection);
        let tables = || -> Vec<String> {
            let connection = rusqlite::Connection::open(&old).unwrap();
            let mut statement = connection
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let before = tables();
        {
            let _env = EnvGuard::set("PHASEGENT_DB_PATH", old.to_str().unwrap());
            assert!(
                existing_branch_link_store().unwrap().is_none(),
                "an old store without branch links is no evidence"
            );
        }
        assert_eq!(tables(), before, "the guard never migrates the old store");
        assert!(!before.iter().any(|table| table == "branch_issue_links"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

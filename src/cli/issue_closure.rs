//! Injectable issue-closure evidence for the worktree finalization
//! (issues 916, 1195).
//!
//! Immediate directory cleanup needs one external fact the local lease table
//! cannot supply: whether every issue a worktree is associated with is
//! *confirmed closed*. This module is the narrow seam that answers it and
//! nothing else — it never mutates leases, directories, or provider state,
//! and it never syncs.
//!
//! Two responsibilities are composed:
//!
//! * [`LinkScopeReader`] resolves the durable branch-link scopes for a number
//!   in one repository (storage plus per-repo Git identity). A number with no
//!   links has no scope evidence; two distinct scopes make it ambiguous and it
//!   is kept.
//! * [`IssueStateReader`] reads one issue's state from the configured
//!   provider. The production reader resolves the configured provider lazily
//!   ([`crate::providers::config::resolve_kind`] plus
//!   [`crate::cli::provider_for`]) so no path that never reaches a closure
//!   check — active, missing, main-checkout — ever resolves a credential.
//!
//! The production composition follows `issue sync`'s current-configured-
//! provider model: it reads whichever provider the configuration selects, with
//! no provider override, no multi-instance guessing, and no invented issue
//! ownership. A linked scope whose provider disagrees with the configured one
//! is kept, so a known-local issue is never dispatched to Redmine or vice
//! versa. Any error, not-found, ambiguity, or mismatch is a keep, never a
//! silent "closed".
//!
//! [`CurrentIssueClosure`] adapts the same resolver to the lifecycle's narrow
//! closure seam, so `issue close` confirms the *other* issues a directory is
//! associated with before it moves a session out of it.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::closure::DirectoryIssueClosure;
use crate::policy::Role;
use crate::providers::{IssueProvider, ProviderDispatcher};

/// Confirmed closure evidence for one associated issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IssueClosure {
    /// The provider authoritatively reported a `closed` state.
    Closed,
    /// Not provably closed, with the bounded operator-facing reason.
    Keep(String),
}

impl IssueClosure {
    pub(crate) fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    pub(crate) fn keep_reason(&self) -> Option<&str> {
        match self {
            Self::Closed => None,
            Self::Keep(reason) => Some(reason.as_str()),
        }
    }
}

/// The single injectable seam the prune pass depends on.
pub(crate) trait IssueStateSource {
    /// Closure evidence for `issue` in the repository checked out at
    /// `repo_path`. Read-only; never mutates provider or local state.
    fn closure(&self, repo_path: &Path, issue: u64) -> IssueClosure;

    /// The invocation's allowed provider/repository context, when the
    /// source can prove one. A scoped directory group is only classified
    /// normally when every scoped lease matches this context exactly; the
    /// default `None` keeps every scoped group.
    fn provider_scope(&self) -> Option<(String, String)> {
        None
    }

    /// Drop any invocation-scoped memo so the next [`Self::closure`] reads the
    /// provider and scope afresh. Sources without targeted invalidation use
    /// this before a removal recheck. The default is a no-op: a source
    /// that caches nothing has nothing to invalidate.
    fn refresh(&self) {}

    /// Invalidate the freshly rebuilt target only; legacy sources may refresh all.
    fn invalidate(&self, _repo_path: &Path, _issues: &[u64]) {
        self.refresh();
    }
}

/// One resolved durable link scope for a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkScope {
    pub provider: String,
    pub project: String,
    pub external_id: String,
}

/// Reads the durable branch-link scopes for one number in one repository.
pub(crate) trait LinkScopeReader {
    fn scopes(&self, repo_path: &Path, issue: u64) -> Result<Vec<LinkScope>, String>;

    /// Drop any per-repository memo (resolved identity and reused store) so the
    /// next [`Self::scopes`] re-derives them. The default is a no-op.
    fn refresh(&self) {}

    /// Invalidate the freshly rebuilt target only; legacy sources may refresh all.
    fn invalidate(&self, _repo_path: &Path, _issues: &[u64]) {
        self.refresh();
    }
}

/// The provider's answer for one issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssueStateFact {
    pub state: String,
    pub project_id: Option<u64>,
    pub project_identifier: Option<String>,
}

/// Reads one issue's state from the configured provider.
pub(crate) trait IssueStateReader {
    /// The configured provider literal (`redmine`/`local`).
    fn provider(&self) -> Result<String, String>;
    /// The issue's state, or `Ok(None)` when the provider has no such issue.
    /// An unavailable provider is an `Err`.
    fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String>;
    /// The provider's repository/project scope when it can prove one
    /// (Forgejo reports `owner/repo`); the default has no proof.
    fn scope(&self) -> Option<(String, String)> {
        None
    }
}

/// Composes the scope check with the provider read and caches one answer per
/// `(repo_path, issue)` for the invocation.
pub(crate) struct ComposedIssueStateSource {
    links: Box<dyn LinkScopeReader>,
    provider: Box<dyn IssueStateReader>,
    cache: RefCell<HashMap<(String, u64), IssueClosure>>,
}

impl ComposedIssueStateSource {
    pub(crate) fn new(
        links: Box<dyn LinkScopeReader>,
        provider: Box<dyn IssueStateReader>,
    ) -> Self {
        Self {
            links,
            provider,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn decide(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        let configured = match self.provider.provider() {
            Ok(provider) => provider,
            Err(reason) => return IssueClosure::Keep(reason),
        };
        let scopes = match self.links.scopes(repo_path, issue) {
            Ok(scopes) => scopes,
            Err(reason) => return IssueClosure::Keep(reason),
        };
        // Distinct (provider, project) scopes: more than one is ambiguous and
        // must be kept rather than dispatched to a guessed provider.
        let distinct: BTreeSet<(String, String)> = scopes
            .iter()
            .map(|scope| (scope.provider.clone(), scope.project.clone()))
            .collect();
        if distinct.len() > 1 {
            return IssueClosure::Keep(format!(
                "issue {issue} has conflicting link scopes ({}); refusing to guess a provider",
                format_scopes(&distinct)
            ));
        }
        let scope = distinct.into_iter().next();
        if let Some((provider, _)) = &scope
            && provider != &configured
        {
            return IssueClosure::Keep(format!(
                "issue {issue} is linked to provider '{provider}' but the configured provider is \
                 '{configured}'; refusing to dispatch across providers"
            ));
        }
        if let Some(scope) = scopes
            .iter()
            .find(|scope| scope.external_id.parse::<u64>().ok() != Some(issue))
        {
            return IssueClosure::Keep(format!(
                "issue {issue} link external id '{}' does not match the lease number",
                scope.external_id
            ));
        }
        let fact = match self.provider.state(issue) {
            Ok(Some(fact)) => fact,
            Ok(None) => {
                return IssueClosure::Keep(format!(
                    "issue {issue} was not found on the configured provider"
                ));
            }
            Err(reason) => return IssueClosure::Keep(reason),
        };
        if let Some((_, project)) = &scope
            && let Some(agrees) = project_agrees(project, &fact)
            && !agrees
        {
            return IssueClosure::Keep(format!(
                "issue {issue} project does not match its link scope '{project}'"
            ));
        }
        if fact.state.eq_ignore_ascii_case("closed") {
            IssueClosure::Closed
        } else {
            IssueClosure::Keep(format!(
                "issue {issue} state '{}' is not closed",
                fact.state
            ))
        }
    }
}

/// Whether the provider's project ref agrees with the link scope's project.
/// `None` means the provider carried no project ref, so nothing contradicts
/// the scope.
fn project_agrees(scope_project: &str, fact: &IssueStateFact) -> Option<bool> {
    if fact.project_id.is_none() && fact.project_identifier.is_none() {
        return None;
    }
    let id_matches = fact
        .project_id
        .is_some_and(|id| id.to_string() == scope_project);
    let identifier_matches = fact
        .project_identifier
        .as_deref()
        .is_some_and(|identifier| identifier == scope_project);
    Some(id_matches || identifier_matches)
}

fn format_scopes(scopes: &BTreeSet<(String, String)>) -> String {
    scopes
        .iter()
        .map(|(provider, project)| format!("{provider}/{project}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl IssueStateSource for ComposedIssueStateSource {
    fn closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        let key = (repo_path.to_string_lossy().into_owned(), issue);
        if let Some(cached) = self.cache.borrow().get(&key) {
            return cached.clone();
        }
        let verdict = self.decide(repo_path, issue);
        self.cache.borrow_mut().insert(key, verdict.clone());
        verdict
    }

    fn invalidate(&self, repo_path: &Path, issues: &[u64]) {
        let repo = repo_path.to_string_lossy();
        self.cache
            .borrow_mut()
            .retain(|(path, issue), _| path != repo.as_ref() || !issues.contains(issue));
        self.links.invalidate(repo_path, issues);
    }

    fn provider_scope(&self) -> Option<(String, String)> {
        let provider = self.provider.provider().ok()?;
        self.provider
            .scope()
            .map(|(_, project)| (provider, project))
    }

    fn refresh(&self) {
        // Both halves are invalidated together: a reopened issue must re-read the
        // provider *and* the durable scope before a removal is authorized, so a
        // cached `closed` can never outlive the scan.
        self.cache.borrow_mut().clear();
        self.links.refresh();
    }
}

/// Production link-scope reader: one resolved repository identity and one
/// reopened branch-link store per repository, reused for every issue that
/// repository is asked about in one invocation.
///
/// The scope rows themselves are always re-queried through
/// `branches_for_number`; only the identity resolution (an origin-URL and
/// checkout-root Git read) and the store open are memoized. Targeted
/// invalidation retains this setup because each scope read queries the current
/// branch-link rows; [`Self::refresh`] still clears all setup.
pub(crate) struct GitLinkScopeReader {
    repos: RefCell<HashMap<String, RepoScope>>,
}

struct RepoScope {
    key: String,
    storage: Storage,
}

impl GitLinkScopeReader {
    pub(crate) fn new() -> Self {
        Self {
            repos: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve the repository identity and open the reuse store once per
    /// repository, memoizing both under the caller's `repo_path` spelling.
    fn ensure(&self, repo_path: &Path) -> Result<(), String> {
        let cache_key = repo_path.to_string_lossy().into_owned();
        if self.repos.borrow().contains_key(&cache_key) {
            return Ok(());
        }
        use crate::branch_links::{checkout_root, read_origin_url, resolve_repo_key};
        use crate::git_runner::ProcessGitRunner;

        let runner = ProcessGitRunner::in_directory(repo_path);
        let resolved =
            resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner))
                .map_err(|error| format!("could not resolve the repository identity: {error}"))?;
        let storage =
            Storage::open().map_err(|error| format!("branch link store unavailable: {error}"))?;
        crate::branch_links::ensure_schema(&storage.connection)
            .map_err(|error| format!("branch link store unavailable: {error}"))?;
        self.repos.borrow_mut().insert(
            cache_key,
            RepoScope {
                key: resolved.key,
                storage,
            },
        );
        Ok(())
    }
}

impl LinkScopeReader for GitLinkScopeReader {
    fn scopes(&self, repo_path: &Path, issue: u64) -> Result<Vec<LinkScope>, String> {
        use crate::branch_links::reads::{UnknownState, branches_for_number};

        self.ensure(repo_path)?;
        let cache_key = repo_path.to_string_lossy().into_owned();
        let repos = self.repos.borrow();
        let entry = repos
            .get(&cache_key)
            .expect("a repository memo was just ensured");
        let rows = branches_for_number(
            &entry.storage.connection,
            &entry.key,
            issue,
            false,
            &UnknownState,
        )
        .map_err(|error| format!("branch link read failed: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| LinkScope {
                provider: row.issue.provider,
                project: row.issue.project,
                external_id: row.issue.external_id,
            })
            .collect())
    }

    fn refresh(&self) {
        self.repos.borrow_mut().clear();
    }

    fn invalidate(&self, _repo_path: &Path, _issues: &[u64]) {
        // Scope rows are never cached; only identity and the open store survive.
    }
}

/// Production provider reader. The dispatcher is resolved on first use so a
/// pass with no closure-checked candidate never resolves a credential.
pub(crate) struct ConfiguredIssueStateReader {
    role: Role,
    /// The explicit selection the invocation was resolved under (a close's
    /// `--provider`, `--api-base`, `--repository`, `--project-id`,
    /// `--close-status-id`). `None` reads the configured default, as the
    /// prune pass and the legacy close do.
    context: crate::cli::provider_context::ProviderContext,
    provider: OnceCell<Result<ProviderDispatcher, String>>,
}

/// The explicit provider selection an invocation parsed, reused to resolve
/// the same dispatcher for a closure check instead of re-deriving a default.
#[derive(Clone, Debug)]
pub(crate) struct ExplicitSelection {
    pub provider_kind: crate::providers::ProviderKind,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
}

impl ConfiguredIssueStateReader {
    #[allow(dead_code)]
    pub(crate) fn new(role: Role) -> Self {
        Self {
            role,
            context: crate::cli::provider_context::ProviderContext::default(),
            provider: OnceCell::new(),
        }
    }

    pub(crate) fn with_selection(role: Role, selection: ExplicitSelection) -> Self {
        Self::with_context(
            role,
            crate::cli::provider_context::ProviderContext::from_parts(
                Some(selection.provider_kind),
                selection.api_base.as_deref(),
                selection.repository.as_deref(),
                selection.project_id.as_deref(),
                selection.close_status_id.as_deref(),
            ),
        )
    }

    pub(crate) fn with_context(
        role: Role,
        context: crate::cli::provider_context::ProviderContext,
    ) -> Self {
        Self {
            role,
            context,
            provider: OnceCell::new(),
        }
    }

    fn dispatcher(&self) -> Result<&ProviderDispatcher, String> {
        match self.provider.get_or_init(|| {
            self.context
                .resolve(self.role)
                .map_err(|error| bounded_error(&error.json()))
        }) {
            Ok(dispatcher) => Ok(dispatcher),
            Err(reason) => Err(reason.clone()),
        }
    }
}

impl IssueStateReader for ConfiguredIssueStateReader {
    fn provider(&self) -> Result<String, String> {
        self.dispatcher()
            .map(|dispatcher| dispatcher.kind().as_str().to_owned())
    }

    fn scope(&self) -> Option<(String, String)> {
        let dispatcher = self.dispatcher().ok()?;
        let scope = crate::providers::index_store::provider_scope(dispatcher).ok()?;
        Some((scope.source, scope.project))
    }

    fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String> {
        let dispatcher = self.dispatcher()?;
        match dispatcher.get_issue(issue) {
            Ok(summary) => Ok(Some(IssueStateFact {
                state: summary.state,
                project_id: summary.project.as_ref().map(|project| project.id),
                project_identifier: summary
                    .project
                    .as_ref()
                    .and_then(|project| project.identifier.clone()),
            })),
            // A missing issue is treated exactly like an error: uncertainty
            // is never closure.
            Err(error) => Err(bounded_error(&error.json())),
        }
    }
}

/// Build the production source used by the CLI executor.
#[allow(dead_code)]
pub(crate) fn configured_source(role: Role) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new(
        Box::new(GitLinkScopeReader::new()),
        Box::new(ConfiguredIssueStateReader::new(role)),
    )
}

/// The explicit-provider form used by the production prune/sync routes: the
/// invocation's raw provider selection and endpoint/repository are carried
/// into the closure source instead of being re-derived from defaults.
#[allow(dead_code)]
pub(crate) fn configured_source_scoped(
    role: Role,
    selection: ExplicitSelection,
) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new(
        Box::new(GitLinkScopeReader::new()),
        Box::new(ConfiguredIssueStateReader::with_selection(role, selection)),
    )
}

/// The full-context form: raw provider, endpoint, repository, project and
/// close-status overrides travel together into the closure reader.
pub(crate) fn configured_source_context(
    role: Role,
    context: crate::cli::provider_context::ProviderContext,
) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new(
        Box::new(GitLinkScopeReader::new()),
        Box::new(ConfiguredIssueStateReader::with_context(role, context)),
    )
}

/// The close-path adapter for the lifecycle's closure seam.
///
/// The issue being closed is closed by definition — the remote close already
/// succeeded before the cleanup ran — so it is never re-read. Every *other*
/// issue the candidate directory is associated with is confirmed through the
/// same scope-aware resolver the prune pass uses, so an open, ambiguous, or
/// mismatched issue keeps the directory.
pub(crate) struct CurrentIssueClosure {
    repo_path: PathBuf,
    source: ComposedIssueStateSource,
}

impl CurrentIssueClosure {
    #[allow(dead_code)]
    pub(crate) fn new(repo_path: &Path, role: Role) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source: configured_source(role),
        }
    }

    /// The full-context form used by an explicit legacy close: the already
    /// resolved invocation selection and endpoint/project overrides are
    /// reused for every other associated issue instead of a re-derived
    /// default.
    pub(crate) fn new_context(
        repo_path: &Path,
        role: Role,
        context: crate::cli::provider_context::ProviderContext,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source: configured_source_context(role, context),
        }
    }

    /// The explicit-provider form used by a Forgejo close: every other
    /// issue the directory is associated with is confirmed through the
    /// same resolved `(provider, repository)` the close was invoked with,
    /// never a re-derived default. A link scope that disagrees keeps.
    pub(crate) fn new_scoped(repo_path: &Path, role: Role, selection: ExplicitSelection) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source: ComposedIssueStateSource::new(
                Box::new(GitLinkScopeReader::new()),
                Box::new(ConfiguredIssueStateReader::with_selection(role, selection)),
            ),
        }
    }

    /// The adapter over an explicit resolver, for the tests that pin the
    /// adapter's own short-circuit.
    #[cfg(test)]
    pub(crate) fn with_source(repo_path: &Path, source: ComposedIssueStateSource) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source,
        }
    }
}

impl DirectoryIssueClosure for CurrentIssueClosure {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        for issue in issues {
            if *issue == current_issue {
                continue;
            }
            let verdict = self.source.closure(&self.repo_path, *issue);
            if !verdict.is_closed() {
                return Err(verdict
                    .keep_reason()
                    .unwrap_or("the issue is not confirmed closed")
                    .to_owned());
            }
        }
        Ok(())
    }

    fn refresh(&self) {
        // A movement invalidates the memo: the post-move recheck must read
        // provider and scope evidence afresh, never the pre-move answers.
        self.source.refresh();
    }
}

fn bounded_error(value: &serde_json::Value) -> String {
    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider read failed");
    crate::worktree::bounded(message)
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    #[test]
    fn targeted_link_refresh_keeps_setup_but_reads_current_rows() {
        let root = crate::test_scratch::root().join(format!(
            "prune-links-{}-{}",
            std::process::id(),
            crate::worktree::now_unix_secs()
        ));
        std::fs::create_dir_all(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let reader = GitLinkScopeReader::new();
        for name in ["one", "two"] {
            let mut storage = Storage::open_at(&root.join(format!("{name}.sqlite3"))).unwrap();
            crate::branch_links::ensure_schema(&storage.connection).unwrap();
            storage.path = root.clone();
            reader.repos.borrow_mut().insert(
                root.join(name).display().to_string(),
                RepoScope {
                    key: name.to_owned(),
                    storage,
                },
            );
        }
        let repo = root.join("one");
        assert!(reader.scopes(&repo, 7).unwrap().is_empty());
        reader.repos.borrow()[&repo.display().to_string()].storage.connection.execute(
            "INSERT INTO branch_issue_links (repo_key,branch,provider,project,external_id,issue_number,status,source,created_at,updated_at) VALUES ('one','branch','local','default','7',7,'linked','test',1,1)", [],
        ).unwrap();
        reader.invalidate(&repo, &[7]);
        assert_eq!(
            reader.repos.borrow().len(),
            2,
            "targeted invalidation must not discard either repository setup"
        );
        let scopes = reader.scopes(&repo, 7).unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].external_id, "7");
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A source that returns a fixed answer per `(repo_path, issue)` and
    /// records every question, so tests pin both the verdict and the laziness
    /// (which candidates triggered a provider read).
    pub(crate) struct ScriptedIssueStateSource {
        responses: RefCell<HashMap<(String, u64), IssueClosure>>,
        default: IssueClosure,
        asked: RefCell<Vec<(String, u64)>>,
        scope: Option<(String, String)>,
    }

    impl ScriptedIssueStateSource {
        /// Every issue defaults to `closed` unless answered otherwise.
        pub(crate) fn closed() -> Self {
            Self {
                responses: RefCell::new(HashMap::new()),
                default: IssueClosure::Closed,
                asked: RefCell::new(Vec::new()),
                scope: None,
            }
        }

        /// Every issue defaults to the given keep reason.
        pub(crate) fn keeping(reason: &str) -> Self {
            Self {
                responses: RefCell::new(HashMap::new()),
                default: IssueClosure::Keep(reason.to_owned()),
                asked: RefCell::new(Vec::new()),
                scope: None,
            }
        }

        /// Prove the invocation's provider context, as a configured Forgejo
        /// dispatcher would.
        pub(crate) fn with_provider_scope(self, provider: &str, project: &str) -> Self {
            Self {
                scope: Some((provider.to_owned(), project.to_owned())),
                ..self
            }
        }

        pub(crate) fn with_issue(self, issue: u64, closure: IssueClosure) -> Self {
            self.responses
                .borrow_mut()
                .insert((String::new(), issue), closure);
            self
        }

        pub(crate) fn asked(&self) -> Vec<(String, u64)> {
            self.asked.borrow().clone()
        }
    }

    impl IssueStateSource for ScriptedIssueStateSource {
        fn closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
            let key = (repo_path.to_string_lossy().into_owned(), issue);
            self.asked.borrow_mut().push(key.clone());
            if let Some(verdict) = self.responses.borrow().get(&key) {
                return verdict.clone();
            }
            if let Some(verdict) = self.responses.borrow().get(&(String::new(), issue)) {
                return verdict.clone();
            }
            self.default.clone()
        }

        fn provider_scope(&self) -> Option<(String, String)> {
            self.scope.clone()
        }
    }
}

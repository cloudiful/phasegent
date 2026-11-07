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
use std::collections::{BTreeSet, HashMap, HashSet};
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

    /// The explicit provider selection this source was built with, when it
    /// has one.
    ///
    /// The selection filters the durable task groups before any remote read
    /// or ownership mutation: a task it does not match keeps its directory
    /// instead of being redirected. The default is no selection, and a source
    /// without one never filters a group.
    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        None
    }

    /// Drop any invocation-scoped memo so the next [`Self::closure`] reads the
    /// provider and scope afresh. Sources without targeted invalidation use
    /// this before a removal recheck. The default is a no-op: a source
    /// that caches nothing has nothing to invalidate.
    fn refresh(&self) {}

    /// Closure evidence for the directory's own durable task.
    ///
    /// The directory's scope already proved this exact task belongs to the
    /// repository, so the read is dispatched by task id. A source without
    /// task routing answers through [`Self::closure`], exactly as before.
    fn own_task_closure(
        &self,
        repo_path: &Path,
        task: TaskScopeRef<'_>,
        issue: u64,
    ) -> IssueClosure {
        let _ = task;
        self.closure(repo_path, issue)
    }

    /// Closure evidence for one other number the directory is associated
    /// with, read through that number's own durable task when the repository
    /// holds exactly one membership for it.
    ///
    /// The default delegates to [`Self::closure`], so a source without task
    /// routing is unchanged and no global number lookup is ever attempted.
    fn associated_closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.closure(repo_path, issue)
    }

    /// Invalidate the freshly rebuilt target only; legacy sources may refresh all.
    fn invalidate(&self, _repo_path: &Path, _issues: &[u64]) {
        self.refresh();
    }

    /// Closure evidence for one number whose lease carries a provider
    /// sidecar.
    ///
    /// The prune pass knows the sidecar its own lease row proved; a source
    /// that can dispatch on that scope answers here. The default ignores the
    /// sidecar and answers through [`Self::closure`], so every existing
    /// source is unchanged.
    fn scoped_closure(
        &self,
        repo_path: &Path,
        issue: u64,
        scope: Option<&crate::worktree::LeaseScope>,
    ) -> IssueClosure {
        let _ = scope;
        self.closure(repo_path, issue)
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

/// One issue's state together with the exact owner that answered it.
///
/// A task route names the durable task's stored provider, project and
/// instance, so the caller can prove the task still matches the scope it was
/// resolved under before trusting the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskRouteFact {
    pub provider: String,
    pub project: String,
    pub instance: String,
    pub state: String,
    pub project_id: Option<u64>,
    pub project_identifier: Option<String>,
}

impl TaskRouteFact {
    fn into_fact(self) -> IssueStateFact {
        IssueStateFact {
            state: self.state,
            project_id: self.project_id,
            project_identifier: self.project_identifier,
        }
    }
}

/// The exact durable task a directory's own scope was proven to name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskScopeRef<'a> {
    pub task_id: i64,
    pub provider: &'a str,
    pub project: &'a str,
    pub instance: &'a str,
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

    /// Drop any invocation-scoped memo so the next read proves its scope
    /// afresh. The default is a no-op.
    fn refresh(&self) {}

    /// The explicit provider selection the reader was built with, when it
    /// has one. The default has no selection.
    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        None
    }

    /// Read the group's own durable task through its stored instance.
    ///
    /// The group scope already proved this task belongs to the repository, so
    /// the lookup is by task id, never by number. A reader with no
    /// task-aware dispatch refuses, so the caller keeps instead of reading
    /// the task's number from the configured provider.
    fn own_task_state(
        &self,
        _task: TaskScopeRef<'_>,
        _issue: u64,
    ) -> Result<TaskRouteFact, String> {
        Err("this source has no task-aware closure dispatch".to_owned())
    }

    /// Read one other associated number through its own durable task in this
    /// repository, when the repository holds exactly one membership for it.
    ///
    /// `Ok(None)` means the number has no durable task here, so the legacy
    /// configured read applies. `Err` means the membership is ambiguous or
    /// the task's provider is unavailable, which always keeps.
    fn associated_task_state(
        &self,
        _repo_path: &Path,
        _issue: u64,
    ) -> Result<Option<TaskRouteFact>, String> {
        Ok(None)
    }

    /// Read one number through the provider its own scope names.
    ///
    /// The prune pass resolves the provider and project from the candidate's
    /// own evidence (a durable link scope or a lease sidecar), never from the
    /// configured default. The default refuses: a source that cannot prove a
    /// provider scope never answers for one.
    fn scoped_state(
        &self,
        _repo_path: &Path,
        issue: u64,
        _provider: &str,
        _project: &str,
    ) -> Result<Option<IssueStateFact>, String> {
        Err(format!(
            "issue {issue}: this source cannot prove a provider scope"
        ))
    }

    /// The provider scope an explicit manual selection names, when the
    /// invocation carries one.
    ///
    /// A candidate whose own scope contradicts an explicit selection keeps
    /// instead of being dispatched somewhere else; a pass without an
    /// explicit selection has no such scope.
    fn explicit_scope(&self) -> Option<(String, String)> {
        None
    }

    /// Read one number that carries no scope evidence at all.
    ///
    /// Only an explicit, trusted manual selection may answer here; the
    /// default refuses so an unbound number is never dispatched to the
    /// configured default.
    fn prune_fallback_state(
        &self,
        _repo_path: &Path,
        issue: u64,
    ) -> Result<Option<IssueStateFact>, String> {
        Err(format!(
            "issue {issue}: no provider scope evidence and no explicit selection"
        ))
    }
}

/// Composes the scope check with the provider read and caches one answer per
/// `(repo_path, route, issue)` for the invocation.
pub(crate) struct ComposedIssueStateSource {
    links: Box<dyn LinkScopeReader>,
    provider: Box<dyn IssueStateReader>,
    cache: RefCell<HashMap<(String, u8, u64), IssueClosure>>,
    policy: SourcePolicy,
}

/// Which dispatch policy the composition applies.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourcePolicy {
    /// The configured provider answers an unbound number. This is the
    /// unchanged close and sync behaviour.
    Legacy,
    /// Every candidate resolves through its own proven scope: the prune
    /// pass never answers from the configured default.
    Prune,
}

/// Which owner answers one closure question.
#[derive(Clone, Copy)]
enum ClosureRoute {
    /// The configured provider answers an unbound number.
    Legacy,
    /// One issue's own durable task answers through its stored instance.
    Task,
}

const ROUTE_LEGACY: u8 = 0;
const ROUTE_OWN: u8 = 1;
const ROUTE_ASSOCIATED: u8 = 2;

impl ComposedIssueStateSource {
    pub(crate) fn new(
        links: Box<dyn LinkScopeReader>,
        provider: Box<dyn IssueStateReader>,
    ) -> Self {
        Self {
            links,
            provider,
            cache: RefCell::new(HashMap::new()),
            policy: SourcePolicy::Legacy,
        }
    }

    /// The prune composition: the reader answers through each candidate's
    /// own proven scope, never through the configured default.
    pub(crate) fn new_prune(
        links: Box<dyn LinkScopeReader>,
        provider: Box<dyn IssueStateReader>,
    ) -> Self {
        Self {
            links,
            provider,
            cache: RefCell::new(HashMap::new()),
            policy: SourcePolicy::Prune,
        }
    }

    /// The legacy verdict: the configured provider answers an unbound number.
    ///
    /// The scope checks run before the provider read, so a link that names a
    /// different provider keeps without a cross-provider request.
    fn decide(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        if self.policy == SourcePolicy::Prune {
            return self.decide_prune(repo_path, issue);
        }
        let configured = match self.provider.provider() {
            Ok(provider) => provider,
            Err(reason) => return IssueClosure::Keep(reason),
        };
        self.decide_with(
            ClosureRoute::Legacy,
            repo_path,
            issue,
            &configured,
            None,
            || self.provider.state(issue),
        )
    }

    /// The prune verdict: the reader answers through the scope the
    /// candidate's own evidence proves.
    fn decide_prune(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        let scopes = match self.links.scopes(repo_path, issue) {
            Ok(scopes) => scopes,
            Err(reason) => return IssueClosure::Keep(reason),
        };
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
        if let Some(scope) = scopes
            .iter()
            .find(|scope| scope.external_id.parse::<u64>().ok() != Some(issue))
        {
            return IssueClosure::Keep(format!(
                "issue {issue} link external id '{}' does not match the lease number",
                scope.external_id
            ));
        }
        let Some((provider, project)) = distinct.into_iter().next() else {
            return match self.provider.prune_fallback_state(repo_path, issue) {
                Ok(Some(fact)) => self.prune_verdict(issue, &None, fact),
                Ok(None) => IssueClosure::Keep(format!(
                    "issue {issue} was not found on its proven provider"
                )),
                Err(reason) => IssueClosure::Keep(reason),
            };
        };
        let fact = match self
            .provider
            .scoped_state(repo_path, issue, &provider, &project)
        {
            Ok(Some(fact)) => fact,
            Ok(None) => {
                return IssueClosure::Keep(format!(
                    "issue {issue} was not found on its proven provider"
                ));
            }
            Err(reason) => return IssueClosure::Keep(reason),
        };
        let agrees = project_agrees(&project, &fact);
        self.prune_verdict(issue, &agrees, fact)
    }

    /// The shared closing rule: the provider's own state decides, and a
    /// project that contradicts the proven scope keeps.
    fn prune_verdict(
        &self,
        issue: u64,
        agrees: &Option<bool>,
        fact: IssueStateFact,
    ) -> IssueClosure {
        if let Some(false) = agrees {
            return IssueClosure::Keep(format!(
                "issue {issue} project does not match its proven scope"
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

    #[allow(clippy::too_many_arguments)]
    fn decide_with(
        &self,
        route: ClosureRoute,
        repo_path: &Path,
        issue: u64,
        route_provider: &str,
        route_project: Option<&str>,
        read: impl FnOnce() -> Result<Option<IssueStateFact>, String>,
    ) -> IssueClosure {
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
            && provider != route_provider
        {
            return IssueClosure::Keep(format!(
                "issue {issue} is linked to provider '{provider}' but its resolved route is \
                 '{route_provider}'; refusing to dispatch across providers"
            ));
        }
        if let (ClosureRoute::Task, Some((_, scope_project))) = (route, &scope)
            && Some(scope_project.as_str()) != route_project
        {
            return IssueClosure::Keep(format!(
                "issue {issue} link scope '{scope_project}' does not match its durable task project"
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
        let fact = match read() {
            Ok(Some(fact)) => fact,
            Ok(None) => {
                return IssueClosure::Keep(format!(
                    "issue {issue} was not found on the resolved provider"
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

    /// The verdict for the group's own durable task.
    ///
    /// The task is dispatched by id, and its stored identity must still equal
    /// the exact scope the group was proven under: a task whose reference
    /// changed after the proof keeps the directory instead of being read.
    fn own_task_verdict(
        &self,
        repo_path: &Path,
        task: TaskScopeRef<'_>,
        issue: u64,
    ) -> IssueClosure {
        let read = || -> Result<Option<IssueStateFact>, String> {
            let route_fact = self.provider.own_task_state(task, issue)?;
            if route_fact.provider != task.provider
                || route_fact.project != task.project
                || route_fact.instance != task.instance
            {
                return Err(format!(
                    "issue {issue} durable task reference changed after the directory proof; \
                     refusing to read it"
                ));
            }
            Ok(Some(route_fact.into_fact()))
        };
        self.decide_with(
            ClosureRoute::Task,
            repo_path,
            issue,
            task.provider,
            Some(task.project),
            read,
        )
    }

    /// The verdict for one other number the directory is associated with.
    ///
    /// The number's own durable task answers when this repository holds a
    /// single membership for it; a number with no membership keeps the legacy
    /// configured read, never a global number lookup.
    fn associated_verdict(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        let route_fact = match self.provider.associated_task_state(repo_path, issue) {
            Ok(Some(route_fact)) => route_fact,
            Ok(None) => return self.decide(repo_path, issue),
            Err(reason) => return IssueClosure::Keep(reason),
        };
        let provider = route_fact.provider.clone();
        let project = route_fact.project.clone();
        let fact = route_fact.into_fact();
        self.decide_with(
            ClosureRoute::Task,
            repo_path,
            issue,
            &provider,
            Some(&project),
            || Ok(Some(fact)),
        )
    }

    /// One cached answer per route tag, repository and issue.
    fn cached(
        &self,
        tag: u8,
        repo_path: &Path,
        issue: u64,
        decide: impl FnOnce() -> IssueClosure,
    ) -> IssueClosure {
        let key = (repo_path.to_string_lossy().into_owned(), tag, issue);
        if let Some(cached) = self.cache.borrow().get(&key) {
            return cached.clone();
        }
        let verdict = decide();
        self.cache.borrow_mut().insert(key, verdict.clone());
        verdict
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
        self.cached(ROUTE_LEGACY, repo_path, issue, || {
            self.decide(repo_path, issue)
        })
    }

    fn own_task_closure(
        &self,
        repo_path: &Path,
        task: TaskScopeRef<'_>,
        issue: u64,
    ) -> IssueClosure {
        self.cached(ROUTE_OWN, repo_path, issue, || {
            self.own_task_verdict(repo_path, task, issue)
        })
    }

    fn associated_closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.cached(ROUTE_ASSOCIATED, repo_path, issue, || {
            self.associated_verdict(repo_path, issue)
        })
    }

    fn invalidate(&self, repo_path: &Path, issues: &[u64]) {
        let repo = repo_path.to_string_lossy();
        self.cache
            .borrow_mut()
            .retain(|(path, _, issue), _| path != repo.as_ref() || !issues.contains(issue));
        self.links.invalidate(repo_path, issues);
    }

    /// The prune sidecar verdict: the lease sidecar names the provider and
    /// project, and the reader answers through exactly that scope. The
    /// sidecar must agree with the durable link scope when one exists.
    fn scoped_closure(
        &self,
        repo_path: &Path,
        issue: u64,
        scope: Option<&crate::worktree::LeaseScope>,
    ) -> IssueClosure {
        if self.policy != SourcePolicy::Prune {
            return self.closure(repo_path, issue);
        }
        let Some(scope) = scope else {
            return self.closure(repo_path, issue);
        };
        if scope.external_id() != issue {
            return IssueClosure::Keep(format!(
                "issue {issue} lease sidecar external id '{}' does not match the lease number",
                scope.external_id()
            ));
        }
        let scopes = match self.links.scopes(repo_path, issue) {
            Ok(scopes) => scopes,
            Err(reason) => return IssueClosure::Keep(reason),
        };
        // Every returned link is checked against the canonical candidate issue
        // before any scope comparison or provider dispatch: a row that names
        // another external issue is contradictory evidence, never a match.
        if let Some(link) = scopes
            .iter()
            .find(|link| link.external_id.parse::<u64>().ok() != Some(issue))
        {
            return IssueClosure::Keep(format!(
                "issue {issue} link external id '{}' does not match the lease number",
                link.external_id
            ));
        }
        let distinct: BTreeSet<(String, String)> = scopes
            .iter()
            .map(|link| (link.provider.clone(), link.project.clone()))
            .collect();
        if distinct.len() > 1 {
            return IssueClosure::Keep(format!(
                "issue {issue} has conflicting link scopes ({}); refusing to guess a provider",
                format_scopes(&distinct)
            ));
        }
        if let Some((provider, project)) = distinct.into_iter().next()
            && (provider != scope.provider() || project != scope.project())
        {
            return IssueClosure::Keep(format!(
                "issue {issue} lease sidecar scope '{}/{}' does not match its durable link scope \
                 '{provider}/{project}'",
                scope.provider(),
                scope.project()
            ));
        }
        let fact =
            match self
                .provider
                .scoped_state(repo_path, issue, scope.provider(), scope.project())
            {
                Ok(Some(fact)) => fact,
                Ok(None) => {
                    return IssueClosure::Keep(format!(
                        "issue {issue} was not found on its proven provider"
                    ));
                }
                Err(reason) => return IssueClosure::Keep(reason),
            };
        let agrees = project_agrees(scope.project(), &fact);
        self.prune_verdict(issue, &agrees, fact)
    }

    /// The scope the pass classifies scoped groups against.
    ///
    /// The prune pass has no pass-wide default scope, so only an explicit
    /// manual selection names one.
    fn provider_scope(&self) -> Option<(String, String)> {
        if self.policy == SourcePolicy::Prune {
            return self.provider.explicit_scope();
        }
        let provider = self.provider.provider().ok()?;
        self.provider
            .scope()
            .map(|(_, project)| (provider, project))
    }

    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        self.provider.selection()
    }

    fn refresh(&self) {
        // Both halves are invalidated together: a reopened issue must re-read the
        // provider *and* the durable scope before a removal is authorized, so a
        // cached `closed` can never outlive the scan.
        self.cache.borrow_mut().clear();
        self.links.refresh();
        self.provider.refresh();
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
    /// Repositories whose physical link identity changed (or could no longer
    /// be proven) after the invocation first resolved it. The prune
    /// composition keeps every candidate of such a repository instead of
    /// silently authorizing a removal under a different origin.
    poisoned: RefCell<HashSet<String>>,
    /// The prune composition reads only an existing store: no creation, no
    /// schema, no migration. The close and sync compositions keep the
    /// write-capable open they have always used.
    read_only: bool,
}

struct RepoScope {
    key: String,
    storage: Storage,
}

impl GitLinkScopeReader {
    pub(crate) fn new() -> Self {
        Self {
            repos: RefCell::new(HashMap::new()),
            poisoned: RefCell::new(HashSet::new()),
            read_only: false,
        }
    }

    /// The prune composition: the branch-link store is opened only when it
    /// already exists with the expected table and columns. A missing, old or
    /// unreadable store reads as an unavailable scope (the caller keeps), and
    /// nothing is ever created, migrated or written.
    pub(crate) fn new_read_only() -> Self {
        Self {
            repos: RefCell::new(HashMap::new()),
            poisoned: RefCell::new(HashSet::new()),
            read_only: true,
        }
    }

    /// The physical link identity of the checkout at `repo_path`, derived the
    /// same way the store memo is keyed.
    fn link_identity(repo_path: &Path) -> Result<String, String> {
        use crate::branch_links::{checkout_root, read_origin_url, resolve_repo_key};
        use crate::git_runner::ProcessGitRunner;

        let runner = ProcessGitRunner::in_directory(repo_path);
        resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner))
            .map(|resolved| resolved.key)
            .map_err(|error| format!("could not resolve the repository identity: {error}"))
    }

    /// Resolve the repository identity and open the reuse store once per
    /// repository, memoizing both under the caller's `repo_path` spelling.
    fn ensure(&self, repo_path: &Path) -> Result<(), String> {
        let cache_key = repo_path.to_string_lossy().into_owned();
        if self.poisoned.borrow().contains(&cache_key) {
            return Err(
                "the repository link identity changed after the scan; keeping the directory"
                    .to_owned(),
            );
        }
        if self.repos.borrow().contains_key(&cache_key) {
            return Ok(());
        }
        let resolved_key = Self::link_identity(repo_path)?;
        let storage = if self.read_only {
            let storage = Storage::open_existing_read_only()
                .map_err(|error| format!("branch link store unavailable: {error}"))?
                .ok_or_else(|| "branch link store is not initialized".to_owned())?;
            require_branch_link_schema(&storage)?;
            storage
        } else {
            let storage = Storage::open()
                .map_err(|error| format!("branch link store unavailable: {error}"))?;
            crate::branch_links::ensure_schema(&storage.connection)
                .map_err(|error| format!("branch link store unavailable: {error}"))?;
            storage
        };
        self.repos.borrow_mut().insert(
            cache_key,
            RepoScope {
                key: resolved_key,
                storage,
            },
        );
        Ok(())
    }
}

/// The columns the read-only number lookup relies on. An older store without
/// them is treated as absent evidence, never migrated on a read path.
const BRANCH_LINK_COLUMNS: &[&str] = &[
    "repo_key",
    "branch",
    "provider",
    "project",
    "external_id",
    "task_id",
    "issue_number",
    "status",
    "source",
    "created_at",
    "updated_at",
    "detached_at",
    "detached_reason",
];

/// Whether an already existing store carries the branch-link table the read
/// needs, without creating or migrating anything.
fn require_branch_link_schema(storage: &Storage) -> Result<(), String> {
    const TABLE: &str = "branch_issue_links";
    if !storage
        .table_exists(TABLE)
        .map_err(|error| format!("branch link store unavailable: {error}"))?
    {
        return Err("branch link store has no branch link table".to_owned());
    }
    for column in BRANCH_LINK_COLUMNS {
        if !storage
            .table_has_column(TABLE, column)
            .map_err(|error| format!("branch link store unavailable: {error}"))?
        {
            return Err(format!(
                "branch link store is missing the '{column}' column"
            ));
        }
    }
    Ok(())
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
        self.poisoned.borrow_mut().clear();
    }

    fn invalidate(&self, repo_path: &Path, _issues: &[u64]) {
        // Only the prune composition re-proves the physical identity at the
        // final boundaries. The shared close/sync reader keeps its existing
        // no-op invalidation.
        if !self.read_only {
            return;
        }
        let cache_key = repo_path.to_string_lossy().into_owned();
        let initial = {
            let repos = self.repos.borrow();
            repos.get(&cache_key).map(|scope| scope.key.clone())
        };
        let Some(initial) = initial else {
            return;
        };
        // A changed or unprovable identity keeps every candidate of this
        // repository: the cached key is never swapped and a newly matching
        // link under another origin cannot authorize a removal.
        let still_proven = Self::link_identity(repo_path).is_ok_and(|key| key == initial);
        if !still_proven {
            self.poisoned.borrow_mut().insert(cache_key);
        }
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

    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        Some(&self.context)
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

/// The task-aware form used by the prune pass: an unbound number still reads
/// through the configured provider, while a number whose durable task is a
/// member of this repository reads through that task's own pinned instance,
/// independently of the configured default.
pub(crate) fn configured_source_task_aware(
    role: Role,
    context: crate::cli::provider_context::ProviderContext,
) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new(
        Box::new(GitLinkScopeReader::new()),
        Box::new(TaskAwareIssueStateReader::new(role, context)),
    )
}

/// The prune form: every candidate resolves through its own proven scope.
///
/// A durable task still reads through its own pinned instance, and the
/// configured default never answers for a candidate.
pub(crate) fn configured_source_prune(
    role: Role,
    context: crate::cli::provider_context::ProviderContext,
) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new_prune(
        Box::new(GitLinkScopeReader::new_read_only()),
        Box::new(TaskAwareIssueStateReader::new_prune(role, context)),
    )
}

/// The endpoint one scope names, after the caller's explicit selection and
/// the role's trusted configuration agree.
///
/// The proof is independent of any provider build: a Forgejo scope needs the
/// candidate's own physical origin to attest the trusted instance and to name
/// the scoped repository, and an explicit endpoint must equal that trusted
/// instance. A Redmine scope needs the explicit endpoint to equal the single
/// trusted configured Redmine address.
fn scoped_instance(
    role: Role,
    repo_path: &Path,
    provider: &str,
    project: &str,
    context: &crate::cli::provider_context::ProviderContext,
) -> Result<String, String> {
    match provider {
        "redmine" => {
            if context.provider == Some(crate::providers::ProviderKind::Redmine)
                && let Some(selected) = context.project_id.as_deref()
                && selected != project
            {
                return Err(format!(
                    "the explicit Redmine selection names project '{selected}', not '{project}'"
                ));
            }
            let storage = Storage::open_existing_read_only()
                .map_err(|error| format!("the local store is not available: {error}"))?
                .ok_or_else(|| "the local store is not initialized".to_owned())?;
            let trusted = crate::providers::config::RedmineConfig::resolve_with_storage(
                &storage,
                role,
                None,
                Some(project),
                None,
            )
            .map_err(|error| bounded_error(&error.json()))?;
            if let Some(explicit) = context.api_base.as_deref() {
                let selected = crate::remote::normalize_redmine_api_base(explicit)
                    .map_err(|error| crate::worktree::bounded(&error))?;
                if selected != trusted.api_base {
                    return Err(
                        "the selected Redmine endpoint does not match the role's trusted instance"
                            .to_owned(),
                    );
                }
            }
            Ok(trusted.api_base)
        }
        "forgejo" => {
            let storage = Storage::open_existing_read_only()
                .map_err(|error| format!("the local store is not available: {error}"))?
                .ok_or_else(|| "the local store is not initialized".to_owned())?;
            let stored = storage
                .load_forgejo_config(role)
                .map_err(|error| format!("could not read the Forgejo role instance: {error}"))?
                .ok_or_else(|| {
                    "the Forgejo role instance is not configured for this role".to_owned()
                })?;
            let trusted = crate::providers::forgejo::ForgejoConfig::new(&stored.api_base, project)
                .map_err(|error| bounded_error(&error.json()))?;
            if let Some(explicit) = context.api_base.as_deref() {
                let selected = crate::providers::forgejo::ForgejoConfig::new(explicit, project)
                    .map_err(|error| bounded_error(&error.json()))?;
                if selected.api_base() != trusted.api_base() {
                    return Err(
                        "the selected Forgejo endpoint does not match the role's trusted instance"
                            .to_owned(),
                    );
                }
            }
            let origin = crate::remote::forgejo_origin_evidence_at(repo_path)?;
            let attested = crate::providers::forgejo::ForgejoConfig::attest_origin_repository(
                trusted.api_base(),
                &origin,
            )
            .map_err(|error| bounded_error(&error.json()))?;
            if attested != project {
                return Err(format!(
                    "the physical origin repository '{attested}' does not attest the scoped \
                     repository '{project}'"
                ));
            }
            Ok(trusted.api_base().to_owned())
        }
        "local" => {
            if project != "default" {
                return Err(format!(
                    "the local provider owns the 'default' project, not '{project}'"
                ));
            }
            Ok("default".to_owned())
        }
        other => Err(format!("unsupported provider scope '{other}'")),
    }
}

/// Build the provider one proven scope names, read-only.
///
/// The construction never creates or migrates a store, never writes a
/// credential and never resolves a default provider: the scope names the
/// provider and project, and the role's own credential proves the instance.
fn scoped_provider(
    role: Role,
    repo_path: &Path,
    provider: &str,
    project: &str,
    context: &crate::cli::provider_context::ProviderContext,
) -> Result<ProviderDispatcher, String> {
    let instance = scoped_instance(role, repo_path, provider, project, context)?;
    match provider {
        "redmine" => {
            let storage = Storage::open_existing_read_only()
                .map_err(|error| format!("the local store is not available: {error}"))?
                .ok_or_else(|| "the local store is not initialized".to_owned())?;
            let config = crate::providers::config::RedmineConfig::resolve_with_storage(
                &storage,
                role,
                Some(&instance),
                Some(project),
                None,
            )
            .map_err(|error| bounded_error(&error.json()))?;
            let api_key = crate::auth::redmine_api_key(role, &storage)?;
            crate::providers::config::RedmineProvider::new(config, api_key)
                .map(ProviderDispatcher::Redmine)
                .map_err(|error| bounded_error(&error.json()))
        }
        "forgejo" => {
            let storage = Storage::open_existing_read_only()
                .map_err(|error| format!("the local store is not available: {error}"))?
                .ok_or_else(|| "the local store is not initialized".to_owned())?;
            let token = storage
                .load_credential(role, "forgejo")
                .map_err(|error| format!("could not read the Forgejo role credential: {error}"))?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "the Forgejo role credential is not configured".to_owned())?;
            let config = crate::providers::forgejo::ForgejoConfig::new(&instance, project)
                .map_err(|error| bounded_error(&error.json()))?;
            crate::providers::forgejo::ForgejoProvider::new(config, &token)
                .map(ProviderDispatcher::Forgejo)
                .map_err(|error| bounded_error(&error.json()))
        }
        "local" => crate::providers::local::LocalProvider::open_read_only()
            .map(ProviderDispatcher::local)
            .map_err(|error| bounded_error(&error.json())),
        other => Err(format!("unsupported provider scope '{other}'")),
    }
}

/// The production task-aware reader.
///
/// Every durable task is dispatched through
/// [`crate::cli::task_route::pinned_provider_for_task`]: the task's own stored
/// instance decides the endpoint and the role credential is only used when
/// the configured provenance matches it. The group's own issue is looked up by
/// task id, never by number, while an associated number is resolved by
/// membership only — there is no global number fallback, and several
/// same-number tasks keep instead of guessing.
pub(crate) struct TaskAwareIssueStateReader {
    configured: ConfiguredIssueStateReader,
    role: Role,
    context: crate::cli::provider_context::ProviderContext,
    store: OnceCell<Result<Storage, String>>,
    repo_keys: RefCell<HashMap<String, Result<String, String>>>,
    scoped_providers: RefCell<HashMap<(String, String, String, String), ProviderDispatcher>>,
    /// The prune composition reads a pinned Local task through the existing
    /// read-only store opener: an absent store keeps instead of being created,
    /// seeded, or migrated. Every close/sync caller keeps the write-capable
    /// provider path.
    local_read_only: bool,
}

impl TaskAwareIssueStateReader {
    pub(crate) fn new(role: Role, context: crate::cli::provider_context::ProviderContext) -> Self {
        Self {
            configured: ConfiguredIssueStateReader::with_context(role, context.clone()),
            role,
            context,
            store: OnceCell::new(),
            repo_keys: RefCell::new(HashMap::new()),
            scoped_providers: RefCell::new(HashMap::new()),
            local_read_only: false,
        }
    }

    /// The prune reader: the only difference from [`Self::new`] is that a
    /// pinned Local task opens an existing store read-only instead of
    /// creating or seeding one.
    pub(crate) fn new_prune(
        role: Role,
        context: crate::cli::provider_context::ProviderContext,
    ) -> Self {
        Self {
            local_read_only: true,
            ..Self::new(role, context)
        }
    }

    /// The shared store the durable memberships live in, opened once.
    fn connection(&self) -> Result<&rusqlite::Connection, String> {
        let store = self
            .store
            .get_or_init(|| match Storage::open_existing_read_only() {
                Ok(Some(store)) => Ok(store),
                Ok(None) => Err("the local store is not initialized".to_owned()),
                Err(reason) => Err(reason),
            });
        match store {
            Ok(store) => Ok(&store.connection),
            Err(reason) => Err(reason.clone()),
        }
    }

    /// The verified durable key of the repository checked out at `repo_path`.
    fn repo_key(&self, repo_path: &Path) -> Result<String, String> {
        let cache_key = repo_path.to_string_lossy().into_owned();
        if let Some(known) = self.repo_keys.borrow().get(&cache_key) {
            return known.clone();
        }
        let resolved = crate::branch_links::resolve_checkout_repo_key(
            &crate::git_runner::ProcessGitRunner::in_directory(repo_path),
        );
        self.repo_keys
            .borrow_mut()
            .insert(cache_key, resolved.clone());
        resolved
    }

    /// The repository the candidate's own physical origin names, proven
    /// against the role's trusted Forgejo instance.
    fn forgejo_origin_repository(&self, repo_path: &Path) -> Result<String, String> {
        let storage = Storage::open_existing_read_only()
            .map_err(|error| format!("the local store is not available: {error}"))?
            .ok_or_else(|| "the local store is not initialized".to_owned())?;
        let stored = storage
            .load_forgejo_config(self.role)
            .map_err(|error| format!("could not read the Forgejo role instance: {error}"))?
            .ok_or_else(|| {
                "the Forgejo role instance is not configured for this role".to_owned()
            })?;
        let origin = crate::remote::forgejo_origin_evidence_at(repo_path)?;
        crate::providers::forgejo::ForgejoConfig::attest_origin_repository(
            &stored.api_base,
            &origin,
        )
        .map_err(|error| bounded_error(&error.json()))
    }

    /// Read one durable task's issue through its pinned instance.
    ///
    /// The binding is re-read on the shared connection and compared with the
    /// exact scope the caller proved, before the provider is built: a task
    /// whose reference changed after the proof keeps without a request.
    fn read_task(
        &self,
        connection: &rusqlite::Connection,
        task: TaskScopeRef<'_>,
        issue: u64,
    ) -> Result<TaskRouteFact, String> {
        let binding = crate::task_identity::binding_for_task_id_on(connection, task.task_id)
            .map_err(|reason| format!("issue {issue}: {reason}"))?
            .ok_or_else(|| {
                format!(
                    "issue {issue}: durable task {} has no binding",
                    task.task_id
                )
            })?;
        let key = binding.key();
        if key.provider() != task.provider
            || key.project() != task.project
            || key.instance() != task.instance
        {
            return Err(format!(
                "issue {issue} durable task reference changed after the directory proof; \
                 refusing to read it"
            ));
        }
        let provider = if self.local_read_only {
            crate::cli::task_route::provider_for_binding_read_only(
                self.role,
                &binding,
                &self.context,
            )
        } else {
            crate::cli::task_route::provider_for_binding(self.role, &binding, &self.context)
        }
        .map_err(|error| {
            format!(
                "issue {issue}: the durable task provider is unavailable: {}",
                bounded_error(&error.json())
            )
        })?;
        let summary = provider
            .get_issue(issue)
            .map_err(|error| bounded_error(&error.json()))?;
        Ok(TaskRouteFact {
            provider: key.provider().to_owned(),
            project: key.project().to_owned(),
            instance: key.instance().to_owned(),
            state: summary.state,
            project_id: summary.project.as_ref().map(|project| project.id),
            project_identifier: summary
                .project
                .as_ref()
                .and_then(|project| project.identifier.clone()),
        })
    }
}

impl IssueStateReader for TaskAwareIssueStateReader {
    fn provider(&self) -> Result<String, String> {
        self.configured.provider()
    }

    fn refresh(&self) {
        self.scoped_providers.borrow_mut().clear();
    }

    fn scope(&self) -> Option<(String, String)> {
        self.configured.scope()
    }

    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        Some(&self.context)
    }

    fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String> {
        self.configured.state(issue)
    }

    fn own_task_state(&self, task: TaskScopeRef<'_>, issue: u64) -> Result<TaskRouteFact, String> {
        let connection = self.connection()?;
        self.read_task(connection, task, issue)
    }

    fn associated_task_state(
        &self,
        repo_path: &Path,
        issue: u64,
    ) -> Result<Option<TaskRouteFact>, String> {
        let repo_key = self.repo_key(repo_path)?;
        let connection = self.connection()?;
        let tasks = crate::cli::task_route::resolve_number_in_repo(connection, &repo_key, issue)
            .map_err(|error| {
                format!(
                    "issue {issue}: the durable task lookup failed: {}",
                    bounded_error(&error.json())
                )
            })?;
        match tasks.len() {
            0 => Ok(None),
            1 => {
                let key = tasks[0].key();
                self.read_task(
                    connection,
                    TaskScopeRef {
                        task_id: tasks[0].task_id(),
                        provider: key.provider(),
                        project: key.project(),
                        instance: key.instance(),
                    },
                    issue,
                )
                .map(Some)
            }
            _ => Err(format!(
                "issue {issue} has several durable tasks in this repository; refusing to guess \
                 a provider"
            )),
        }
    }

    /// The scope an explicit selection names, when the invocation has one.
    fn explicit_scope(&self) -> Option<(String, String)> {
        let kind = self.context.provider?;
        let project = match kind {
            crate::providers::ProviderKind::Forgejo => self.context.repository.clone()?,
            crate::providers::ProviderKind::Redmine => self.context.project_id.clone()?,
            crate::providers::ProviderKind::Local => "default".to_owned(),
        };
        Some((kind.as_str().to_owned(), project))
    }

    /// Read one number through the provider its own scope names.
    ///
    /// The scope is the only authority: the provider is built read-only for
    /// exactly that provider and project, and the candidate's own physical
    /// repository proves a Forgejo instance.
    fn scoped_state(
        &self,
        repo_path: &Path,
        issue: u64,
        provider: &str,
        project: &str,
    ) -> Result<Option<IssueStateFact>, String> {
        // The scope proof runs on every read, so a cached dispatcher never
        // carries a stale origin, endpoint or selection into a verdict.
        let instance = scoped_instance(self.role, repo_path, provider, project, &self.context)
            .map_err(|reason| format!("issue {issue}: {reason}"))?;
        let key = (
            provider.to_owned(),
            project.to_owned(),
            repo_path.to_string_lossy().into_owned(),
            instance,
        );
        if !self.scoped_providers.borrow().contains_key(&key) {
            let dispatcher =
                scoped_provider(self.role, repo_path, provider, project, &self.context)
                    .map_err(|reason| format!("issue {issue}: {reason}"))?;
            self.scoped_providers
                .borrow_mut()
                .insert(key.clone(), dispatcher);
        }
        let summary = self
            .scoped_providers
            .borrow()
            .get(&key)
            .expect("the scoped provider was just cached")
            .get_issue(issue)
            .map_err(|error| format!("issue {issue}: {}", bounded_error(&error.json())))?;
        Ok(Some(IssueStateFact {
            state: summary.state,
            project_id: summary.project.as_ref().map(|project| project.id),
            project_identifier: summary
                .project
                .as_ref()
                .and_then(|project| project.identifier.clone()),
        }))
    }

    /// A number without scope evidence is answered only by an explicit,
    /// trusted manual selection.
    fn prune_fallback_state(
        &self,
        repo_path: &Path,
        issue: u64,
    ) -> Result<Option<IssueStateFact>, String> {
        let Some(kind) = self.context.provider else {
            return Err(format!(
                "issue {issue}: no link or sidecar scope proves a provider, and no explicit \
                 provider was selected"
            ));
        };
        match kind {
            crate::providers::ProviderKind::Redmine => {
                let Some(project) = self.context.project_id.clone() else {
                    return Err(format!(
                        "issue {issue}: an explicit Redmine selection needs --project-id to prove \
                         which project owns the legacy number"
                    ));
                };
                let dispatcher =
                    scoped_provider(self.role, repo_path, "redmine", &project, &self.context)
                        .map_err(|reason| format!("issue {issue}: {reason}"))?;
                let summary = dispatcher
                    .get_issue(issue)
                    .map_err(|error| format!("issue {issue}: {}", bounded_error(&error.json())))?;
                let fact = IssueStateFact {
                    state: summary.state,
                    project_id: summary.project.as_ref().map(|project| project.id),
                    project_identifier: summary
                        .project
                        .as_ref()
                        .and_then(|project| project.identifier.clone()),
                };
                if let Some(false) = project_agrees(&project, &fact) {
                    return Err(format!(
                        "issue {issue} project does not match the selected Redmine project \
                         '{project}'"
                    ));
                }
                Ok(Some(fact))
            }
            crate::providers::ProviderKind::Local => {
                let dispatcher =
                    scoped_provider(self.role, repo_path, "local", "default", &self.context)
                        .map_err(|reason| format!("issue {issue}: {reason}"))?;
                let summary = dispatcher
                    .get_issue(issue)
                    .map_err(|error| format!("issue {issue}: {}", bounded_error(&error.json())))?;
                Ok(Some(IssueStateFact {
                    state: summary.state,
                    project_id: summary.project.as_ref().map(|project| project.id),
                    project_identifier: summary
                        .project
                        .as_ref()
                        .and_then(|project| project.identifier.clone()),
                }))
            }
            crate::providers::ProviderKind::Forgejo => {
                // A Forgejo legacy number still needs the physical origin proof.
                let project = self.forgejo_origin_repository(repo_path)?;
                if let Some(selected) = self.context.repository.as_deref()
                    && selected != project
                {
                    return Err(format!(
                        "issue {issue}: the selected repository '{selected}' does not match the \
                         physical origin repository '{project}'"
                    ));
                }
                let dispatcher =
                    scoped_provider(self.role, repo_path, "forgejo", &project, &self.context)
                        .map_err(|reason| format!("issue {issue}: {reason}"))?;
                let summary = dispatcher
                    .get_issue(issue)
                    .map_err(|error| format!("issue {issue}: {}", bounded_error(&error.json())))?;
                Ok(Some(IssueStateFact {
                    state: summary.state,
                    project_id: summary.project.as_ref().map(|project| project.id),
                    project_identifier: summary
                        .project
                        .as_ref()
                        .and_then(|project| project.identifier.clone()),
                }))
            }
        }
    }
}

/// Which resolver answers the associated numbers of a close.
#[derive(Clone, Copy)]
enum CurrentClosureRoute {
    /// The configured/default provider answers an unbound number.
    Configured,
    /// Every other number resolves by durable membership only and reads
    /// through its own pinned task.
    TaskAware,
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
    route: CurrentClosureRoute,
}

impl CurrentIssueClosure {
    #[allow(dead_code)]
    pub(crate) fn new(repo_path: &Path, role: Role) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source: configured_source(role),
            route: CurrentClosureRoute::Configured,
        }
    }

    /// The task-aware form used by a routed close.
    ///
    /// Every other associated number is resolved by durable membership only
    /// (never a global number lookup) and read through its own pinned task; an
    /// ambiguous membership, a moved task reference, or an unavailable
    /// instance keeps before any wrong-provider request. A number with no
    /// membership keeps the configured read.
    pub(crate) fn new_task_aware(
        repo_path: &Path,
        role: Role,
        context: crate::cli::provider_context::ProviderContext,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source: configured_source_task_aware(role, context),
            route: CurrentClosureRoute::TaskAware,
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
            route: CurrentClosureRoute::Configured,
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
            route: CurrentClosureRoute::Configured,
        }
    }

    /// The adapter over an explicit resolver, for the tests that pin the
    /// adapter's own short-circuit.
    #[cfg(test)]
    pub(crate) fn with_source(repo_path: &Path, source: ComposedIssueStateSource) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            source,
            route: CurrentClosureRoute::Configured,
        }
    }
}

impl DirectoryIssueClosure for CurrentIssueClosure {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        for issue in issues {
            if *issue == current_issue {
                continue;
            }
            let verdict = match self.route {
                CurrentClosureRoute::Configured => self.source.closure(&self.repo_path, *issue),
                CurrentClosureRoute::TaskAware => {
                    self.source.associated_closure(&self.repo_path, *issue)
                }
            };
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

    /// The prune link reader accepts only an existing store that already
    /// carries the branch-link table and the columns the read selects; it
    /// never migrates an older one.
    #[test]
    fn the_read_only_link_store_check_never_migrates() {
        let root = crate::test_scratch::root().join(format!(
            "prune-links-readonly-{}-{}",
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
        let storage = Storage::open_at(&root.join("fresh.sqlite3")).unwrap();
        let absent = require_branch_link_schema(&storage)
            .expect_err("a store without the branch link table is not usable evidence");
        assert!(absent.contains("branch link"), "got: {absent}");
        assert!(
            !storage.table_exists("branch_issue_links").unwrap(),
            "the read-only check must not create the table"
        );
        crate::branch_links::ensure_schema(&storage.connection).unwrap();
        assert!(require_branch_link_schema(&storage).is_ok());
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

        /// A sidecar that names another provider scope keeps without a
        /// provider read.
        fn scoped_closure(
            &self,
            repo_path: &Path,
            issue: u64,
            scope: Option<&crate::worktree::LeaseScope>,
        ) -> IssueClosure {
            if let (Some(scope), Some((provider, project))) = (scope, self.scope.as_ref())
                && (scope.provider() != provider || scope.project() != project)
            {
                return IssueClosure::Keep(format!(
                    "issue {issue} lease sidecar scope '{}/{}' is not the proven scope \
                     '{provider}/{project}'",
                    scope.provider(),
                    scope.project()
                ));
            }
            self.closure(repo_path, issue)
        }
    }
}

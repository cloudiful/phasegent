//! Coverage for the narrow issue-closure adapter (issue 916).
//!
//! These drive [`ComposedIssueStateSource`] directly with a scripted link
//! reader and a scripted provider reader, so every state, scope conflict, and
//! mismatch is exercised without a real provider or network. The prune pass
//! itself is covered elsewhere with a plain
//! [`ScriptedIssueStateSource`](crate::cli::issue_closure::test_support::ScriptedIssueStateSource).

use super::*;

use crate::cli::issue_closure::{
    ComposedIssueStateSource, IssueClosure, IssueStateFact, IssueStateReader, LinkScope,
    LinkScopeReader,
};
use crate::lifecycle::cleanup::closure::DirectoryIssueClosure;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Scripted link scopes keyed by issue number.
struct FakeLinks {
    scopes: Vec<(u64, LinkScope)>,
    fail: bool,
}

impl FakeLinks {
    fn none() -> Self {
        Self {
            scopes: Vec::new(),
            fail: false,
        }
    }

    fn with(mut self, issue: u64, provider: &str, project: &str, external_id: &str) -> Self {
        self.scopes.push((
            issue,
            LinkScope {
                provider: provider.to_owned(),
                project: project.to_owned(),
                external_id: external_id.to_owned(),
            },
        ));
        self
    }

    fn failing() -> Self {
        Self {
            scopes: Vec::new(),
            fail: true,
        }
    }
}

impl LinkScopeReader for FakeLinks {
    fn scopes(&self, _repo_path: &Path, issue: u64) -> Result<Vec<LinkScope>, String> {
        if self.fail {
            return Err("branch link store unavailable: scripted".to_owned());
        }
        Ok(self
            .scopes
            .iter()
            .filter(|(number, _)| *number == issue)
            .map(|(_, scope)| scope.clone())
            .collect())
    }
}

/// Scripted provider answers.
struct FakeProvider {
    provider: String,
    answers: Vec<(u64, Result<Option<IssueStateFact>, String>)>,
}

impl FakeProvider {
    fn redmine() -> Self {
        Self {
            provider: "redmine".to_owned(),
            answers: Vec::new(),
        }
    }

    fn with_state(mut self, issue: u64, state: &str) -> Self {
        self.answers
            .push((issue, Ok(Some(fact(state, None, None)))));
        self
    }

    fn with_fact(mut self, issue: u64, fact: IssueStateFact) -> Self {
        self.answers.push((issue, Ok(Some(fact))));
        self
    }

    fn missing(mut self, issue: u64) -> Self {
        self.answers.push((issue, Ok(None)));
        self
    }

    fn failing(mut self, issue: u64, reason: &str) -> Self {
        self.answers.push((issue, Err(reason.to_owned())));
        self
    }
}

impl IssueStateReader for FakeProvider {
    fn provider(&self) -> Result<String, String> {
        Ok(self.provider.clone())
    }

    fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String> {
        self.answers
            .iter()
            .find(|(number, _)| *number == issue)
            .map(|(_, answer)| answer.clone())
            .unwrap_or(Ok(None))
    }
}

fn fact(state: &str, project_id: Option<u64>, identifier: Option<&str>) -> IssueStateFact {
    IssueStateFact {
        state: state.to_owned(),
        project_id,
        project_identifier: identifier.map(str::to_owned),
    }
}

fn repo() -> PathBuf {
    PathBuf::from("/tmp/phasegent-916-issue-state")
}

fn source(links: FakeLinks, provider: FakeProvider) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new(Box::new(links), Box::new(provider))
}

#[test]
fn targeted_invalidation_preserves_unrelated_issue_and_repo_memos() {
    use std::cell::RefCell;
    use std::rc::Rc;
    struct Links(Rc<RefCell<Vec<(String, u64)>>>);
    impl LinkScopeReader for Links {
        fn scopes(&self, path: &Path, issue: u64) -> Result<Vec<LinkScope>, String> {
            self.0
                .borrow_mut()
                .push((path.display().to_string(), issue));
            Ok(Vec::new())
        }
        fn refresh(&self) {
            panic!("targeted invalidation must retain repository setup");
        }
        fn invalidate(&self, _: &Path, _: &[u64]) {}
    }
    struct Provider(Rc<RefCell<Vec<u64>>>);
    impl IssueStateReader for Provider {
        fn provider(&self) -> Result<String, String> {
            Ok("redmine".to_owned())
        }
        fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String> {
            self.0.borrow_mut().push(issue);
            Ok(Some(fact("closed", None, None)))
        }
    }
    let links = Rc::new(RefCell::new(Vec::new()));
    let reads = Rc::new(RefCell::new(Vec::new()));
    let source = ComposedIssueStateSource::new(
        Box::new(Links(links.clone())),
        Box::new(Provider(reads.clone())),
    );
    let other = Path::new("/other-repo");
    for (path, issue) in [(repo(), 7), (repo(), 8), (other.to_path_buf(), 7)] {
        assert!(source.closure(&path, issue).is_closed());
    }
    source.invalidate(&repo(), &[7]);
    assert!(source.closure(&repo(), 8).is_closed());
    assert!(source.closure(other, 7).is_closed());
    assert_eq!(reads.borrow().len(), 3);
    for _ in 0..2 {
        assert!(source.closure(&repo(), 7).is_closed());
    }
    assert_eq!(*reads.borrow(), vec![7, 8, 7, 7]);
    assert_eq!(links.borrow().len(), 4);
    source.invalidate(&repo(), &[7]);
    assert!(source.closure(&repo(), 7).is_closed());
    assert_eq!(
        reads.borrow().len(),
        5,
        "each removal rechecks its target again"
    );
}

#[test]
fn a_closed_issue_is_closed() {
    let source = source(
        FakeLinks::none(),
        FakeProvider::redmine().with_state(7, "closed"),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
}

#[test]
fn closure_is_case_insensitive() {
    let source = source(
        FakeLinks::none(),
        FakeProvider::redmine().with_state(7, "Closed"),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
}

#[test]
fn open_and_resolved_states_keep() {
    for state in ["Open", "Resolved", "In Progress"] {
        let source = source(
            FakeLinks::none(),
            FakeProvider::redmine().with_state(7, state),
        );
        let verdict = source.closure(&repo(), 7);
        assert!(!verdict.is_closed(), "state {state} must not be closed");
        assert!(
            verdict.keep_reason().unwrap().contains(state),
            "reason must name the state: {verdict:?}"
        );
    }
}

#[test]
fn a_missing_issue_keeps() {
    let source = source(FakeLinks::none(), FakeProvider::redmine().missing(7));
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(verdict.keep_reason().unwrap().contains("not found"));
}

#[test]
fn a_provider_error_keeps() {
    let source = source(
        FakeLinks::none(),
        FakeProvider::redmine().failing(7, "redmine: 500 internal error"),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(verdict.keep_reason().unwrap().contains("500"));
}

#[test]
fn a_link_scope_read_failure_keeps() {
    let source = source(
        FakeLinks::failing(),
        FakeProvider::redmine().with_state(7, "closed"),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(verdict.keep_reason().unwrap().contains("branch link"));
}

#[test]
fn an_ambiguous_link_scope_keeps() {
    let source = source(
        FakeLinks::none()
            .with(7, "redmine", "tools-phasegent", "7")
            .with(7, "redmine", "other-project", "7"),
        FakeProvider::redmine().with_state(7, "closed"),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(
        verdict.keep_reason().unwrap().contains("conflicting"),
        "{verdict:?}"
    );
}

#[test]
fn a_provider_mismatch_keeps() {
    // The link says local, the configured provider is redmine.
    let source = source(
        FakeLinks::none().with(7, "local", "default", "7"),
        FakeProvider::redmine().with_state(7, "closed"),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(
        verdict
            .keep_reason()
            .unwrap()
            .contains("refusing to dispatch"),
        "{verdict:?}"
    );
}

#[test]
fn a_matching_link_scope_is_accepted() {
    let source = source(
        FakeLinks::none().with(7, "redmine", "tools-phasegent", "7"),
        FakeProvider::redmine().with_fact(7, fact("closed", Some(23), Some("tools-phasegent"))),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
}

#[test]
fn an_external_id_mismatch_keeps() {
    let source = source(
        FakeLinks::none().with(7, "redmine", "tools-phasegent", "8"),
        FakeProvider::redmine().with_state(7, "closed"),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(
        verdict.keep_reason().unwrap().contains("external id"),
        "{verdict:?}"
    );
}

#[test]
fn a_project_mismatch_keeps() {
    let source = source(
        FakeLinks::none().with(7, "redmine", "tools-phasegent", "7"),
        FakeProvider::redmine().with_fact(7, fact("closed", Some(99), Some("other"))),
    );
    let verdict = source.closure(&repo(), 7);
    assert!(!verdict.is_closed());
    assert!(
        verdict.keep_reason().unwrap().contains("project"),
        "{verdict:?}"
    );
}

#[test]
fn answers_are_cached_per_invocation() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct CountingLinks {
        calls: Rc<Cell<usize>>,
    }
    impl LinkScopeReader for CountingLinks {
        fn scopes(&self, _repo_path: &Path, _issue: u64) -> Result<Vec<LinkScope>, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(Vec::new())
        }
    }
    let calls = Rc::new(Cell::new(0));
    let source = ComposedIssueStateSource::new(
        Box::new(CountingLinks {
            calls: Rc::clone(&calls),
        }),
        Box::new(FakeProvider::redmine().with_state(7, "closed")),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(
        calls.get(),
        1,
        "a repeated question is served from the cache"
    );
}

/// Issue 929 P1: the removal recheck refreshes the composed source, so a
/// `closed` scan answer is re-read and a reopened issue vetoes removal.
#[test]
fn refresh_re_reads_the_provider_after_a_reopen() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct TogglingProvider {
        state: Rc<RefCell<String>>,
        calls: Rc<Cell<usize>>,
    }
    impl IssueStateReader for TogglingProvider {
        fn provider(&self) -> Result<String, String> {
            Ok("redmine".to_owned())
        }
        fn state(&self, _issue: u64) -> Result<Option<IssueStateFact>, String> {
            self.calls.set(self.calls.get() + 1);
            let state = self.state.borrow().clone();
            Ok(Some(fact(&state, None, None)))
        }
    }

    let state = Rc::new(RefCell::new("closed".to_owned()));
    let calls = Rc::new(Cell::new(0));
    let source = ComposedIssueStateSource::new(
        Box::new(FakeLinks::none()),
        Box::new(TogglingProvider {
            state: Rc::clone(&state),
            calls: Rc::clone(&calls),
        }),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(calls.get(), 1, "the scan answer is cached");

    // The issue is reopened between the scan and the removal recheck. Without a
    // refresh the cached `closed` answer would still authorize the deletion.
    *state.borrow_mut() = "Open".to_owned();
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);

    // The recheck refreshes first: the provider is re-read and vetoes removal.
    source.refresh();
    let verdict = source.closure(&repo(), 7);
    assert!(
        !verdict.is_closed(),
        "a reopened issue must veto removal: {verdict:?}"
    );
    assert_eq!(calls.get(), 2, "the provider is re-read exactly once");
}

/// Issue 929 P1: a refresh also invalidates the link-scope reader, so the
/// durable scope is re-read independently of the provider answer.
#[test]
fn refresh_invalidates_the_link_scope_reader() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct CountingLinks {
        calls: Rc<Cell<usize>>,
        refreshes: Rc<Cell<usize>>,
    }
    impl LinkScopeReader for CountingLinks {
        fn scopes(&self, _repo_path: &Path, _issue: u64) -> Result<Vec<LinkScope>, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(Vec::new())
        }
        fn refresh(&self) {
            self.refreshes.set(self.refreshes.get() + 1);
        }
    }

    let calls = Rc::new(Cell::new(0));
    let refreshes = Rc::new(Cell::new(0));
    let source = ComposedIssueStateSource::new(
        Box::new(CountingLinks {
            calls: Rc::clone(&calls),
            refreshes: Rc::clone(&refreshes),
        }),
        Box::new(FakeProvider::redmine().with_state(7, "closed")),
    );
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(calls.get(), 1);
    source.refresh();
    assert_eq!(source.closure(&repo(), 7), IssueClosure::Closed);
    assert_eq!(calls.get(), 2, "the scope is re-read after a refresh");
    assert_eq!(refreshes.get(), 1, "the reader's refresh is invoked");
}

// ---------------------------------------------------------------------------
// The close-path adapter
// ---------------------------------------------------------------------------

/// The issue being closed is closed by definition: the adapter never
/// re-reads it, and every other issue is confirmed through the same
/// scope-aware resolver the prune pass uses.
#[test]
fn the_close_adapter_short_circuits_the_current_issue() {
    use crate::cli::issue_closure::CurrentIssueClosure;
    let closure = CurrentIssueClosure::with_source(
        &repo(),
        source(
            FakeLinks::none().with(7, "redmine", "tools", "7"),
            FakeProvider::redmine().with_state(7, "open"),
        ),
    );
    assert!(
        closure.confirm_others_closed(7, &[7]).is_ok(),
        "the issue the close already closed is never re-read"
    );
}

#[test]
fn the_close_adapter_keeps_an_open_neighbour() {
    use crate::cli::issue_closure::CurrentIssueClosure;
    let closure = CurrentIssueClosure::with_source(
        &repo(),
        source(
            FakeLinks::none().with(8, "redmine", "tools", "8"),
            FakeProvider::redmine().with_state(8, "open"),
        ),
    );
    let reason = closure
        .confirm_others_closed(7, &[7, 8])
        .expect_err("an open neighbour keeps the directory");
    assert!(reason.contains("8"), "got: {reason}");
}

#[test]
fn the_close_adapter_accepts_a_closed_neighbour() {
    use crate::cli::issue_closure::CurrentIssueClosure;
    let closure = CurrentIssueClosure::with_source(
        &repo(),
        source(
            FakeLinks::none().with(8, "redmine", "tools", "8"),
            FakeProvider::redmine().with_state(8, "closed"),
        ),
    );
    assert!(
        closure.confirm_others_closed(7, &[7, 8]).is_ok(),
        "a confirmed-closed neighbour no longer protects the directory"
    );
}

#[test]
fn the_close_adapter_keeps_an_ambiguous_neighbour() {
    use crate::cli::issue_closure::CurrentIssueClosure;
    let links = FakeLinks::none()
        .with(8, "redmine", "tools", "8")
        .with(8, "local", "default", "8");
    let closure = CurrentIssueClosure::with_source(
        &repo(),
        source(links, FakeProvider::redmine().with_state(8, "closed")),
    );
    let reason = closure
        .confirm_others_closed(7, &[7, 8])
        .expect_err("an ambiguous scope keeps the directory");
    assert!(reason.contains("conflicting link scopes"), "got: {reason}");
}

#[test]
fn the_close_adapter_keeps_a_neighbour_the_provider_does_not_know() {
    use crate::cli::issue_closure::CurrentIssueClosure;
    let closure = CurrentIssueClosure::with_source(
        &repo(),
        source(
            FakeLinks::none().with(8, "redmine", "tools", "8"),
            FakeProvider::redmine().missing(8),
        ),
    );
    let reason = closure
        .confirm_others_closed(7, &[7, 8])
        .expect_err("a missing issue keeps the directory");
    assert!(reason.contains("was not found"), "got: {reason}");
}

/// The prune policy: a candidate answers through the provider and project
/// its own scope names, never through the configured default.
#[derive(Clone)]
struct ScopedProvider {
    provider: String,
    closed: Rc<Vec<(String, String, u64)>>,
    asked: Rc<RefCell<Vec<(String, String, u64)>>>,
    default_asked: Rc<RefCell<Vec<u64>>>,
}

impl ScopedProvider {
    fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            closed: Rc::new(Vec::new()),
            asked: Rc::new(RefCell::new(Vec::new())),
            default_asked: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn closed_for(self, provider: &str, project: &str, issue: u64) -> Self {
        Self {
            closed: Rc::new(vec![(provider.to_owned(), project.to_owned(), issue)]),
            ..self
        }
    }

    fn asked(&self) -> Vec<(String, String, u64)> {
        self.asked.borrow().clone()
    }

    /// The numbers the default provider was asked about.
    fn default_asked(&self) -> Vec<u64> {
        self.default_asked.borrow().clone()
    }
}

impl IssueStateReader for ScopedProvider {
    fn provider(&self) -> Result<String, String> {
        Ok(self.provider.clone())
    }

    fn state(&self, issue: u64) -> Result<Option<IssueStateFact>, String> {
        self.default_asked.borrow_mut().push(issue);
        Ok(None)
    }

    fn scoped_state(
        &self,
        _repo_path: &Path,
        issue: u64,
        provider: &str,
        project: &str,
    ) -> Result<Option<IssueStateFact>, String> {
        self.asked
            .borrow_mut()
            .push((provider.to_owned(), project.to_owned(), issue));
        if self
            .closed
            .iter()
            .any(|(known, scope, number)| known == provider && scope == project && *number == issue)
        {
            return Ok(Some(fact("closed", None, None)));
        }
        Ok(None)
    }
}

fn prune_source(links: FakeLinks, provider: ScopedProvider) -> ComposedIssueStateSource {
    ComposedIssueStateSource::new_prune(Box::new(links), Box::new(provider))
}

#[test]
fn prune_dispatches_a_link_scope_to_its_own_provider() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(
        FakeLinks::none().with(7, "redmine", "23", "7"),
        provider.clone(),
    );
    assert_eq!(
        source.closure(&repo(), 7),
        IssueClosure::Closed,
        "the Redmine link scope must answer even under a Forgejo default"
    );
    assert_eq!(
        provider.asked(),
        vec![("redmine".to_owned(), "23".to_owned(), 7)],
        "only the scoped provider answers"
    );
    assert!(
        provider.default_asked().is_empty(),
        "the configured default must not answer a scoped candidate"
    );
}

#[test]
fn prune_keeps_an_ambiguous_link_scope() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(
        FakeLinks::none()
            .with(7, "redmine", "23", "7")
            .with(7, "forgejo", "owner/repo", "7"),
        provider,
    );
    let verdict = source.closure(&repo(), 7);
    assert!(matches!(verdict, IssueClosure::Keep(_)), "{verdict:?}");
}

#[test]
fn prune_keeps_a_link_external_id_mismatch() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(FakeLinks::none().with(7, "redmine", "23", "8"), provider);
    let verdict = source.closure(&repo(), 7);
    assert!(matches!(verdict, IssueClosure::Keep(_)), "{verdict:?}");
}

#[test]
fn prune_keeps_a_number_without_scope_evidence() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo");
    let source = prune_source(FakeLinks::none(), provider.clone());
    let verdict = source.closure(&repo(), 7);
    assert!(matches!(verdict, IssueClosure::Keep(_)), "{verdict:?}");
    assert!(
        provider.default_asked().is_empty(),
        "the configured default must not answer a number without scope evidence"
    );
}

#[test]
fn prune_dispatches_a_sidecar_through_its_own_scope() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(FakeLinks::none(), provider);
    let sidecar = crate::worktree::LeaseScope::new("redmine", "23", 7).unwrap();
    assert_eq!(
        source.scoped_closure(&repo(), 7, Some(&sidecar)),
        IssueClosure::Closed,
        "the lease sidecar names its own provider"
    );
}

#[test]
fn prune_keeps_a_sidecar_that_contradicts_the_link_scope() {
    use crate::cli::issue_closure::IssueStateSource;
    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(FakeLinks::none().with(7, "redmine", "23", "7"), provider);
    let sidecar = crate::worktree::LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let verdict = source.scoped_closure(&repo(), 7, Some(&sidecar));
    assert!(matches!(verdict, IssueClosure::Keep(_)), "{verdict:?}");

    let provider = ScopedProvider::new("forgejo").closed_for("redmine", "23", 7);
    let source = prune_source(
        FakeLinks::none().with(7, "redmine", "23", "8"),
        provider.clone(),
    );
    let sidecar = crate::worktree::LeaseScope::new("redmine", "23", 7).unwrap();
    let verdict = source.scoped_closure(&repo(), 7, Some(&sidecar));
    assert!(matches!(verdict, IssueClosure::Keep(_)), "{verdict:?}");
    assert!(provider.asked().is_empty());
}

/// The prune link reader proves the physical link identity once and re-proves
/// it at the final boundaries. An origin change between the scan and the
/// removal recheck keeps the directory instead of swapping the cached key and
/// authorizing a removal under another repository identity.
#[test]
fn a_changed_link_identity_before_removal_keeps() {
    use super::support::{TempRepo, open_temp_db};
    use crate::branch_links::{
        IssueKey, LinkParams, ensure_schema as ensure_link_schema, store::link,
    };
    use crate::cli::issue_closure::GitLinkScopeReader;

    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-link-identity") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-link-identity");
    crate::worktree::ensure_schema(&storage).expect("worktree schema");
    ensure_link_schema(&storage.connection).expect("link schema");
    let runner = ProcessWorktreeRunner::new();
    runner
        .run(
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/first/repo.git",
            ],
            repo.dir.path(),
        )
        .expect("origin A");
    let issue = 7;
    let repo_key = crate::worktree::lease_scope::verified_repo_key_for_identity(
        &crate::worktree::repo_identity(&runner, repo.dir.path()).expect("identity"),
    )
    .expect("origin-derived key A");
    let branch = crate::git_runner::current_branch(
        &crate::git_runner::ProcessGitRunner::in_directory(repo.dir.path()),
    )
    .expect("branch");
    link(
        &storage.connection,
        &LinkParams {
            repo_key: &repo_key,
            branch: &branch,
            issue: &IssueKey::new("redmine", "tools", issue.to_string()).unwrap(),
            issue_number: issue,
            source: "test",
            now: now_unix_secs().max(1),
        },
    )
    .expect("link under identity A");

    let source = ComposedIssueStateSource::new_prune(
        Box::new(GitLinkScopeReader::new_read_only()),
        Box::new(ScopedProvider::new("redmine").closed_for("redmine", "tools", issue)),
    );
    assert_eq!(
        source.closure(repo.dir.path(), issue),
        IssueClosure::Closed,
        "the scan proves the issue closed under identity A"
    );

    // The checkout origin changes before the removal recheck.
    runner
        .run(
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.test/second/repo.git",
            ],
            repo.dir.path(),
        )
        .expect("origin B");
    // The final proof boundary invalidates the target and must re-prove the
    // physical link identity rather than reuse the cached key.
    source.invalidate(repo.dir.path(), &[issue]);
    let verdict = source.closure(repo.dir.path(), issue);
    match verdict {
        IssueClosure::Keep(reason) => assert!(
            reason.contains("link identity changed"),
            "the keep names the changed identity: {reason}"
        ),
        IssueClosure::Closed => {
            panic!("a changed repository link identity must never authorize the removal")
        }
    }
}

//! Coverage for the narrow issue-closure adapter (issue 916).
//!
//! These drive [`ComposedIssueStateSource`] directly with a scripted link
//! reader and a scripted provider reader, so every state, scope conflict, and
//! mismatch is exercised without a real provider or network. The prune pass
//! itself is covered elsewhere with a plain
//! [`ScriptedIssueStateSource`](crate::cli::worktree::ScriptedIssueStateSource).

use super::*;

use crate::cli::worktree::{
    ComposedIssueStateSource, IssueClosure, IssueStateFact, IssueStateReader, LinkScope,
    LinkScopeReader,
};
use std::path::{Path, PathBuf};

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

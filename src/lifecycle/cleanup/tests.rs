//! Unit tests for the OpenCode guard of the closed-issue cleanup (issue
//! 747 P1, extended by issue 1195 P1).
//!
//! These drive the decision directly, without a Git or storage fixture:
//! the guard's only inputs are the mode, the candidate directory, the
//! sessions its registry associations name, and the answer the host gives.
//! The full-stack path (real registry rows, real worktree
//! guards, real `git worktree remove`) is covered by
//! `crate::worktree_tests::opencode_cleanup`.

use std::path::{Path, PathBuf};

use super::sessions::{FamilyOutcome, finalize_family};
use super::target::{MoveTarget, TargetResolver};
use super::{CleanupMode, associated_sessions};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::{DirectoryLegacyIssue, DirectoryRecord};

fn record(
    worktree_path: &str,
    sessions: &[&str],
    legacy_issues: Vec<DirectoryLegacyIssue>,
) -> DirectoryRecord {
    DirectoryRecord {
        directory_id: format!("dir-{worktree_path}"),
        repo_identity: "/repo/.git".to_owned(),
        checkout_path: "/repo".to_owned(),
        worktree_path: worktree_path.to_owned(),
        branch: "feat/7".to_owned(),
        created_at: 0,
        tasks: Vec::new(),
        legacy_issues,
        sessions: sessions
            .iter()
            .map(|session| (*session).to_owned())
            .collect(),
    }
}

fn unscoped(issue: u64) -> DirectoryLegacyIssue {
    DirectoryLegacyIssue {
        issue,
        provider: String::new(),
        project: String::new(),
        external_id: 0,
    }
}

pub(super) fn candidate() -> PathBuf {
    PathBuf::from("/tmp/pg-p1-candidate")
}

pub(super) fn main_checkout() -> MoveTarget {
    MoveTarget::Main(PathBuf::from("/tmp/pg-p1-main"))
}

pub(super) fn evaluate_with(
    api: &FakeOpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    associated: &[&str],
    closing: Option<&str>,
) -> FamilyOutcome {
    let ids: Vec<String> = associated.iter().map(|id| (*id).to_owned()).collect();
    finalize_family(api, mode, target, &candidate(), &ids, closing)
}

/// A resolver that never touches Git: the fixtures name the target
/// directly so the decision table is exercised without a repository.
pub(super) fn fixed(target: MoveTarget) -> TargetResolver<'static> {
    TargetResolver::resolved(target)
}

// ---------------------------------------------------------------------------
// Association discovery
// ---------------------------------------------------------------------------

#[test]
fn scope_directory_evidence_includes_mixed_legacy_rows() {
    let legacy = record("/tmp/pg-p1-candidate", &["legacy"], vec![unscoped(7)]);
    let scoped = record(
        "/tmp/pg-p1-candidate",
        &["scoped"],
        vec![DirectoryLegacyIssue {
            issue: 7,
            provider: "forgejo".to_owned(),
            project: "owner/repo".to_owned(),
            external_id: 7,
        }],
    );
    let context = super::CleanupContext {
        identity: "/repo/.git".to_owned(),
        records: vec![legacy, scoped],
    };
    assert!(context.has_scoped_directory(Path::new("/tmp/pg-p1-candidate")));
    assert!(!context.has_scoped_directory(Path::new("/tmp/pg-p1-other")));
}

#[test]
fn scope_cleanup_keeps_before_bare_provider_or_host_checks() {
    struct NoClosure;
    impl super::closure::DirectoryIssueClosure for NoClosure {
        fn confirm_others_closed(&self, _: u64, _: &[u64]) -> Result<(), String> {
            panic!("scoped cleanup queried bare issue")
        }
    }
    let directory = crate::test_scratch::root().join(format!(
        "pg-cleanup-scoped-{}",
        std::process::id() as u64 * 1_000_000 + line!() as u64
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.to_string_lossy();
    let scoped = record(
        &path,
        &["scoped"],
        vec![
            unscoped(7),
            DirectoryLegacyIssue {
                issue: 7,
                provider: "forgejo".to_owned(),
                project: "owner/repo".to_owned(),
                external_id: 7,
            },
        ],
    );
    let context = super::CleanupContext {
        identity: "/repo/.git".into(),
        records: vec![scoped],
    };
    let api = FakeOpenCodeApi::empty();
    let guard = super::candidate::DirectOccupancy(&api);
    struct CleanLinkedRunner;
    impl crate::worktree::WorktreeRunner for CleanLinkedRunner {
        fn run(
            &self,
            args: &[&str],
            _: &Path,
        ) -> Result<crate::worktree::GitOutput, crate::worktree::WorktreeError> {
            let stdout = if args == ["rev-parse", "--git-dir"] {
                "/repo/.git/worktrees/wt-7\n".to_owned()
            } else {
                String::new()
            };
            Ok(crate::worktree::GitOutput { status: 0, stdout })
        }
    }
    let runner = CleanLinkedRunner;
    let result = super::cleanup_closed_issue_worktrees_in_context(
        &runner,
        &guard,
        CleanupMode::Close,
        None,
        context,
        Path::new("/repo"),
        7,
        None,
        &NoClosure,
        None,
    );
    match result {
        super::AutoCleanupOutcome::Cleaned { removed, kept } => {
            assert_eq!(removed, 0);
            assert!(
                kept.iter()
                    .any(|reason| reason.contains("scoped provider association")),
                "the scoped directory is kept for provider-aware cleanup: {kept:?}"
            );
        }
        _ => panic!("unexpected cleanup outcome"),
    }
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn associated_sessions_keeps_only_opencode_ids_pointing_at_the_directory() {
    let records = vec![
        record(
            "/tmp/pg-p1-candidate",
            &["ses_a", "ses_b"],
            vec![unscoped(7)],
        ),
        record(
            "/tmp/pg-p1-candidate",
            &["phasegent-cli"],
            vec![unscoped(7)],
        ),
        record("/tmp/pg-p1-other", &["ses_c"], vec![unscoped(7)]),
    ];
    assert_eq!(
        associated_sessions(&records, &candidate()),
        vec!["ses_a".to_owned(), "ses_b".to_owned()]
    );
}

#[test]
fn a_candidate_without_an_opencode_association_still_requires_complete_host_evidence() {
    // A pure CLI close is not exempt from the host question: an occupant no
    // registry row names is somebody else's session. An unusable listing is
    // uncertainty and keeps the directory.
    let api = FakeOpenCodeApi::empty().failing_session().failing_list();
    let verdict = evaluate_with(&api, CleanupMode::Close, &fixed(main_checkout()), &[], None);
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an incomplete host answer must keep, got {verdict:?}");
    };
    assert!(reason.contains("incomplete"), "got: {reason}");
    assert!(api.list_calls() > 0, "the host listing must be attempted");
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// Close mode
// ---------------------------------------------------------------------------

#[test]
fn close_moves_the_own_session_and_confirms_with_fresh_evidence() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_main", Path::new("/tmp/pg-p1-main"));
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned()]
        }
    );
    assert_eq!(
        api.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())]
    );
}

#[test]
fn a_queued_move_keeps_the_candidate() {
    // Acceptance is not relocation: the session still reports the
    // candidate, so the directory must survive this pass.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .queueing_moves();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a queued move must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("has not left"), "got: {reason}");
}

#[test]
fn a_failed_move_request_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .failing_move();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a failed move must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("moving the OpenCode session 'ses_a' failed"),
        "got: {reason}"
    );
}

#[test]
fn an_unverified_main_checkout_keeps_the_candidate_without_moving() {
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &candidate());
    let target = MoveTarget::Unavailable("bare repository".to_owned());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(target),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unproven target must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("no verified main checkout"),
        "got: {reason}"
    );
    assert!(
        api.moves().is_empty(),
        "nothing may be moved without a target"
    );
}

#[test]
fn close_returns_the_whole_idle_family_of_the_candidate() {
    // Every session the registry associates with the directory is part of
    // its family, so the close returns all of them — not just its own session.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .with_session("ses_closer", Path::new("/tmp/pg-p1-main"));
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        Some("ses_closer"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned(), "ses_b".to_owned()]
        }
    );
    assert_eq!(
        api.moves(),
        vec![
            ("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned()),
            ("ses_b".to_owned(), "/tmp/pg-p1-main".to_owned()),
        ]
    );
}

#[test]
fn close_never_moves_a_running_family_member() {
    // A family member that is running is not idle: the close waits for it
    // instead of relocating a live session.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .with_running("ses_b");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a running session must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_b"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn close_keeps_when_caller_and_another_family_member_are_running() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .with_running("ses_a")
        .with_running("ses_b");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        Some("ses_a"),
    );
    assert!(
        matches!(verdict, FamilyOutcome::Keep(_)),
        "got: {verdict:?}"
    );
    assert!(api.moves().is_empty(), "got moves: {:?}", api.moves());
}

#[test]
fn close_keeps_when_several_other_family_members_are_running() {
    // Every running occupant is checked, not only the first: two running
    // peers behind the exempt closing session must still keep the directory.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .with_session("ses_c", &candidate())
        .with_running("ses_a")
        .with_running("ses_b")
        .with_running("ses_c");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b", "ses_c"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("every other running occupant must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("ses_b") || reason.contains("ses_c"),
        "the reason must name a non-caller running session, got: {reason}"
    );
    assert!(api.moves().is_empty(), "got moves: {:?}", api.moves());
}

#[test]
fn a_running_closing_session_still_finishes_itself() {
    // The exemption stays exact: when the closing session is the only
    // running occupant, the close still returns it and removes the
    // directory.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_running("ses_a");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned()]
        }
    );
    assert_eq!(
        api.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())]
    );
}

#[test]
fn prune_keeps_when_a_non_caller_family_member_is_running() {
    // Prune has no exemption at all: the same two running occupants keep the
    // directory there too.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .with_running("ses_b");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        None,
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a running family member must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("running OpenCode session"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_family_member_that_starts_running_during_an_earlier_move_is_not_moved() {
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };
    use std::cell::Cell;

    struct Host {
        inner: FakeOpenCodeApi,
        started: Cell<bool>,
    }
    impl OpenCodeApi for Host {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            self.inner.session(id)
        }
        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.inner.list(request)
        }
        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            let result = self.inner.move_session(id, directory);
            if id == "ses_a" && result.is_ok() {
                self.started.set(true);
            }
            result
        }
        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            Ok(if self.started.get() {
                vec!["ses_b".to_owned()]
            } else {
                Vec::new()
            })
        }
    }
    let api = Host {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &candidate())
            .with_session("ses_b", &candidate()),
        started: Cell::new(false),
    };
    let verdict = finalize_family(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned(), "ses_b".to_owned()],
        None,
    );
    assert!(
        api.inner.moves().iter().all(|(id, _)| id != "ses_b"),
        "running peer moved: {:?}",
        api.inner.moves()
    );
    assert!(
        matches!(verdict, FamilyOutcome::Keep(_)),
        "got: {verdict:?}"
    );
}

/// A family member that relocates itself to a third directory while an
/// earlier member is being returned is not reclaimed from unrelated work.
#[test]
fn a_family_member_that_moves_to_a_third_location_between_moves_is_not_moved() {
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };
    use std::cell::Cell;

    struct Host {
        inner: FakeOpenCodeApi,
        relocated: Cell<bool>,
    }
    impl OpenCodeApi for Host {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            if id == "ses_b" && self.relocated.get() {
                return Ok(LocatedSession {
                    id: "ses_b".to_owned(),
                    directory: "/tmp/pg-p1-third".to_owned(),
                    parent_id: None,
                });
            }
            self.inner.session(id)
        }
        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.inner.list(request)
        }
        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            let result = self.inner.move_session(id, directory);
            if id == "ses_a" && result.is_ok() {
                self.relocated.set(true);
            }
            result
        }
        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            self.inner.active_sessions()
        }
    }
    let api = Host {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &candidate())
            .with_session("ses_b", &candidate()),
        relocated: Cell::new(false),
    };
    let verdict = finalize_family(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned(), "ses_b".to_owned()],
        None,
    );
    assert_eq!(
        api.inner.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "only the member that still sat in the candidate may move"
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a relocated member must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_b"), "got: {reason}");
    assert!(reason.contains("not the directory"), "got: {reason}");
}

/// A descendant whose lineage no longer reaches a root between two moves is
/// not moved: the family proof is re-read for each exact session.
#[test]
fn a_family_member_whose_lineage_changes_between_moves_is_not_moved() {
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };
    use std::cell::Cell;

    struct Host {
        inner: FakeOpenCodeApi,
        changed: Cell<bool>,
    }
    impl OpenCodeApi for Host {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            if id == "ses_other" && self.changed.get() {
                return Ok(LocatedSession {
                    id: "ses_other".to_owned(),
                    directory: candidate().to_string_lossy().into_owned(),
                    parent_id: Some("ses_foreign_root".to_owned()),
                });
            }
            self.inner.session(id)
        }
        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.inner.list(request)
        }
        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            let result = self.inner.move_session(id, directory);
            if id == "ses_child" && result.is_ok() {
                self.changed.set(true);
            }
            result
        }
        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            self.inner.active_sessions()
        }
    }
    let api = Host {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
            .with_child("ses_child", &candidate(), "ses_root")
            .with_child("ses_other", &candidate(), "ses_root")
            .with_session("ses_foreign_root", Path::new("/tmp/pg-p1-main")),
        changed: Cell::new(false),
    };
    let verdict = finalize_family(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_root".to_owned()],
        None,
    );
    assert_eq!(
        api.inner.moves(),
        vec![("ses_child".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "a descendant that left the family must not move"
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a foreign lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("no longer part of"), "got: {reason}");
}

/// A running-session answer that becomes unavailable after the first move
/// fails closed before the next mutation.
#[test]
fn an_unavailable_running_answer_between_moves_keeps_the_candidate() {
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };
    use std::cell::Cell;

    struct Host {
        inner: FakeOpenCodeApi,
        fail: Cell<bool>,
    }
    impl OpenCodeApi for Host {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            self.inner.session(id)
        }
        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.inner.list(request)
        }
        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            let result = self.inner.move_session(id, directory);
            if id == "ses_a" && result.is_ok() {
                self.fail.set(true);
            }
            result
        }
        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            if self.fail.get() {
                return Err(ApiError::plain("scripted active failure"));
            }
            self.inner.active_sessions()
        }
    }
    let api = Host {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &candidate())
            .with_session("ses_b", &candidate()),
        fail: Cell::new(false),
    };
    let verdict = finalize_family(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned(), "ses_b".to_owned()],
        None,
    );
    assert_eq!(
        api.inner.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "the unavailable answer must stop the next move"
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unreadable running answer must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("running-session query failed"),
        "got: {reason}"
    );
}

#[test]
fn a_foreign_occupant_blocks_removal_without_being_moved() {
    // `ses_foreign` has no registry association with this repository and no
    // lineage to one, so it is somebody else's session and must never be
    // relocated by this pass.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_foreign", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a foreign occupant must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_foreign"), "got: {reason}");
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// The session family and its lineage
// ---------------------------------------------------------------------------

#[test]
fn a_descendant_of_the_candidate_root_is_returned_with_it() {
    // The root moved elsewhere, but it is still the session the registry
    // names, so the descendant that sits in the candidate is part of the
    // same family and is returned with it.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_root");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_child".to_owned()]
        }
    );
    assert_eq!(
        api.moves(),
        vec![("ses_child".to_owned(), "/tmp/pg-p1-main".to_owned())]
    );
}

#[test]
fn a_grandchild_is_proven_through_its_own_lineage() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_mid", Path::new("/tmp/pg-p1-main"), "ses_root")
        .with_child("ses_leaf", &candidate(), "ses_mid");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_leaf".to_owned()]
        }
    );
}

#[test]
fn a_descendant_of_another_family_is_foreign() {
    // The lineage reaches a session that is not one of this directory's
    // roots, so the occupant belongs to somebody else's family.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_session("ses_other", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_other");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a foreign lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_child"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn an_unknown_parent_keeps_the_candidate() {
    // The lineage cannot be read: the parent is a session the selected
    // instance does not own, so the occupant's family is unknown.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_parent")
        .with_unknown_session("ses_parent");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unreadable lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("could not be located"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_mismatched_parent_answer_keeps_the_candidate() {
    // The host answered for the parent with a different session: the
    // lineage edge is not evidence about the session that was asked for.
    struct MismatchedParent {
        inner: FakeOpenCodeApi,
    }
    impl crate::worktree::opencode::OpenCodeApi for MismatchedParent {
        fn session(
            &self,
            id: &str,
        ) -> Result<crate::worktree::opencode::LocatedSession, crate::worktree::opencode::ApiError>
        {
            let located = self.inner.session(id)?;
            if id == "ses_parent" {
                return Ok(crate::worktree::opencode::LocatedSession {
                    id: "ses_other".to_owned(),
                    directory: located.directory,
                    parent_id: None,
                });
            }
            Ok(located)
        }
        fn list(
            &self,
            request: &crate::worktree::opencode::ListRequest,
        ) -> Result<crate::worktree::opencode::SessionPage, crate::worktree::opencode::ApiError>
        {
            self.inner.list(request)
        }
        fn move_session(
            &self,
            id: &str,
            directory: &Path,
        ) -> Result<(), crate::worktree::opencode::ApiError> {
            self.inner.move_session(id, directory)
        }
        fn active_sessions(&self) -> Result<Vec<String>, crate::worktree::opencode::ApiError> {
            self.inner.active_sessions()
        }
    }
    let api = MismatchedParent {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
            .with_child("ses_child", &candidate(), "ses_parent")
            .with_session("ses_parent", Path::new("/tmp/pg-p1-main")),
    };
    let verdict = finalize_family(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_root".to_owned()],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a mismatched lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("different session"), "got: {reason}");
    assert!(api.inner.moves().is_empty());
}

#[test]
fn a_cyclic_lineage_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_parent")
        .with_child("ses_parent", Path::new("/tmp/pg-p1-main"), "ses_child");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a cyclic lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("cyclic"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn an_over_long_lineage_keeps_the_candidate() {
    // A chain longer than the fixed edge budget is uncertainty, not a
    // family: the traversal stops and the directory survives.
    let mut api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_link0");
    for index in 0..(super::sessions::MAX_PARENT_EDGES + 2) {
        api = api.with_child(
            &format!("ses_link{index}"),
            Path::new("/tmp/pg-p1-main"),
            &format!("ses_link{}", index + 1),
        );
    }
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an over-long lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("edge limit"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn an_unusable_parent_edge_keeps_the_candidate() {
    // A lineage edge that is not a session id cannot be read as "this
    // session has no parent": the occupant's family stays unknown.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_unusable_parent("ses_child", &candidate(), "phasegent/1-aaaaaa");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unusable lineage must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("unusable parent"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_running_descendant_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_root")
        .with_running("ses_child");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_root"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a running descendant must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_child"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_prune_never_gets_a_running_exemption() {
    // The close may finish its own session; the prune has no such
    // exemption, so a running root keeps the directory there too.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", &candidate())
        .with_running("ses_root");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &["ses_root"],
        None,
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a running session must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("running OpenCode session"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn the_preview_reports_the_family_without_moving_it() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_child("ses_child", &candidate(), "ses_root");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Preview,
        &fixed(main_checkout()),
        &["ses_root"],
        None,
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Releasable {
            sessions: vec!["ses_child".to_owned()]
        }
    );
    assert!(
        api.moves().is_empty(),
        "the preview reports the move eligibility and never performs it"
    );
}

#[test]
fn a_same_number_unknown_legacy_association_is_not_the_current_task() {
    use super::closure::{DirectoryAssociation, DirectoryIssueClosure};

    struct ExactTaskOnly;
    impl DirectoryIssueClosure for ExactTaskOnly {
        fn confirm_others_closed(&self, _: u64, _: &[u64]) -> Result<(), String> {
            Ok(())
        }

        fn confirm_association_closed(
            &self,
            current_issue: u64,
            association: DirectoryAssociation<'_>,
            _: Option<&crate::worktree::WorktreeScope>,
        ) -> Result<(), String> {
            match association {
                DirectoryAssociation::Task(task) if task.issue == current_issue => Ok(()),
                DirectoryAssociation::Task(task) => {
                    Err(format!("task {} is not confirmed closed", task.task_id))
                }
                DirectoryAssociation::Legacy(issue) => Err(format!(
                    "unknown same-number legacy {} is not the current task",
                    issue.issue
                )),
            }
        }
    }

    let path = "/tmp/pg-p1-candidate";
    let record = DirectoryRecord {
        directory_id: "dir-same-number".to_owned(),
        repo_identity: "/repo/.git".to_owned(),
        checkout_path: "/repo".to_owned(),
        worktree_path: path.to_owned(),
        branch: "feat/7".to_owned(),
        created_at: 0,
        tasks: vec![crate::worktree::DirectoryTask {
            task_id: 5,
            issue: 7,
            provider: Some("forgejo".to_owned()),
            instance: Some("https://forgejo.test".to_owned()),
            project: Some("owner/repo".to_owned()),
            external_id: Some("7".to_owned()),
        }],
        legacy_issues: vec![unscoped(7)],
        sessions: Vec::new(),
    };
    let error = super::confirm_directory_associations(
        &ExactTaskOnly,
        7,
        &[record],
        std::path::Path::new(path),
        None,
    )
    .expect_err("a same-number unknown legacy row must block");
    assert!(error.contains("unknown same-number legacy"), "got: {error}");
}

#[test]
fn the_final_cleanup_rechecks_the_host_for_a_cli_only_candidate() {
    use std::cell::Cell;

    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };
    use crate::worktree::registry::{self, NewAssociation, NewDirectory};
    use crate::worktree::{ProcessWorktreeRunner, repo_identity, worktree_add};

    struct ForeignOnlyAfterFirstList {
        inner: FakeOpenCodeApi,
        directory: PathBuf,
        calls: Cell<usize>,
    }

    impl OpenCodeApi for ForeignOnlyAfterFirstList {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            self.inner.session(id)
        }

        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            let mut page = self.inner.list(request)?;
            if call >= 1 {
                page.sessions.push(LocatedSession {
                    id: "ses_late".to_owned(),
                    directory: self.directory.to_string_lossy().into_owned(),
                    parent_id: None,
                });
            }
            Ok(page)
        }

        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            self.inner.move_session(id, directory)
        }

        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            self.inner.active_sessions()
        }
    }

    struct NoClosure;
    impl super::closure::DirectoryIssueClosure for NoClosure {
        fn confirm_others_closed(&self, _: u64, _: &[u64]) -> Result<(), String> {
            Ok(())
        }
    }

    let _lock = lock_workflow_tests();
    let Some(repo) = crate::worktree_tests::support::TempRepo::init("cleanup-final-host") else {
        return;
    };
    let (_db, storage, _env) = crate::worktree_tests::support::open_temp_db("cleanup-final-host");
    registry::ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = repo.dir.path().join("closed-cli");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/closed-cli").expect("worktree");
    let canonical = crate::worktree::canonical_or_self(&target);
    let id = registry::stable_directory_id(&identity, &canonical.to_string_lossy());
    registry::insert_directory(
        &storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &identity,
            checkout_path: &repo.dir.path().to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/closed-cli",
            created_at: 1,
            session: "cli-session",
            associations: &[NewAssociation::Unscoped { issue: 7 }],
        },
    )
    .expect("registry row");
    let context = super::CleanupContext {
        identity: identity.clone(),
        records: registry::load_records(&storage, Some(&identity)).expect("rows"),
    };
    let api = ForeignOnlyAfterFirstList {
        inner: FakeOpenCodeApi::empty(),
        directory: canonical.clone(),
        calls: Cell::new(0),
    };
    let outcome = super::cleanup_closed_issue_worktrees_in_context(
        &runner,
        &super::candidate::DirectOccupancy(&api),
        CleanupMode::Close,
        Some(&storage),
        context,
        repo.dir.path(),
        7,
        None,
        &NoClosure,
        None,
    );
    let super::AutoCleanupOutcome::Cleaned { removed, kept } = outcome else {
        panic!("a late foreign occupant must keep the directory, got {outcome:?}");
    };
    assert_eq!(removed, 0);
    assert!(
        kept.iter().any(|reason| reason.contains("not associated")),
        "the final post-cache host read must see the late occupant: {kept:?}"
    );
    assert_eq!(api.calls.get(), 2, "initial and final host reads both ran");
    assert!(target.exists());
}

#[test]
fn an_unrelated_idle_closing_session_is_never_promoted_into_the_family() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_session("ses_stranger", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_stranger"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unrelated closing id must keep, got {verdict:?}");
    };
    assert!(reason.contains("ses_stranger"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn an_unrelated_running_closing_session_is_exempt_from_nothing() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_root", Path::new("/tmp/pg-p1-main"))
        .with_session("ses_stranger", &candidate())
        .with_running("ses_stranger");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_root"],
        Some("ses_stranger"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unrelated running closing id must keep, got {verdict:?}");
    };
    assert!(
        reason.contains("currently occupies") || reason.contains("ses_stranger"),
        "got: {reason}"
    );
    assert!(api.moves().is_empty());
}

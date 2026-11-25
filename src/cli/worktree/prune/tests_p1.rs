//! The reduced reads of `worktree prune` (issue 51 P1).
//!
//! Every case here is isolated: a scripted closure source counts the numbers
//! it was asked about, and a scripted host counts the queries it served. No
//! registry, provider, host, or Git fixture is involved.

use std::cell::RefCell;

use super::*;
use crate::lifecycle::cleanup::target::{MoveTarget, TargetResolver};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::registry::{DirectoryLegacyIssue, DirectoryRecord, DirectoryTask};

/// The candidate directory every case names.
fn candidate_directory() -> PathBuf {
    PathBuf::from("/tmp/pg-p1-candidate")
}

/// The verified main checkout every case moves to.
fn main_directory() -> PathBuf {
    PathBuf::from("/tmp/pg-p1-main")
}

fn scope<'a>(target: &'a TargetResolver<'a>) -> PruneScope<'a> {
    PruneScope {
        identity: "/tmp/pg-p1-identity",
        repo_path: Path::new("/tmp/pg-p1-repo"),
        records: &[],
        target,
    }
}

fn target() -> TargetResolver<'static> {
    TargetResolver::resolved(MoveTarget::Main(main_directory()))
}

/// A registry record with the given durable tasks and legacy issues.
fn record(tasks: &[u64], legacy: &[u64]) -> DirectoryRecord {
    DirectoryRecord {
        directory_id: "dir-1".to_owned(),
        repo_identity: "/tmp/pg-p1-identity".to_owned(),
        checkout_path: "/tmp/pg-p1-repo".to_owned(),
        worktree_path: candidate_directory().to_string_lossy().into_owned(),
        branch: "feat/1".to_owned(),
        created_at: 0,
        tasks: tasks
            .iter()
            .enumerate()
            .map(|(index, issue)| DirectoryTask {
                task_id: index as i64 + 1,
                issue: *issue,
                provider: Some("forgejo".to_owned()),
                instance: Some("https://forgejo.example".to_owned()),
                project: Some("phasegent".to_owned()),
                external_id: Some(issue.to_string()),
            })
            .collect(),
        legacy_issues: legacy
            .iter()
            .enumerate()
            .map(|(index, issue)| DirectoryLegacyIssue {
                issue: *issue,
                provider: "redmine".to_owned(),
                project: "phasegent".to_owned(),
                external_id: *issue + index as u64,
            })
            .collect(),
        sessions: vec!["ses_root".to_owned()],
    }
}

fn candidate() -> DirectoryCandidate {
    DirectoryCandidate {
        canonical_path: candidate_directory(),
        worktree_path: candidate_directory().to_string_lossy().into_owned(),
        branch: "feat/1".to_owned(),
        directory_id: "dir-1".to_owned(),
        issues: vec![1],
        sessions: vec!["ses_root".to_owned()],
        disposition: PruneDisposition::Removable,
        cache: None,
        expected: record(&[1], &[]),
        reason_code: None,
    }
}

/// A closure source that records every number it was asked about.
struct CountingSource {
    closed: Vec<u64>,
    reads: RefCell<Vec<u64>>,
}

impl CountingSource {
    fn new(closed: &[u64]) -> Self {
        Self {
            closed: closed.to_vec(),
            reads: RefCell::new(Vec::new()),
        }
    }

    fn reads(&self) -> Vec<u64> {
        self.reads.borrow().clone()
    }
}

impl IssueStateSource for CountingSource {
    fn closure(&self, _repo_path: &Path, issue: u64) -> IssueClosure {
        self.reads.borrow_mut().push(issue);
        if self.closed.contains(&issue) {
            IssueClosure::Closed
        } else {
            IssueClosure::Open(format!("issue {issue} is not closed"))
        }
    }
}

// ---------------------------------------------------------------------------
// The closure evidence
// ---------------------------------------------------------------------------

#[test]
fn the_first_keeping_task_stops_the_later_task_reads() {
    let source = CountingSource::new(&[2, 3]);
    let target = target();
    let scope = scope(&target);
    let verdict = closure_of(&source, &scope, &record(&[1, 2, 3], &[]));
    assert_eq!(
        verdict,
        IssueClosure::Open("issue 1 is not closed".to_owned())
    );
    assert_eq!(
        source.reads(),
        vec![1],
        "a later task must not be read once an earlier one keeps the directory"
    );
}

#[test]
fn every_association_is_read_when_all_of_them_are_closed() {
    let source = CountingSource::new(&[1, 2, 3, 4]);
    let target = target();
    let scope = scope(&target);
    let verdict = closure_of(&source, &scope, &record(&[1, 2, 3], &[4]));
    assert_eq!(verdict, IssueClosure::Closed);
    assert_eq!(
        source.reads(),
        vec![1, 2, 3, 4],
        "every association must prove itself closed before the candidate is eligible"
    );
}

#[test]
fn a_keeping_legacy_issue_stops_the_later_legacy_reads() {
    let source = CountingSource::new(&[1]);
    let target = target();
    let scope = scope(&target);
    let verdict = closure_of(&source, &scope, &record(&[1], &[2, 3]));
    assert_eq!(
        verdict,
        IssueClosure::Open("issue 2 is not closed".to_owned())
    );
    assert_eq!(
        source.reads(),
        vec![1, 2],
        "a later legacy issue must not be read once an earlier one keeps the directory"
    );
}

#[test]
fn an_incomplete_task_identity_keeps_without_reading_the_provider() {
    let source = CountingSource::new(&[1, 2]);
    let target = target();
    let scope = scope(&target);
    let mut incomplete = record(&[1, 2], &[]);
    incomplete.tasks[0].instance = None;
    let verdict = closure_of(&source, &scope, &incomplete);
    assert!(
        matches!(verdict, IssueClosure::Unattributed(_)),
        "got: {verdict:?}"
    );
    assert!(
        source.reads().is_empty(),
        "an incomplete identity keeps before any provider read"
    );
}

// ---------------------------------------------------------------------------
// The post-move verification
// ---------------------------------------------------------------------------

#[test]
fn the_post_move_verification_reads_the_running_state_once() {
    let api = FakeOpenCodeApi::empty().with_running_session("ses_run", &candidate_directory());
    let target = target();
    let scope = scope(&target);
    let candidate = candidate();
    let rejection = verify_family_after_move(&api, &scope, &candidate, &[])
        .expect_err("a running session must keep the directory");
    assert!(
        rejection.message.contains("running OpenCode session"),
        "got: {}",
        rejection.message
    );
    assert_eq!(
        api.active_calls(),
        1,
        "the post-move verification reads the running state exactly once"
    );
    assert_eq!(
        api.session_calls(),
        1,
        "the running session is located exactly once"
    );
}

#[test]
fn the_earlier_running_snapshot_is_redundant_with_the_final_verification() {
    let api = FakeOpenCodeApi::empty().with_running_session("ses_run", &candidate_directory());
    let target = target();
    let scope = scope(&target);
    let candidate = candidate();
    let _ = verify_family_after_move(&api, &scope, &candidate, &[]);
    assert_eq!((api.active_calls(), api.session_calls()), (1, 1));
    // The earlier snapshot the post-move path used to take reads the very same
    // running state again: one running query and one location per running
    // session, for every post-move verification of every candidate.
    let snapshot = active_guard::RunningSnapshot::capture(&api).expect("the host answers");
    assert!(snapshot.keep_reason(&candidate_directory()).is_some());
    assert_eq!(
        (api.active_calls(), api.session_calls()),
        (2, 2),
        "the removed snapshot costs one running query and one location"
    );
}

#[test]
fn an_occupied_descendant_keeps_without_a_lineage_read() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_other", &main_directory())
        .with_session("ses_child", &candidate_directory())
        .with_parent("ses_child", "ses_other");
    let target = target();
    let scope = scope(&target);
    let candidate = candidate();
    let rejection = verify_family_after_move(&api, &scope, &candidate, &["ses_root".to_owned()])
        .expect_err("a hosted occupant must keep the directory");
    assert!(
        rejection.message.contains("still hosted here"),
        "got: {}",
        rejection.message
    );
    assert_eq!(
        api.session_calls(),
        1,
        "the verification locates the family root and traverses no lineage"
    );
}

#[test]
fn a_fresh_running_change_after_the_moves_keeps_the_directory() {
    let api = FakeOpenCodeApi::empty().with_running_session("ses_run", &candidate_directory());
    let target = target();
    let scope = scope(&target);
    let candidate = candidate();
    let rejection = verify_family_after_move(&api, &scope, &candidate, &[])
        .expect_err("a session that started running after the moves must keep the directory");
    assert!(
        rejection
            .message
            .contains("running OpenCode session 'ses_run'"),
        "got: {}",
        rejection.message
    );
}

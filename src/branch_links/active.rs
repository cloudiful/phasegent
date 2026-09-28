//! Active-issue resolution for one branch.
//!
//! Only `linked` rows count. Zero links is `None`; more than one is
//! `Ambiguous` and must never be guessed by automation — the worktree
//! lease disambiguates instead.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use super::issue_key::IssueKey;
use super::reads::LinkedIssue;
use super::store::STATUS_LINKED;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveIssue {
    None,
    Single(IssueKey),
    Ambiguous(Vec<IssueKey>),
}

pub fn resolve_active_issue(links: &[LinkedIssue]) -> ActiveIssue {
    let mut linked: Vec<IssueKey> = links
        .iter()
        .filter(|entry| entry.status == STATUS_LINKED)
        .map(|entry| entry.issue.clone())
        .collect();
    linked.sort_by(|a, b| {
        (&a.provider, &a.project, &a.external_id).cmp(&(&b.provider, &b.project, &b.external_id))
    });
    linked.dedup();
    match linked.len() {
        0 => ActiveIssue::None,
        1 => ActiveIssue::Single(linked.into_iter().next().expect("one link")),
        _ => ActiveIssue::Ambiguous(linked),
    }
}

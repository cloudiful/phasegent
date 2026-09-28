//! Compatibility and status-document tests for durable branch links.
//!
//! Covers the single-issue `compat` projection (numeric and scoped),
//! including cross-scope ambiguity, legacy merging, detach handling,
//! default-branch suppression, and the JSON status document shape.
//! Link-store behavior (link/detach/active resolution) lives in
//! `links.rs`; this file asserts the adapter-facing projection only.

use crate::branch_links::compat::{ScopedIssueRef, resolve_scoped_compat_issue};
use crate::branch_links::{self, IssueKey};

fn scoped<'a>(provider: &'a str, project: &'a str, issue_number: u64) -> ScopedIssueRef<'a> {
    ScopedIssueRef {
        provider,
        project,
        issue_number,
    }
}

#[test]
fn compat_rule_covers_single_ambiguous_detached_and_default() {
    // Single linked issue wins.
    let compat = branch_links::resolve_compat_issue(&[628], &[], None, "feat/628", Some("main"));
    assert_eq!(compat.issue_number, Some(628));
    assert!(!compat.ambiguous && !compat.suppressed_default);
    // Legacy-only branch keeps working.
    let compat = branch_links::resolve_compat_issue(&[], &[], Some(616), "feat/616", Some("main"));
    assert_eq!(compat.issue_number, Some(616));
    // Two distinct links are never guessed.
    let compat = branch_links::resolve_compat_issue(&[616, 628], &[], None, "feat/x", Some("main"));
    assert_eq!(compat.issue_number, None);
    assert!(compat.ambiguous);
    // A legacy binding superseded by an explicit detach is ignored.
    let compat =
        branch_links::resolve_compat_issue(&[], &[616], Some(616), "feat/616", Some("main"));
    assert_eq!(compat.issue_number, None);
    assert!(!compat.ambiguous);
    // The detected default branch never reports an active issue.
    let compat = branch_links::resolve_compat_issue(&[616], &[], Some(616), "main", Some("main"));
    assert_eq!(compat.issue_number, None);
    assert!(compat.suppressed_default);
    // Unknown detection keeps reporting.
    let compat = branch_links::resolve_compat_issue(&[616], &[], Some(616), "main", None);
    assert_eq!(compat.issue_number, Some(616));
    // Nothing linked anywhere.
    let compat = branch_links::resolve_compat_issue(&[], &[], None, "feat/x", Some("main"));
    assert_eq!(compat.issue_number, None);
}

#[test]
fn scoped_compat_marks_cross_scope_same_number_ambiguous() {
    // Two distinct provider/project identities sharing a number are
    // never collapsed into one active issue (issue 628).
    let both = [
        scoped("redmine", "tools-phasegent", 11),
        scoped("forgejo", "acme/widgets", 11),
    ];
    let compat = resolve_scoped_compat_issue(&both, &[], None, "feat/1", Some("main"));
    assert_eq!(compat.issue_number, None);
    assert!(compat.ambiguous);
    assert!(!compat.suppressed_default);
    // Two rows in one scope with different numbers stay ambiguous too.
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "p", 11), scoped("redmine", "p", 12)],
        &[],
        None,
        "feat/1",
        Some("main"),
    );
    assert_eq!(compat.issue_number, None);
    assert!(compat.ambiguous);
    // One scope plus one number still resolves.
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "tools-phasegent", 628)],
        &[],
        None,
        "feat/628",
        Some("main"),
    );
    assert_eq!(compat.issue_number, Some(628));
    assert!(!compat.ambiguous && !compat.suppressed_default);
    // A legacy binding that agrees with the single linked identity
    // still merges (P3 pin preserved).
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "tools-phasegent", 616)],
        &[],
        Some(616),
        "feat/616",
        Some("main"),
    );
    assert_eq!(compat.issue_number, Some(616));
    assert!(!compat.ambiguous);
    // A disagreeing legacy binding stays ambiguous.
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "tools-phasegent", 616)],
        &[],
        Some(628),
        "feat/616",
        Some("main"),
    );
    assert_eq!(compat.issue_number, None);
    assert!(compat.ambiguous);
    // A detached other-scope row never confuses the linked identity.
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "p", 11)],
        &[scoped("forgejo", "q", 11)],
        None,
        "feat/1",
        Some("main"),
    );
    assert_eq!(compat.issue_number, Some(11));
    assert!(!compat.ambiguous);
    // A legacy binding superseded by an explicit detach is ignored.
    let compat = resolve_scoped_compat_issue(
        &[],
        &[scoped("redmine", "p", 616)],
        Some(616),
        "feat/616",
        Some("main"),
    );
    assert_eq!(compat.issue_number, None);
    assert!(!compat.ambiguous);
    // The detected default branch never reports an active issue, and a
    // cross-scope duplicate keeps the ambiguous flag under suppression.
    let compat = resolve_scoped_compat_issue(
        &[scoped("redmine", "p", 616)],
        &[],
        Some(616),
        "main",
        Some("main"),
    );
    assert_eq!(compat.issue_number, None);
    assert!(compat.suppressed_default);
    let compat = resolve_scoped_compat_issue(&both, &[], None, "main", Some("main"));
    assert_eq!(compat.issue_number, None);
    assert!(compat.suppressed_default && compat.ambiguous);
}

fn linked_issue(number: u64, status: &str) -> branch_links::LinkedIssue {
    branch_links::LinkedIssue {
        issue: IssueKey::from_number("redmine", "tools-phasegent", number).expect("key"),
        issue_number: number,
        status: status.to_owned(),
        source: "cli-bind".to_owned(),
        created_at: 1_700_000_001,
        updated_at: 1_700_000_002,
        detached_at: if status == "detached" {
            Some(1_700_000_010)
        } else {
            None
        },
        detached_reason: if status == "detached" {
            Some("unbind".to_owned())
        } else {
            None
        },
        state: None,
    }
}

#[test]
fn status_document_keeps_the_compat_core_and_projects_states() {
    let scope = branch_links::LinkScope {
        provider: "redmine".to_owned(),
        project: "tools-phasegent".to_owned(),
    };
    let resolved = branch_links::ResolvedRepo {
        key: "forge.example.com/owner/repo".to_owned(),
        local_only: false,
    };
    let rows = vec![linked_issue(628, "linked")];
    let compat = branch_links::resolve_compat_issue(&[628], &[], None, "feat/628", Some("main"));
    let document =
        crate::cli::branch::status::status_document(&crate::cli::branch::status::StatusView {
            branch: "feat/628",
            legacy: None,
            repo_key: Some(&resolved),
            scope: Some(&scope),
            rows: &rows,
            reverse: &[],
            default_branch: Some("main"),
            compat: &compat,
        });
    assert_eq!(document["branch"], serde_json::json!("feat/628"));
    assert_eq!(document["issue_id"], serde_json::json!(628));
    // `feat/628` parses a branch-name fallback, which the legacy
    // `source` still reports while the durable link decides `issue_id`.
    assert_eq!(document["source"], serde_json::json!("named"));
    assert_eq!(
        document["repo_key"],
        serde_json::json!("forge.example.com/owner/repo")
    );
    assert_eq!(document["local_only"], serde_json::json!(false));
    assert_eq!(document["is_default_branch"], serde_json::json!(false));
    assert_eq!(document["ambiguous"], serde_json::json!(false));
    let issues = document["linked_issues"].as_array().expect("linked_issues");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["state"], serde_json::json!("unknown"));
    assert_eq!(issues[0]["state_source"], serde_json::json!("unknown"));
    assert!(issues[0]["indexed_at"].is_null());
}

#[test]
fn status_document_reports_null_issue_id_for_cross_scope_duplicates() {
    // Unscoped rendering over two scopes sharing one number: the
    // document keeps both rows but guesses no `issue_id` (issue 628).
    let mut forgejo_row = linked_issue(628, "linked");
    forgejo_row.issue = IssueKey::from_number("forgejo", "acme/widgets", 628).expect("key");
    let rows = vec![linked_issue(628, "linked"), forgejo_row];
    let linked_refs: Vec<crate::branch_links::compat::ScopedIssueRef> = rows
        .iter()
        .map(|entry| crate::branch_links::compat::ScopedIssueRef {
            provider: &entry.issue.provider,
            project: &entry.issue.project,
            issue_number: entry.issue_number,
        })
        .collect();
    let compat = resolve_scoped_compat_issue(&linked_refs, &[], None, "feat/628", Some("main"));
    assert!(compat.ambiguous);
    let resolved = branch_links::ResolvedRepo {
        key: "forge.example.com/owner/repo".to_owned(),
        local_only: false,
    };
    let document =
        crate::cli::branch::status::status_document(&crate::cli::branch::status::StatusView {
            branch: "feat/628",
            legacy: None,
            repo_key: Some(&resolved),
            scope: None,
            rows: &rows,
            reverse: &[],
            default_branch: Some("main"),
            compat: &compat,
        });
    assert!(
        document["issue_id"].is_null(),
        "no guessed issue: {document}"
    );
    assert_eq!(document["ambiguous"], serde_json::json!(true));
    assert_eq!(document["scope"], serde_json::Value::Null);
    let issues = document["linked_issues"].as_array().expect("linked_issues");
    assert_eq!(issues.len(), 2, "both scopes stay visible: {issues:?}");
    assert_eq!(issues[0]["issue_number"], serde_json::json!(628));
    assert_eq!(issues[1]["issue_number"], serde_json::json!(628));
}

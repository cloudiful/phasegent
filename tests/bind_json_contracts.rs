//! Backward-compatibility contract for the single-id `issue bind` /
//! `issue unbind` JSON documents.
//!
//! Batch support must stay additive: a one-id invocation keeps every
//! pre-existing field with its historical meaning and type, and the batch
//! fields appear alongside them.

#[path = "snapshots_support/mod.rs"]
mod snapshots;

use std::process::Command;

use snapshots::fixtures::init_repo;
use snapshots::{Scratch, expect_json};

fn checkout_branch(repo: &std::path::Path, branch: &str) {
    let output = Command::new("git")
        .args(["checkout", "-q", "-b", branch])
        .current_dir(repo)
        .output()
        .expect("git checkout runs");
    assert!(
        output.status.success(),
        "git checkout failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn single_bind_and_unbind_keep_legacy_fields_and_add_batch_fields() {
    let scratch = Scratch::new("bind-json");
    let repo = init_repo(&scratch.join("repo"));
    checkout_branch(&repo, "feat/7");

    let bound = expect_json(
        &scratch,
        &repo,
        Some("orchestrator"),
        &["--provider", "local", "issue", "bind", "7"],
    );
    // Legacy single-id fields, unchanged in name, type, and meaning.
    assert_eq!(bound["bound"].as_bool(), Some(true));
    assert_eq!(bound["issue_id"].as_u64(), Some(7));
    assert_eq!(bound["already_bound"].as_bool(), Some(false));
    assert_eq!(bound["replaced"].as_bool(), Some(false));
    assert_eq!(bound["branch"].as_str(), Some("feat/7"));
    assert_eq!(bound["provider"].as_str(), Some("local"));
    assert_eq!(bound["project"].as_str(), Some("default"));
    assert!(bound["repo_key"].is_string(), "{bound}");
    assert_eq!(bound["local_only"].as_bool(), Some(true));
    // Additive batch fields.
    assert_eq!(bound["bound_count"].as_u64(), Some(1));
    assert_eq!(bound["issue_ids"], serde_json::json!([7]));
    assert_eq!(
        bound["results"],
        serde_json::json!([{"issue_id": 7, "already_bound": false, "replaced": false}])
    );

    // An idempotent repeat keeps the same shape and flips only the
    // already-bound flag.
    let repeated = expect_json(
        &scratch,
        &repo,
        Some("orchestrator"),
        &["--provider", "local", "issue", "bind", "7"],
    );
    assert_eq!(repeated["bound"].as_bool(), Some(true));
    assert_eq!(repeated["issue_id"].as_u64(), Some(7));
    assert_eq!(repeated["already_bound"].as_bool(), Some(true));
    assert_eq!(repeated["replaced"].as_bool(), Some(false));
    assert_eq!(repeated["bound_count"].as_u64(), Some(1));

    let unbound = expect_json(
        &scratch,
        &repo,
        Some("orchestrator"),
        &["--provider", "local", "issue", "unbind", "7"],
    );
    assert_eq!(unbound["unbound"].as_bool(), Some(true));
    assert_eq!(unbound["detached"].as_u64(), Some(1));
    assert_eq!(unbound["branch"].as_str(), Some("feat/7"));
    assert_eq!(unbound["provider"].as_str(), Some("local"));
    assert_eq!(unbound["project"].as_str(), Some("default"));
    assert!(unbound["repo_key"].is_string(), "{unbound}");
    assert_eq!(unbound["local_only"].as_bool(), Some(true));
    assert_eq!(unbound["issue_ids"], serde_json::json!([7]));
    assert_eq!(
        unbound["results"],
        serde_json::json!([{"issue_id": 7, "detached": true, "status": "detached"}])
    );

    // A second unbind is a no-op with the historical reason field.
    let noop = expect_json(
        &scratch,
        &repo,
        Some("orchestrator"),
        &["--provider", "local", "issue", "unbind", "7"],
    );
    assert_eq!(noop["unbound"].as_bool(), Some(false));
    assert_eq!(noop["detached"].as_u64(), Some(0));
    assert!(noop["reason"].is_string(), "{noop}");
    assert_eq!(
        noop["results"],
        serde_json::json!([{"issue_id": 7, "detached": false, "status": "already_detached"}])
    );
}

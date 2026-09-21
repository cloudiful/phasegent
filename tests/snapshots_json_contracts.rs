//! Snapshot coverage for the stdout JSON documents of the close and sync
//! contracts, driven against the local provider.
//!
//! `issue close` prints the provider's issue document and `issue sync
//! --no-clean` prints the per-directory verdict report; both are parsed by
//! orchestration scripts, so the exact serialized shape is the contract.
//! Volatile values (the scratch paths a developer's checkout happens to
//! produce) are filtered out of the snapshots, and each document is produced
//! twice to prove the shape is deterministic.

#[path = "snapshots_support/mod.rs"]
mod snapshots;

use std::fs;

use insta::assert_snapshot;
use snapshots::fixtures::{
    add_worktree, create_local_issue, init_repo, insert_active_lease, open_lease_store,
    set_local_status,
};
use snapshots::{Scratch, regex_escape, run, stderr_text, stdout_text};

/// Snapshot one stdout document with the scratch root filtered to `[SCRATCH]`,
/// so a developer's checkout path never enters the snapshot.
fn snapshot_stdout(name: &str, scratch: &Scratch, stdout: String) {
    let root = regex_escape(&scratch.canonical_root().to_string_lossy());
    insta::with_settings!({filters => vec![(root.as_str(), "[SCRATCH]")]}, {
        assert_snapshot!(name, stdout.trim_end());
    });
}

/// `issue close` prints the closed issue document. A second close of the same
/// issue returns the same document, which doubles as the determinism guard.
#[test]
fn issue_close_stdout_snapshot() {
    let scratch = Scratch::new("json-close");
    let repo = init_repo(&scratch.join("repo"));
    let fixture = scratch.dir("fixture");
    let number = create_local_issue(
        &scratch,
        &fixture,
        "Snapshot close target",
        "Close target body",
    );
    set_local_status(&scratch, &fixture, number, "Resolved");
    let number = number.to_string();
    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "local",
        "issue",
        "close",
        &number,
    ];

    let first = run(&scratch, &repo, &args);
    assert!(
        first.status.success(),
        "issue close exited with {}: stderr={}",
        first.status,
        stderr_text(&first),
    );
    let document = stdout_text(&first);
    let second = stdout_text(&run(&scratch, &repo, &args));
    assert_eq!(
        document, second,
        "an already-closed issue must print the same document",
    );

    snapshot_stdout("issue_close_stdout", &scratch, document);
}

/// `issue sync --no-clean` prints the report envelope: counters plus one entry
/// per closed issue with a verdict per existing worktree directory. The
/// fixture holds a clean directory (would_clean), a dirty one (would_keep with
/// the guard's reason), and an issue the provider still has open (not_closed).
#[test]
fn issue_sync_no_clean_stdout_snapshot() {
    let scratch = Scratch::new("json-sync");
    let repo = init_repo(&scratch.join("repo"));
    let fixture = scratch.dir("fixture");
    let identity = open_lease_store(&scratch, &repo);

    let closed = create_local_issue(
        &scratch,
        &fixture,
        "Snapshot sync closed",
        "Closed remotely",
    );
    set_local_status(&scratch, &fixture, closed, "Closed");
    let clean = add_worktree(&repo, &scratch.join("wt-clean"), "feat/558-clean");
    insert_active_lease(&scratch, &identity, closed, "session-clean", &repo, &clean);
    let dirty = add_worktree(&repo, &scratch.join("wt-dirty"), "feat/558-dirty");
    fs::write(dirty.join("scratch.txt"), "work in progress\n").expect("write dirty worktree file");
    insert_active_lease(&scratch, &identity, closed, "session-dirty", &repo, &dirty);

    let open = create_local_issue(&scratch, &fixture, "Snapshot sync open", "Still open");
    let open_worktree = add_worktree(&repo, &scratch.join("wt-open"), "feat/558-open");
    insert_active_lease(
        &scratch,
        &identity,
        open,
        "session-open",
        &repo,
        &open_worktree,
    );

    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "local",
        "issue",
        "sync",
        "--no-clean",
    ];
    let first = run(&scratch, &repo, &args);
    assert!(
        first.status.success(),
        "issue sync --no-clean exited with {}: stderr={}",
        first.status,
        stderr_text(&first),
    );
    let document = stdout_text(&first);
    let second = stdout_text(&run(&scratch, &repo, &args));
    assert_eq!(
        document, second,
        "report mode writes nothing, so two runs must print the same document",
    );

    assert!(
        clean.exists() && dirty.exists() && open_worktree.exists(),
        "report mode never removes a worktree directory",
    );
    snapshot_stdout("issue_sync_no_clean_stdout", &scratch, document);
}

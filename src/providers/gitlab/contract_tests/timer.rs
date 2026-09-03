#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{IssueProvider, ProviderDispatcher, RepoProvider};
#[test]
fn gitlab_timer_finish_first_call_posts_spent_time_with_marker() {
    // Drive `project_run_with_gitlab_provider` end-to-end. The
    // local ledger is the source of truth; the provider POSTs
    // `add_spent_time` with the run marker as the summary and never
    // round-trips through `/notes` or `/time_stats` for
    // reconciliation.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-abc",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-abc", "DONE", 1_700_003_600)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(run.sync_status, "synced");
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"1h""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}
#[test]
fn gitlab_timer_finish_retry_uses_local_ledger_marker_not_note_body() {
    // GitLab REST v4 does not surface the spent-time summary back
    // through `/notes` or `/time_stats`, so the projection cannot
    // rely on note-body matching. The local SQLite ledger's
    // `sync_status` column is the sole idempotency marker; the
    // test deliberately does NOT inject the run marker into any
    // mocked note body so the assertion verifies real GitLab
    // behaviour rather than the old (broken) find_marker path.
    let provider = provider("http://127.0.0.1:1".to_owned());
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-retry",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-retry", "DONE", 1_700_003_600)
        .unwrap();
    // Mark the run as already-projected (sync_status = synced,
    // time_entry_id stays None because the GitLab API does not
    // surface a numeric timelog id). A retry on the same run id
    // must observe this state and skip every network call.
    let _ = storage.mark_timer_sync(
        "timer-retry",
        None,
        None,
        crate::infra::storage::TIMER_SYNC_SYNCED,
        None,
    );
    let mut run = storage.load_timer_run("timer-retry").unwrap().unwrap();
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(run.sync_status, "synced");
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must keep time_entry_id null when no remote id exists",
    );
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}
#[test]
fn gitlab_timer_finish_failure_marks_ledger_failed() {
    // The projection POST is the only network call in the GitLab
    // path. A 422 response surfaces as a structured http error and
    // the failed-state recovery path in `execute_finish` records
    // the bounded error message on the ledger.
    let (base, requests, server) = sequence(vec![MockResponse::status(
        422,
        r#"{"message":"invalid duration"}"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-fail",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-fail", "DONE", 1_700_000_060)
        .unwrap();
    let mut run = finished;
    let error = crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 422);
    // The failed-state recovery path inside execute_finish records the
    // bounded error message on the ledger so a retry can pick up the
    // context.
    let _ = storage.mark_timer_sync(
        "timer-fail",
        run.activity_id,
        run.time_entry_id,
        crate::infra::storage::TIMER_SYNC_FAILED,
        Some(&error.to_string()),
    );
    let row = storage.load_timer_run("timer-fail").unwrap().unwrap();
    assert_eq!(row.sync_status, "failed");
    assert!(row.sync_error.is_some());
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
    let _ = requests;
}

#[test]
fn gitlab_timer_finish_skips_when_already_synced() {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-sync",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-sync", "DONE", 1_700_000_060)
        .unwrap();
    // Mark the run as synced without a time_entry_id (the GitLab
    // happy path leaves the column null).
    let _ = storage.mark_timer_sync(
        "timer-sync",
        None,
        None,
        crate::infra::storage::TIMER_SYNC_SYNCED,
        None,
    );
    let mut run = storage.load_timer_run("timer-sync").unwrap().unwrap();
    // The projection path must observe the synced status and skip
    // every network call.
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(run.sync_status, "synced");
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_marks_synced_when_response_uses_live_time_stats() {
    // GitLab 19.x returns the issue-shaped body for
    // POST /add_spent_time with the running totals wrapped under
    // a nested `time_stats` block. The projection must treat this
    // as a successful write (sync_status = synced) instead of
    // falling back to `unconfirmed`, otherwise the local ledger
    // would never observe a successful projection and every retry
    // would re-POST against the live instance. The disposable
    // issue that captured the live 2-second write is expected to
    // retain it; the test asserts only that the local state
    // machine advances to `synced`.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "id": 7,
            "iid": 2,
            "title": "Live timer fixture",
            "state": "opened",
            "labels": [],
            "time_stats": {
                "time_estimate": 0,
                "total_time_spent": 2,
                "human_time_estimate": null,
                "human_total_time_spent": "2s"
            }
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-live-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-live-shape", "DONE", 1_700_000_002)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(
        run.sync_status, "synced",
        "nested time_stats must confirm the spent-time write: run={run:?}",
    );
    assert!(
        run.sync_error.is_none(),
        "synced projection must not carry a sync_error: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id even when the response is issue-shaped",
    );
    let persisted = storage.load_timer_run("timer-live-shape").unwrap().unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(
        persisted.time_entry_id.is_none(),
        "persisted run must also keep time_entry_id null: {persisted:?}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"2s""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-live-shape""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_unconfirmed_when_response_omits_totals_entirely() {
    // A genuinely empty / unknown-shape response must still fall
    // back to `unconfirmed`. The repair only widens confirmation
    // to the issue-shaped body; the retry path keeps its
    // structured warning semantics for ambiguous results.
    let (base, _requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "id": 7,
            "iid": 2,
            "state": "opened",
            "labels": []
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-empty-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-empty-shape", "DONE", 1_700_000_002)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(
        run.sync_status, "unconfirmed",
        "totals-free response must keep unconfirmed semantics: run={run:?}",
    );
    assert!(
        run.sync_error.is_some(),
        "unconfirmed projection must record the bounded warning: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "unconfirmed projection must not invent a time_entry_id",
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_marks_synced_when_response_uses_top_level_time_stats() {
    // Live GitLab 19.x returns the top-level time-stats object
    // (not the nested issue shape) for
    // POST /projects/:id/issues/:iid/add_spent_time. The body
    // captured against project 3 issue 5 was
    // `{ "time_estimate": 0, "total_time_spent": 6,
    //   "human_time_estimate": null, "human_total_time_spent": "6s" }`.
    // The projection must observe the top-level `total_time_spent`
    // and advance `sync_status` to `synced`. The previous
    // attempt's nested-only handling left every top-level field
    // None and therefore marked a successful POST as
    // `unconfirmed`.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "time_estimate": 0,
            "total_time_spent": 6,
            "human_time_estimate": null,
            "human_total_time_spent": "6s"
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-top-level-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-top-level-shape", "DONE", 1_700_000_006)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(
        &storage, &mut run, &provider, "tok-test",
    )
    .unwrap();
    assert_eq!(
        run.sync_status, "synced",
        "top-level time_stats must confirm the spent-time write: run={run:?}",
    );
    assert!(
        run.sync_error.is_none(),
        "synced projection must not carry a sync_error: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id even when the response uses the top-level shape",
    );
    let persisted = storage
        .load_timer_run("timer-top-level-shape")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(
        persisted.time_entry_id.is_none(),
        "persisted run must also keep time_entry_id null: {persisted:?}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"6s""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-top-level-shape""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

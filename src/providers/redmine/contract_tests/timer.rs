#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn timer_rounding_and_marker_helpers_have_exact_summary_semantics() {
    assert_eq!(crate::time_tracking_cli::rounded_hours(0), 0.01);
    assert_eq!(crate::time_tracking_cli::rounded_hours(1), 0.01);
    assert_eq!(crate::time_tracking_cli::rounded_hours(35), 0.01);
    assert_eq!(crate::time_tracking_cli::rounded_hours(36), 0.01);
    assert_eq!(crate::time_tracking_cli::rounded_hours(37), 0.02);
    assert_eq!(crate::time_tracking_cli::rounded_hours(3_600), 1.0);
    assert_eq!(crate::time_tracking_cli::rounded_hours(3_601), 1.01);
    assert_eq!(
        crate::time_tracking_cli::format_unix_date(1_700_000_037).unwrap(),
        "2023-11-14"
    );

    let run = TimerRun {
        run_id: "run-1".to_owned(),
        issue: 28,
        phase: "implementation".to_owned(),
        role: "executor".to_owned(),
        attempt: 1,
        started_at: 1_700_000_000,
        finished_at: Some(1_700_000_037),
        status: "DONE".to_owned(),
        elapsed_seconds: Some(37),
        rounded_hours: Some(0.02),
        activity_id: None,
        time_entry_id: None,
        sync_status: "pending".to_owned(),
        sync_error: None,
        owner_session_id: None,
        owner_call_id: None,
        projection_token: None,
        projection_claimed_at: None,
    };
    assert_eq!(
        crate::time_tracking_cli::time_entry_comments(&run),
        "phasegent timer run_id=run-1"
    );
}

#[test]
fn timer_projection_retry_is_local_only_after_a_synced_201_create() {
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-retry-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .start_timer_run(
            "retry-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let mut run = storage
        .finish_timer_run("retry-run", "DONE", 1_700_000_037)
        .unwrap();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(time_entry_activities(&[(9, "AI automation", false)])),
        MockResponse::ok(time_entry_collection(&[])),
        MockResponse::status(
            201,
            time_entry_response(
                77,
                28,
                9,
                0.02,
                "phasegent timer run_id=retry-run",
                "2023-11-14",
            ),
        ),
    ]);
    let provider = crate::providers::RedmineProvider::new(
        crate::providers::RedmineConfig::new(base, "42", 37),
        TEST_API_KEY.to_owned(),
    )
    .unwrap();

    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider, "tok-test")
        .unwrap();
    assert_eq!(run.time_entry_id, Some(77));
    assert_eq!(run.sync_status, "synced");
    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider, "tok-test")
        .unwrap();

    let observed = requests.recv().unwrap();
    assert_eq!(
        observed.len(),
        3,
        "a synced retry must not call Redmine again"
    );
    assert!(observed[0].starts_with("GET /enumerations/time_entry_activities.json"));
    assert!(observed[1].starts_with("GET /time_entries.json?"));
    assert!(observed[2].starts_with("POST /time_entries.json"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_projection_reconciles_a_204_before_creating_another_entry() {
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-unconfirmed-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .start_timer_run(
            "unconfirmed-run",
            28,
            "implementation",
            "reviewer",
            1,
            1_700_000_000,
        )
        .unwrap();
    let mut run = storage
        .finish_timer_run("unconfirmed-run", "DONE", 1_700_000_037)
        .unwrap();
    let comments = crate::time_tracking_cli::time_entry_comments(&run);
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(time_entry_activities(&[(9, "AI automation", false)])),
        MockResponse::ok(time_entry_collection(&[])),
        MockResponse::status(204, ""),
        MockResponse::ok(time_entry_collection(&[(
            77,
            28,
            9,
            0.02,
            &comments,
            "2023-11-14",
        )])),
    ]);
    let provider = crate::providers::RedmineProvider::new(
        crate::providers::RedmineConfig::new(base, "42", 37),
        TEST_API_KEY.to_owned(),
    )
    .unwrap();

    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider, "tok-test")
        .unwrap();
    assert_eq!(run.sync_status, "unconfirmed");
    assert_eq!(run.time_entry_id, None);
    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider, "tok-test")
        .unwrap();
    assert_eq!(run.sync_status, "synced");
    assert_eq!(run.time_entry_id, Some(77));

    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 4, "reconciliation must avoid a second POST");
    assert!(observed[2].starts_with("POST /time_entries.json"));
    assert!(observed[3].starts_with("GET /time_entries.json?"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

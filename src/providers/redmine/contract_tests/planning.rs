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
use crate::ci_model::CiRunsFilter;
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
fn create_issue_with_planning_serializes_native_fields_and_omits_unset_ones() {
    let planning = crate::providers::redmine::model::IssuePlanning {
        parent_issue_id: Some(7),
        fixed_version_id: Some(3),
        start_date: Some("2026-08-01".to_owned()),
        due_date: Some("2026-08-31".to_owned()),
        estimated_hours: Some(4.5),
        done_ratio: Some(40),
    };
    let (result, request) = one(
        MockResponse::ok(issue_response(26, "Planned", "Body", false, &[])),
        |redmine| redmine.create_issue_with_planning("Planned", "Body", Some(2), &planning),
    );
    assert_eq!(result.unwrap().number, 26);
    support::assert_request(&request, "POST", "/issues.json", None);
    assert!(
        request.contains(
            r#""issue":{"project_id":42,"subject":"Planned","description":"Body","tracker_id":2,"parent_issue_id":7,"fixed_version_id":3,"start_date":"2026-08-01","due_date":"2026-08-31","estimated_hours":4.5,"done_ratio":40}"#
        ),
        "request: {request}"
    );

    // Omitted planning fields must stay out of the payload entirely so the
    // legacy create request shape remains byte-identical.
    let (_, request) = one(
        MockResponse::ok(issue_response(27, "Plain", "Body", false, &[])),
        |redmine| redmine.create_issue_with_planning("Plain", "Body", None, &Default::default()),
    );
    for field in [
        "parent_issue_id",
        "fixed_version_id",
        "start_date",
        "due_date",
        "estimated_hours",
        "done_ratio",
    ] {
        assert!(
            !request.contains(field),
            "payload leaked {field}: {request}"
        );
    }
}

#[test]
fn update_body_with_planning_keeps_single_put_shape() {
    let planning = crate::providers::redmine::model::IssuePlanning {
        fixed_version_id: Some(9),
        due_date: Some("2026-09-15".to_owned()),
        done_ratio: Some(60),
        ..Default::default()
    };
    let (result, request) = one(
        MockResponse::ok(issue_response(28, "Title", "Updated", false, &[])),
        |redmine| redmine.update_body_with_planning(28, "Updated", None, &planning),
    );
    assert_eq!(result.unwrap().body, "Updated");
    support::assert_request(&request, "PUT", "/issues/28.json", None);
    assert!(
        request.contains(
            r#""issue":{"description":"Updated","fixed_version_id":9,"due_date":"2026-09-15","done_ratio":60}"#
        ),
        "request: {request}"
    );
    assert!(!request.contains("status_id"));
}

#[test]
fn planning_validation_rejects_malformed_values_before_any_write() {
    use crate::command::PlanningOptions;
    use crate::providers::redmine::planning::resolve_planning;
    // A closed-port base keeps the provider offline: any network access in
    // these validation paths would surface as an http error instead of the
    // expected config error.
    let dispatcher = ProviderDispatcher::Redmine(provider("http://127.0.0.1:1".to_owned()));
    let invalid = |options: PlanningOptions| {
        resolve_planning(&dispatcher, &options)
            .expect_err("malformed planning value must be rejected")
            .json()["kind"]
            == "config"
    };
    let options = PlanningOptions {
        parent_issue: Some("0".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        parent_issue: Some("abc".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        done_ratio: Some("101".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        estimated_hours: Some("-2".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        start_date: Some("2026/08/01".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        due_date: Some("2026-13-01".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
    let options = PlanningOptions {
        due_date: Some("not-a-date".to_owned()),
        ..Default::default()
    };
    assert!(invalid(options));
}

#[test]
fn planning_flags_are_forgejo_not_supported_and_empty_planning_stays_plain() {
    use crate::command::PlanningOptions;
    use crate::providers::redmine::planning::resolve_planning;
    let forgejo = ProviderDispatcher::Forgejo(
        crate::providers::forgejo::ForgejoProvider::new(
            crate::providers::forgejo::ForgejoConfig::new("http://forgejo.test", "owner", "repo"),
            "token".to_owned(),
        )
        .unwrap(),
    );
    let options = PlanningOptions {
        fixed_version: Some("Sprint 1".to_owned()),
        ..Default::default()
    };
    let error = resolve_planning(&forgejo, &options).unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
    assert!(!error.to_string().contains("Sprint 1"));
}

#[test]
fn done_ratio_accepts_zero_and_serializes_the_boundary_value() {
    use crate::command::PlanningOptions;
    use crate::providers::redmine::planning::resolve_planning;
    // 0% is a valid default state; only values above 100 are rejected.
    let dispatcher = ProviderDispatcher::Redmine(provider("http://127.0.0.1:1".to_owned()));
    let options = PlanningOptions {
        done_ratio: Some("0".to_owned()),
        ..Default::default()
    };
    let resolved = resolve_planning(&dispatcher, &options).unwrap();
    assert_eq!(resolved.done_ratio, Some(0));
    let options = PlanningOptions {
        done_ratio: Some("100".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        resolve_planning(&dispatcher, &options).unwrap().done_ratio,
        Some(100)
    );
    let options = PlanningOptions {
        done_ratio: Some("101".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        resolve_planning(&dispatcher, &options).unwrap_err().json()["kind"],
        "config"
    );

    // The accepted boundary value must survive serialization as a numeric 0.
    let planning = crate::providers::redmine::model::IssuePlanning {
        done_ratio: Some(0),
        ..Default::default()
    };
    let (_, request) = one(
        MockResponse::ok(issue_response(29, "Reset", "Body", false, &[])),
        |redmine| redmine.update_body_with_planning(29, "Body", None, &planning),
    );
    support::assert_request(&request, "PUT", "/issues/29.json", None);
    assert!(
        request.contains(r#""issue":{"description":"Body","done_ratio":0}"#),
        "request: {request}"
    );
}

#[test]
fn date_validation_is_strict_yyyy_mm_dd_including_leap_years() {
    use crate::command::PlanningOptions;
    use crate::providers::redmine::planning::resolve_planning;
    let dispatcher = ProviderDispatcher::Redmine(provider("http://127.0.0.1:1".to_owned()));
    let rejected = |date: &str| {
        let options = PlanningOptions {
            start_date: Some(date.to_owned()),
            ..Default::default()
        };
        resolve_planning(&dispatcher, &options)
            .expect_err("malformed date must be rejected")
            .json()["kind"]
            == "config"
    };
    // Non-zero-padded forms must not reach the server.
    assert!(rejected("2026-1-1"));
    assert!(rejected("2026-01-1"));
    assert!(rejected("26-01-01"));
    assert!(rejected("2026/08/01"));
    assert!(rejected("not-a-date"));
    // Impossible calendar dates are rejected locally.
    assert!(rejected("2026-13-01"));
    assert!(rejected("2026-00-10"));
    assert!(rejected("2026-02-30"));
    assert!(rejected("2026-02-31"));
    assert!(rejected("2026-04-31"));
    assert!(rejected("2026-02-29")); // non-leap year
    assert!(rejected("2100-02-29")); // century non-leap year

    // Real dates — including leap days — are accepted and passed through.
    for valid in [
        "2024-02-29",
        "2000-02-29",
        "2026-12-31",
        "2026-04-30",
        "2026-08-25",
    ] {
        let options = PlanningOptions {
            start_date: Some(valid.to_owned()),
            ..Default::default()
        };
        let resolved = resolve_planning(&dispatcher, &options)
            .unwrap_or_else(|error| panic!("{valid} must be accepted: {error:?}"));
        assert_eq!(resolved.start_date.as_deref(), Some(valid));
    }
}

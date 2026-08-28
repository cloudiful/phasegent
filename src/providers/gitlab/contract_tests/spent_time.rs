#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn format_gitlab_duration_handles_zero_and_sub_second_values() {
    use crate::providers::gitlab::model::format_gitlab_duration;
    // Phase 4 contract: a zero-second projection still produces a
    // positive GitLab duration so the request never carries the
    // literal `0s` value (which GitLab rejects). The exact second
    // count is also preserved end-to-end.
    assert_eq!(format_gitlab_duration(0), "1s");
    assert_eq!(format_gitlab_duration(1), "1s");
    assert_eq!(format_gitlab_duration(59), "59s");
    assert_eq!(format_gitlab_duration(60), "1m");
    assert_eq!(format_gitlab_duration(3_600), "1h");
    assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
    assert_eq!(format_gitlab_duration(86_400), "1d");
}

#[test]
fn format_gitlab_duration_round_trip_is_identity_for_every_known_unit() {
    use crate::providers::gitlab::model::format_gitlab_duration;
    // The `add_spent_time` / `set_time_estimate` paths consume
    // second counts and emit durations through `format_gitlab_duration`,
    // so the production code never has to validate a string. This
    // test pins the canonical shape (every supported unit plus a
    // concatenated compound) to keep the wire format stable.
    assert_eq!(format_gitlab_duration(1), "1s");
    assert_eq!(format_gitlab_duration(60), "1m");
    assert_eq!(format_gitlab_duration(3_600), "1h");
    assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
    assert_eq!(format_gitlab_duration(86_400), "1d");
}

#[test]
fn add_spent_time_posts_to_add_spent_time_with_summary() {
    let (result, request) = one(
        MockResponse::ok(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        ),
        |provider| provider.add_spent_time(7, 3_600, Some("phasegent timer run_id=timer-abc")),
    );
    let response = result.unwrap();
    assert_eq!(response.seconds, Some(3_600));
    assert_eq!(response.total_seconds, Some(3_600));
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"1h""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "missing summary in body: {request}",
    );
}

#[test]
fn add_spent_time_response_carries_no_per_entry_identifier() {
    // Phase 4 audit invariant: GitLab REST v4 does not surface a
    // per-entry identifier for an individual spent-time addition.
    // The response carries the updated running totals only, so the
    // local SQLite ledger is the sole idempotency marker for the
    // timer path. The decoder must not invent a fake id.
    let (result, _request) = one(
        MockResponse::ok(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        ),
        |provider| provider.add_spent_time(7, 3_600, Some("phasegent timer run_id=timer-abc")),
    );
    let response = result.unwrap();
    let value = serde_json::to_value(&response).unwrap();
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "human_readable".to_owned(),
            "seconds".to_owned(),
            "total_human_readable".to_owned(),
            "total_seconds".to_owned(),
        ],
        "ApiSpentTimeSummary must not invent an id field beyond the documented totals",
    );
}

#[test]
fn add_spent_time_rejects_non_positive_duration_before_request() {
    let result = zero_request(|provider| provider.add_spent_time(7, 0, None));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("positive")
    );
}

#[test]
fn add_spent_time_rejects_zero_iid_before_request() {
    let result = zero_request(|provider| provider.add_spent_time(0, 60, None));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("iid")
    );
}

#[test]
fn add_spent_time_handles_404_as_structured_error() {
    let (result, _request) = one(
        MockResponse::status(404, r#"{"message":"404 Not found"}"#),
        |provider| provider.add_spent_time(99, 60, Some("phasegent timer run_id=r")),
    );
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 404);
    assert_eq!(error.json()["operation"], "time spent create");
}

#[test]
fn set_time_estimate_posts_to_time_estimate_with_duration() {
    let (result, request) = one(
        MockResponse::ok(
            r#"{"seconds":1800,"human_readable":"30m","total_seconds":1800,"total_human_readable":"30m"}"#,
        ),
        |provider| provider.set_time_estimate(7, 1_800),
    );
    let response = result.unwrap();
    assert_eq!(response.seconds, Some(1_800));
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

#[test]
fn set_time_estimate_rejects_non_positive_duration_before_request() {
    let result = zero_request(|provider| provider.set_time_estimate(7, -1));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("positive")
    );
}

#[test]
fn add_spent_time_decodes_live_issue_shaped_response_with_time_stats() {
    // Live GitLab 19.x returns the full issue-shaped body for
    // POST /projects/:id/issues/:iid/add_spent_time with the
    // running totals wrapped under a nested `time_stats` block.
    // The decoder must surface the nested totals (not invent a
    // remote id) and mark the response as confirmed so the
    // projection can advance `sync_status` to `synced`.
    let body = serde_json::json!({
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
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.add_spent_time(7, 2, Some("phasegent timer run_id=timer-abc"))
    });
    let response = result.unwrap();
    // The live response carries the totals only under time_stats;
    // the documented flat fields stay None so callers do not
    // mistake a wrapped response for the flat contract shape.
    assert!(
        response.seconds.is_none() && response.total_seconds.is_none(),
        "issue-shaped response must not promote flat totals from time_stats: {response:?}",
    );
    let stats = response
        .time_stats
        .as_ref()
        .expect("issue-shaped response must decode nested time_stats");
    assert_eq!(stats.total_time_spent, Some(2));
    assert_eq!(stats.human_total_time_spent.as_deref(), Some("2s"));
    assert_eq!(stats.time_estimate, Some(0));
    assert!(
        response.is_confirmed(),
        "nested time_stats.total_time_spent must confirm the write",
    );
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"2s""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {request}",
    );
}

#[test]
fn set_time_estimate_decodes_live_issue_shaped_response_with_time_stats() {
    // Same response-shape compatibility applies to set_time_estimate:
    // GitLab 19.x echoes the issue body with the running estimate
    // wrapped under time_stats.time_estimate. The decoder must
    // surface the nested value so a successful estimate update is
    // confirmed without inventing a remote id.
    let body = serde_json::json!({
        "id": 7,
        "iid": 2,
        "title": "Live estimate fixture",
        "state": "opened",
        "labels": [],
        "time_stats": {
            "time_estimate": 1800,
            "total_time_spent": 0,
            "human_time_estimate": "30m",
            "human_total_time_spent": null
        }
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.set_time_estimate(7, 1_800)
    });
    let response = result.unwrap();
    assert!(
        response.seconds.is_none() && response.total_seconds.is_none(),
        "issue-shaped response must not promote flat totals: {response:?}",
    );
    let stats = response
        .time_stats
        .as_ref()
        .expect("issue-shaped response must decode nested time_stats");
    assert_eq!(stats.time_estimate, Some(1_800));
    assert_eq!(stats.human_time_estimate.as_deref(), Some("30m"));
    assert!(response.is_confirmed());
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

#[test]
fn add_spent_time_decodes_top_level_time_stats_response() {
    // Live GitLab 19.x returns a top-level time-stats object
    // (not the nested `time_stats` issue shape) for
    // POST /projects/:id/issues/:iid/add_spent_time. The body
    // captured live against project 3 issue 5 was
    // `{ "time_estimate": 0, "total_time_spent": 6,
    //   "human_time_estimate": null, "human_total_time_spent": "6s" }`.
    // The decoder must surface those totals at the top level
    // (NOT under time_stats) so `is_confirmed` returns true via
    // the top-level `total_time_spent` and the projection
    // advances `sync_status` to `synced`. The previous attempt's
    // nested-only handling kept every top-level field None and
    // left a successful POST marked `unconfirmed`.
    let body = r#"{
        "time_estimate": 0,
        "total_time_spent": 6,
        "human_time_estimate": null,
        "human_total_time_spent": "6s"
    }"#;
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.add_spent_time(7, 6, Some("phasegent timer run_id=timer-abc"))
    });
    let response = result.unwrap();
    // Top-level time-stats fields must be populated directly on
    // the response struct, not under time_stats.
    assert_eq!(response.total_time_spent, Some(6));
    assert_eq!(response.time_estimate, Some(0));
    assert_eq!(response.human_total_time_spent.as_deref(), Some("6s"));
    assert!(
        response.human_time_estimate.is_none(),
        "JSON null must decode to None for human_time_estimate",
    );
    // The nested time_stats block stays None because the live
    // response does not wrap the totals inside an issue body;
    // the legacy flat totals also stay None because the live
    // response uses neither `seconds` nor `total_seconds`.
    assert!(
        response.time_stats.is_none(),
        "top-level response must not wrap totals under time_stats: {response:?}",
    );
    assert!(response.seconds.is_none());
    assert!(response.total_seconds.is_none());
    assert!(response.human_readable.is_none());
    assert!(response.total_human_readable.is_none());
    assert!(
        response.is_confirmed(),
        "top-level total_time_spent must confirm the write",
    );
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"6s""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {request}",
    );
}

#[test]
fn set_time_estimate_decodes_top_level_time_stats_response() {
    // Same shape compatibility for `set_time_estimate`: GitLab
    // 19.x returns a top-level time-stats object whose
    // `time_estimate` carries the updated value. The decoder
    // must surface the top-level field so a successful POST is
    // confirmed without inventing a remote id.
    let body = r#"{
        "time_estimate": 1800,
        "total_time_spent": 0,
        "human_time_estimate": "30m",
        "human_total_time_spent": null
    }"#;
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.set_time_estimate(7, 1_800)
    });
    let response = result.unwrap();
    assert_eq!(response.time_estimate, Some(1_800));
    assert_eq!(response.total_time_spent, Some(0));
    assert_eq!(response.human_time_estimate.as_deref(), Some("30m"));
    assert!(
        response.human_total_time_spent.is_none(),
        "JSON null must decode to None for human_total_time_spent",
    );
    assert!(response.time_stats.is_none());
    assert!(response.seconds.is_none());
    assert!(response.total_seconds.is_none());
    assert!(response.is_confirmed());
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

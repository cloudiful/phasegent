#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn update_body_with_tracker_replaces_bug_with_feature() {
    // Switching from Bug to Feature must remove the type::bug label
    // so the issue never carries both managed tracker labels.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(50, "type::feature")),
        MockResponse::ok(issue_payload(60, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(60, "Title", "opened", &["type::feature"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    provider
        .update_body_with_labels(60, "Switched to feature", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/60"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/60"));
    assert!(requests[3].contains(r#""add_labels":["type::feature"]"#));
    assert!(
        requests[3].contains(r#""remove_labels":["type::bug"]"#),
        "expected remove_labels to drop the prior bug tracker: {}",
        requests[3],
    );
    assert!(
        !requests[3].contains(r#""add_labels":["type::bug"]"#),
        "the payload must not re-add the dropped bug tracker: {}",
        requests[3],
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_replaces_feature_with_bug() {
    // Mirror of the bug->feature case: switching from Feature to
    // Bug must drop the existing type::feature label.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(51, "type::bug")),
        MockResponse::ok(issue_payload(61, "Title", "opened", &["type::feature"])),
        MockResponse::ok(issue_payload(61, "Title", "opened", &["type::bug"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    provider
        .update_body_with_labels(61, "Switched to bug", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/61"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/61"));
    assert!(requests[3].contains(r#""add_labels":["type::bug"]"#));
    assert!(
        requests[3].contains(r#""remove_labels":["type::feature"]"#),
        "expected remove_labels to drop the prior feature tracker: {}",
        requests[3],
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_preserves_unrelated_labels() {
    // Workflow labels and unrelated project labels must survive a
    // tracker swap. Only the opposite managed tracker label is
    // removed; everything else is left alone.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(52, "type::feature")),
        MockResponse::ok(issue_payload(
            62,
            "Title",
            "opened",
            &["type::bug", "workflow::in-review", "frontend"],
        )),
        MockResponse::ok(issue_payload(
            62,
            "Title",
            "opened",
            &["type::feature", "workflow::in-review", "frontend"],
        )),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    provider
        .update_body_with_labels(62, "Updated", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    let put = &requests[3];
    assert!(put.contains(r#""add_labels":["type::feature"]"#));
    assert!(
        put.contains(r#""remove_labels":["type::bug"]"#),
        "expected the only removal to be the prior bug tracker: {put}",
    );
    // Workflow and unrelated project labels are not touched.
    assert!(
        !put.contains(r#""remove_labels":["workflow::in-review"]"#),
        "workflow label must not be removed by a tracker swap: {put}",
    );
    assert!(
        !put.contains(r#""remove_labels":["frontend"]"#),
        "unrelated project labels must not be removed by a tracker swap: {put}",
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_keeps_same_tracker_idempotent() {
    // Setting the same tracker the issue already carries must not
    // remove any label. add_labels is a no-op for an already
    // attached label, and remove_labels stays empty because the
    // opposite tracker is not currently attached.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(53, "type::bug")),
        MockResponse::ok(issue_payload(63, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(63, "Title", "opened", &["type::bug"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    provider
        .update_body_with_labels(63, "Body", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    let put = &requests[3];
    assert!(put.contains(r#""add_labels":["type::bug"]"#));
    assert!(
        !put.contains(r#""remove_labels":["type::feature"]"#),
        "must not drop the opposite tracker when only the same tracker is requested: {put}",
    );
    assert!(
        !put.contains(r#""remove_labels":["type::bug"]"#),
        "must not drop the requested tracker itself: {put}",
    );
    server.join().unwrap();
}

#[test]
fn close_issue_pairs_state_event_close_with_workflow_closed_label() {
    let (base, requests, server) = sequence(vec![
        // First: ensure workflow::closed label exists. The label
        // endpoint returns an empty list so the provider creates it.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        // Second: the create label POST.
        MockResponse::ok(label_payload(1, "workflow::closed")),
        // Third: GET the current issue so the provider can decide
        // whether state_event=close is required. The issue is open
        // here, so a close transition is needed.
        MockResponse::ok(issue_payload(15, "Title", "opened", &[])),
        // Fourth: the close PUT on the issue.
        MockResponse::ok(issue_payload(15, "Title", "closed", &["workflow::closed"])),
    ]);
    let provider = provider(base);
    let issue = provider.close_issue(15).unwrap();
    assert_eq!(issue.number, 15);
    assert_eq!(issue.state, "closed");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/15"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/15"));
    assert!(requests[3].contains(r#""state_event":"close""#));
    assert!(requests[3].contains(r#""add_labels":["workflow::closed"]"#));
    // Every other managed workflow label must be in the remove list.
    for label in [
        "workflow::new",
        "workflow::in-progress",
        "workflow::in-review",
        "workflow::changes-requested",
        "workflow::blocked",
        "workflow::resolved",
        "workflow::cancelled",
    ] {
        assert!(
            requests[3].contains(&format!(r#""{}""#, label)),
            "close payload missing remove_label for {label}: {}",
            requests[3]
        );
    }
    server.join().unwrap();
}

#[test]
fn reopen_for_non_closed_status_emits_state_event_reopen() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(2, "workflow::in-review")),
        // GET current issue: the issue is currently closed so the
        // provider must emit state_event=reopen to transition it
        // back to the open state alongside the workflow label swap.
        MockResponse::ok(issue_payload(21, "Title", "closed", &["workflow::new"])),
        MockResponse::ok(issue_payload(
            21,
            "Title",
            "opened",
            &["workflow::in-review"],
        )),
    ]);
    let provider = provider(base);
    let issue = provider.set_workflow_status(21, "InReview").unwrap();
    assert_eq!(issue.state, "open");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/21"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/21"));
    assert!(requests[3].contains(r#""state_event":"reopen""#));
    assert!(requests[3].contains(r#""add_labels":["workflow::in-review"]"#));
    server.join().unwrap();
}

#[test]
fn status_set_open_to_open_omits_state_event() {
    // GitLab REST v4 rejects state_event=reopen on an already-open
    // issue with HTTP 400. Setting an open workflow status on an
    // open issue must omit state_event entirely so repeated
    // `status set` calls remain idempotent.
    let (base, requests, server) = sequence(vec![
        // Ensure workflow::new label exists.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(5, "workflow::new")),
        // GET current issue: open.
        MockResponse::ok(issue_payload(22, "Title", "opened", &[])),
        // PUT response after the label swap.
        MockResponse::ok(issue_payload(22, "Title", "opened", &["workflow::new"])),
    ]);
    let provider = provider(base);
    let issue = provider.set_workflow_status(22, "New").unwrap();
    assert_eq!(issue.state, "open");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/22"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/22"));
    assert!(
        !requests[3].contains("state_event"),
        "open->open must omit state_event: {}",
        requests[3],
    );
    assert!(requests[3].contains(r#""add_labels":["workflow::new"]"#));
    server.join().unwrap();
}

#[test]
fn close_issue_already_closed_omits_state_event() {
    // GitLab REST v4 rejects state_event=close on an already-closed
    // issue with HTTP 400. The provider must omit state_event when
    // no state transition is required so the close path stays
    // idempotent.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(6, "workflow::closed")),
        // GET current issue: already closed.
        MockResponse::ok(issue_payload(23, "Title", "closed", &["workflow::closed"])),
        // PUT response after the (now idempotent) label refresh.
        MockResponse::ok(issue_payload(23, "Title", "closed", &["workflow::closed"])),
    ]);
    let provider = provider(base);
    let issue = provider.close_issue(23).unwrap();
    assert_eq!(issue.state, "closed");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/23"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/23"));
    assert!(
        !requests[3].contains("state_event"),
        "closed->closed must omit state_event: {}",
        requests[3],
    );
    assert!(requests[3].contains(r#""add_labels":["workflow::closed"]"#));
    server.join().unwrap();
}

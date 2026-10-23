//! Redmine wire contract for structured records.
//!
//! Records ride on the ordinary journal primitives, so these tests pin
//! the provider-specific facts the record protocol depends on: the native
//! journal id is the record id and anchors `#change-<id>`, a plain `204 No
//! Content` write leaves the result unknown so the service recovers by
//! reading, and a write is only confirmed by an exact decoded record read.
//!
//! The scenarios are split into cohesive child modules; the shared
//! `spec`/`stored` fixtures live in [`fixtures`].

mod confirmation;
mod error;
mod fixtures;
mod native;
mod read;
mod replay;

#[test]
fn journal_pages_filter_records_before_count_and_slice_without_offset_requests() {
    use super::support::{MockResponse, issue_response, one};
    use crate::policy::Role;
    use crate::record::{self, RecordFilter, RecordKind};
    let first = fixtures::stored(
        &fixtures::spec(RecordKind::Executor, Role::Executor, "first"),
        "first body",
    );
    let review = fixtures::stored(
        &fixtures::spec(RecordKind::Reviewer, Role::Reviewer, "review"),
        "review body",
    );
    let second = fixtures::stored(
        &fixtures::spec(RecordKind::Executor, Role::Executor, "second"),
        "second body",
    );
    let body = issue_response(
        21,
        "Title",
        "Body",
        false,
        &[
            (501, "ordinary"),
            (502, &first),
            (503, &review),
            (504, &second),
        ],
    );
    let (result, request) = one(MockResponse::ok(body.clone()), |provider| {
        record::list_page(
            provider,
            21,
            RecordFilter {
                kind: Some(RecordKind::Executor),
                ..Default::default()
            },
            crate::pagination::PageRequest::new(2, 1).unwrap(),
        )
    });
    let page = result.unwrap();
    assert_eq!(page.total_count, Some(2));
    assert_eq!(page.items[0].id, 504);
    assert!(!page.has_more);
    assert!(request.contains("include=journals"));
    assert!(!request.contains("offset="));
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.list_comments_page(21, crate::pagination::PageRequest::new(2, 2).unwrap())
    });
    let page = result.unwrap();
    assert_eq!(page.total_count, Some(4));
    assert_eq!(page.items[0].id, 503);
    assert!(!page.has_more);
    assert!(!request.contains("offset="));
}

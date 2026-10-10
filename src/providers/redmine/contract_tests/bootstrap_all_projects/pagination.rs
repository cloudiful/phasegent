use super::{empty_memberships, projects_response, service_roles_payload};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    seed_admin_only, temp_db, user_list_empty,
};
use crate::providers::redmine::contract_tests::support::MockResponse;
use crate::providers::redmine::contract_tests::support::sequence;
use std::fs;

#[test]
fn incomplete_empty_project_page_fails_before_any_write() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-pagination-incomplete");
    // A positive total_count with an empty first page is an incomplete
    // enumeration, not a silent complete set.
    let (base, requests, server) = sequence(vec![projects_response(3, 100, &[])]);
    seed_admin_only(&storage, &base);
    let error = crate::workflow::bootstrap_all_projects(None, true).unwrap_err();
    assert_eq!(error.json()["kind"], "pagination");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 1, "only enumeration ran: {reqs:?}");
    assert!(reqs[0].starts_with("GET /projects.json?"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn repeated_project_page_fails_before_any_write() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-pagination-repeated");
    let (base, requests, server) = sequence(vec![
        projects_response(2, 1, &[(44, "Workflow", "workflow")]),
        projects_response(2, 1, &[(44, "Workflow", "workflow")]),
    ]);
    seed_admin_only(&storage, &base);
    let error = crate::workflow::bootstrap_all_projects(None, false).unwrap_err();
    assert_eq!(error.json()["kind"], "pagination");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert!(reqs.iter().all(|request| request.starts_with("GET ")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn complete_multi_page_project_set_is_sorted_and_included_in_preview() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-pagination-complete");
    let (base, requests, server) = sequence(vec![
        projects_response(2, 1, &[(22, "Second", "second")]),
        projects_response(2, 1, &[(11, "First", "first")]),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(service_roles_payload()),
        empty_memberships(),
        empty_memberships(),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(report.ready);
    assert_eq!(
        report
            .projects
            .iter()
            .map(|project| project.project_id)
            .collect::<Vec<_>>(),
        [11, 22]
    );
    assert_eq!(report.summary.projects, 2);
    assert_eq!(report.summary.planned, 8);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 9, "{reqs:?}");
    assert!(reqs[0].contains("offset=0"));
    assert!(reqs[1].contains("offset=1"));
    assert!(reqs.iter().all(|request| !request.starts_with("POST")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

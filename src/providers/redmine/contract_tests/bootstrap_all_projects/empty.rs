use super::projects_response;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{seed_admin_only, temp_db};
use crate::providers::redmine::contract_tests::support::sequence;
use std::fs;

#[test]
fn empty_project_set_is_a_ready_no_op() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-empty-noop");
    let (base, requests, server) = sequence(vec![projects_response(0, 100, &[])]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(report.ready);
    assert_eq!(report.exit_code(), 0);
    assert!(report.projects.is_empty());
    assert_eq!(report.summary.projects, 0);
    assert_eq!(report.roles.len(), 4);
    assert!(report.roles.iter().all(|role| role.status == "planned"));

    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 1, "only enumeration runs: {reqs:?}");
    assert!(reqs[0].starts_with("GET /projects.json?"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

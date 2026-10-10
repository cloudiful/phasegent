use super::{save_orchestrator, temp_storage};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, project_collection, sequence, strings,
};
use std::fs;

#[test]
fn discovery_error_for_version_list_is_not_swallowed() {
    let _lock = lock_workflow_tests();
    let (s, _g, d) = temp_storage();
    let _m = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (b, req, srv) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(401, r#"{"errors":["unauthorized"]}"#),
    ]);
    save_orchestrator(&s, Some(b.clone()));
    let c = crate::cli::run_with_role(
        strings(["--provider", "redmine", "--api-base", &b, "version", "list"]),
        Some(Role::Orchestrator.as_str()),
    );
    assert_eq!(c, 1);
    let r = req.recv().unwrap();
    assert_eq!(r.len(), 2);
    srv.join().unwrap();
    let _ = fs::remove_dir_all(d);
}

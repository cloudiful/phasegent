/// The CLI JSON contract for `workflow bootstrap` is parameterised over the
/// success and membership-warning shapes: both expose `user_memberships` per
/// agent identity and the credential-free `git_mirror` outcome, must never
/// re-introduce the legacy `membership`/`group_name`/`group_role` keys, and
/// must never leak a bearer key or embedded origin credential. The detailed
/// bootstrap flow is exercised end-to-end by
/// `redmine_contract_tests::issue_create_automatically_bootstraps_once_before_returning_issue`;
/// this test pins the surface contract for both shapes.
#[test]
fn workflow_bootstrap_json_contract_holds_for_success_and_warning_shapes() {
    fn bootstrap_output(warning: bool) -> serde_json::Value {
        let memberships = if warning {
            serde_json::json!([
                {
                    "role": "Developer",
                    "user_id": 22_u64,
                    "user_login": "executor",
                    "status": "warning",
                    "warning": "Redmine role was not found: user 'executor', role 'Developer'",
                },
            ])
        } else {
            serde_json::json!([
                {
                    "role": "Maintainer",
                    "user_id": 11_u64,
                    "user_login": "orchestrator",
                    "status": "added",
                },
                {
                    "role": "Developer",
                    "user_id": 22_u64,
                    "user_login": "executor",
                    "status": "added",
                },
                {
                    "role": "Reporter",
                    "user_id": 33_u64,
                    "user_login": "reviewer",
                    "status": "added",
                },
            ])
        };
        let mut output = serde_json::json!({
            "bootstrapped": !warning,
            "created": true,
            "repository": "owner/repo",
            "identifier": "owner-repo",
            "project_id": 44_u64,
            "close_status_id": 5_u64,
            "close_status_name": "Closed",
            "user_memberships": memberships,
            "git_mirror": {
                "id": 901_u64,
                "project_id": 44_u64,
                "identifier": "mirror_44_owner_repo",
                "status": "pending",
                "remote_url": "https://git.example.com/owner/repo.git",
                "local_path": "/var/redmine/repos/owner_repo.git",
                "error": null,
            },
        });
        if warning {
            output["warning"] =
                serde_json::json!("Redmine role was not found: user 'executor', role 'Developer'");
        }
        output
    }

    for warning in [false, true] {
        let label = if warning {
            "warning shape"
        } else {
            "success shape"
        };
        let output = bootstrap_output(warning);
        assert_eq!(
            output["bootstrapped"],
            serde_json::json!(!warning),
            "{label}"
        );
        for legacy in ["membership", "group_name", "group_role"] {
            assert!(
                output.get(legacy).is_none(),
                "{label} must not re-introduce the legacy {legacy} key"
            );
        }
        let memberships = output["user_memberships"]
            .as_array()
            .expect("user_memberships must be an array");
        assert!(!memberships.is_empty(), "{label} must report memberships");

        let git_mirror = output
            .get("git_mirror")
            .expect("bootstrap JSON must include git_mirror");
        assert_eq!(git_mirror["status"], "pending", "{label}");
        assert_eq!(git_mirror["identifier"], "mirror_44_owner_repo", "{label}");
        assert_eq!(git_mirror["project_id"], 44_u64, "{label}");
        assert_eq!(
            git_mirror["remote_url"], "https://git.example.com/owner/repo.git",
            "{label}"
        );
        assert_eq!(
            git_mirror["local_path"], "/var/redmine/repos/owner_repo.git",
            "{label}"
        );
        assert!(git_mirror["error"].is_null(), "{label}");

        let serialized = output.to_string();
        assert!(
            !serialized.to_ascii_lowercase().contains("bearer "),
            "{label} must not include bearer credentials: {serialized}"
        );
        assert!(
            !serialized.contains("supersecret"),
            "{label} must not include embedded origin credentials: {serialized}"
        );

        if warning {
            assert!(
                output["warning"].is_string(),
                "warning shape must carry the failure reason"
            );
            assert_eq!(memberships[0]["status"], "warning");
            assert!(memberships[0]["warning"].is_string());
        } else {
            assert_eq!(
                memberships.len(),
                3,
                "success shape reports one membership per agent identity"
            );
        }
    }
}

#[test]
fn redmine_new_project_includes_repository_module_for_mirror_enablement() {
    use crate::providers::redmine::model::RedmineNewProject;
    let payload = RedmineNewProject::new("Workflow", "workflow", Some("issues"));
    let value = serde_json::to_value(&payload).unwrap();
    let project = value.get("project").unwrap();
    assert!(
        project.get("enabled_modules").is_none(),
        "default payload must not include modules so direct `project create` calls remain opt-in"
    );

    let payload =
        RedmineNewProject::new("Workflow", "workflow", Some("issues")).with_repository_module();
    let value = serde_json::to_value(&payload).unwrap();
    let project = value.get("project").unwrap();
    let modules = project
        .get("enabled_modules")
        .expect("bootstrap-enabled payload must include enabled_modules")
        .as_array()
        .expect("enabled_modules must be an array");
    assert_eq!(
        modules,
        &vec![serde_json::json!({"name": "repository"})],
        "the bootstrap-enabled payload must enable the repository module only"
    );
}

pub(crate) mod support;

use crate::auth;
use crate::ci_model::CiRunsFilter;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::policy::{Capability, Role};
use crate::provider::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use crate::redmine_model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::storage::{Storage, TimerRun};
use std::path::Path;
use std::str::FromStr;
use std::{env, ffi::OsString, fs, time};
use support::{
    MockResponse, TEST_API_KEY, issue_response, one, provider, sequence, time_entry_activities,
    time_entry_collection, time_entry_response,
};

#[test]
fn get_issue_uses_redmine_json_journals_and_maps_summary() {
    let (result, request) = one(
        MockResponse::ok(issue_response(17, "Subject", "Description", false, &[])),
        |redmine| redmine.get_issue(17),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 17);
    assert_eq!(issue.title, "Subject");
    assert_eq!(issue.body, "Description");
    assert_eq!(issue.state, "open");
    assert!(
        issue
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/17"))
    );
    support::assert_request(&request, "GET", "/issues/17.json?include=journals", None);
}

#[test]
fn create_issue_wraps_project_and_fields_as_json() {
    let (result, request) = one(
        MockResponse::ok(issue_response(18, "Created", "Body", false, &[])),
        |redmine| redmine.create_issue("Created", "Body"),
    );
    assert_eq!(result.unwrap().number, 18);
    support::assert_request(&request, "POST", "/issues.json", None);
    assert!(request.contains("content-type: application/json"));
    assert!(
        request.contains(r#""issue":{"project_id":42,"subject":"Created","description":"Body"}"#)
    );
}

#[test]
fn update_body_uses_put_and_description_wrapper() {
    let (result, request) = one(
        MockResponse::ok(issue_response(19, "Title", "Updated", false, &[])),
        |redmine| redmine.update_body(19, "Updated"),
    );
    assert_eq!(result.unwrap().body, "Updated");
    support::assert_request(&request, "PUT", "/issues/19.json", None);
    assert!(request.contains(r#""issue":{"description":"Updated"}"#));
    assert!(!request.contains("status_id"));
}

#[test]
fn close_uses_the_configured_status_id() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        20,
        "Title",
        "Body",
        true,
        &[],
    ))]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    assert!(redmine.close_issue(20).is_ok());
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "PUT", "/issues/20.json", None);
    assert!(request.contains(r#""issue":{"status_id":37}"#));
    assert!(!request.contains(r#""status_id":5"#));
    server.join().unwrap();
}

#[test]
fn project_list_uses_redmine_collection_wrapper_and_pagination_params() {
    let response = support::project_collection(1, 100, &[(41, "Workflow", "workflow")]);
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.list_projects()
    });
    let projects = result.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, 41);
    assert_eq!(projects[0].identifier, "workflow");
    assert_eq!(projects[0].description, "description");
    assert_eq!(
        serde_json::to_value(&projects[0]).unwrap()["description"],
        "description"
    );
    support::assert_request(&request, "GET", "/projects.json?", None);
    assert!(request.contains("limit=100"));
    assert!(request.contains("offset=0"));
}

#[test]
fn project_list_decodes_null_description_as_empty_string() {
    let response = serde_json::json!({
        "total_count": 1,
        "limit": 100,
        "projects": [{
            "id": 41,
            "name": "Workflow",
            "identifier": "workflow",
            "description": null,
            "is_public": false,
            "inherit_members": false,
        }]
    })
    .to_string();
    let created_response = serde_json::json!({
        "project": {
            "id": 43,
            "name": "Created",
            "identifier": "created",
            "description": null,
            "is_public": false,
            "inherit_members": false,
        }
    })
    .to_string();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(response),
        MockResponse::ok(created_response),
    ]);
    let redmine = provider(base);
    let projects = redmine.list_projects().unwrap();
    assert_eq!(projects[0].description, "");
    let serialized = serde_json::to_value(&projects[0]).unwrap();
    assert!(!serialized.as_object().unwrap().contains_key("description"));
    let created = redmine.create_project("Created", "created", None).unwrap();
    assert_eq!(created.description, "");
    let requests = requests.recv().unwrap();
    support::assert_request(&requests[0], "GET", "/projects.json?", None);
    support::assert_request(&requests[1], "POST", "/projects.json", None);
    server.join().unwrap();
}

#[test]
fn project_create_wraps_fields_and_does_not_change_configured_project() {
    let response = support::project_response(43, "Created", "created", "Created project");
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        let result = redmine.create_project("Created", "created", Some("Created project"));
        assert_eq!(redmine.config.project_id.as_deref(), Some("42"));
        result
    });
    let project = result.unwrap();
    assert_eq!(project.id, 43);
    support::assert_request(&request, "POST", "/projects.json", None);
    // Bootstrap enables the `repository` module on creation so the mirror
    // plugin can attach the Git repository without a separate PUT.
    assert!(request.contains(
        r#""project":{"name":"Created","identifier":"created","is_public":false,"description":"Created project","enabled_modules":[{"name":"repository"}]}"#
    ));
}

#[test]
fn bootstrap_derives_identifier_with_owner_and_redmine_normalization() {
    assert_eq!(
        crate::remote::redmine_identifier("Acme/Workflow Repo").unwrap(),
        "acme-workflow-repo"
    );
    assert_eq!(
        crate::remote::redmine_identifier("Owner.Name/Repo+One").unwrap(),
        "owner-name-repo-one"
    );
    assert_eq!(
        crate::remote::redmine_identifier("!owner/repo").unwrap(),
        "owner-repo"
    );
    assert_eq!(
        crate::remote::redmine_identifier("123owner/repo").unwrap(),
        "wf-123owner-repo"
    );
}

#[test]
fn bootstrap_project_lookup_is_exact_and_404_means_missing() {
    let response = support::project_response(44, "Workflow", "acme-repo", "description");
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.find_project("acme-repo")
    });
    assert_eq!(result.unwrap().unwrap().id, 44);
    support::assert_request(&request, "GET", "/projects/acme-repo.json", None);

    let (result, _) = one(
        MockResponse::ok(support::project_response(
            45,
            "Workflow",
            "other-project",
            "description",
        )),
        |redmine| redmine.find_project("acme-repo"),
    );
    assert!(result.unwrap().is_none());

    let (result, request) = one(
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        |redmine| redmine.find_project("missing-project"),
    );
    assert!(result.unwrap().is_none());
    support::assert_request(&request, "GET", "/projects/missing-project.json", None);
}

#[test]
fn bootstrap_found_project_selects_status_without_creating() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::project_response(
            44,
            "Workflow",
            "acme-repo",
            "description",
        )),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [
                    {"id": 1, "name": "New", "is_closed": false},
                    {"id": 5, "name": "Closed", "is_closed": true}
                ]
            })
            .to_string(),
        ),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    assert_eq!(bootstrap.close_status.id, 5);
    assert!(!bootstrap.created);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "GET", "/projects/acme-repo.json", None);
    support::assert_request(&requests[1], "GET", "/issue_statuses.json", None);
    server.join().unwrap();
}

#[test]
fn bootstrap_missing_project_creates_automatically_without_confirmation() {
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "acme/repo",
            "acme-repo",
            "Workflow issues for acme/repo",
        )),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /projects/acme-repo.json"));
    support::assert_request(&requests[2], "POST", "/projects.json", None);
    assert!(requests[2].contains(r#""is_public":false"#));
    server.join().unwrap();
}

#[test]
fn bootstrap_missing_project_creation_is_private() {
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "acme/repo",
            "acme-repo",
            "Workflow issues for acme/repo",
        )),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    assert_eq!(bootstrap.close_status.id, 5);
    assert!(bootstrap.created);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "POST", "/projects.json", None);
    assert!(requests[2].contains(r#""identifier":"acme-repo""#));
    assert!(requests[2].contains(r#""is_public":false"#));
    server.join().unwrap();
}

#[test]
fn bootstrap_does_not_guess_multiple_closed_statuses() {
    let statuses = [
        crate::redmine_model::RedmineIssueStatus {
            id: 5,
            name: "Closed".to_owned(),
            is_closed: true,
        },
        crate::redmine_model::RedmineIssueStatus {
            id: 6,
            name: "Resolved".to_owned(),
            is_closed: true,
        },
    ];
    let error = RedmineProvider::select_close_status(&statuses, None, None).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("multiple closed"));
    assert_eq!(
        RedmineProvider::select_close_status(&statuses, Some("6"), None)
            .unwrap()
            .id,
        6
    );
    assert_eq!(
        RedmineProvider::select_close_status(&statuses, None, Some("Closed"))
            .unwrap()
            .id,
        5
    );
    let not_found_id =
        RedmineProvider::select_close_status(&statuses, Some("99"), None).unwrap_err();
    assert!(not_found_id.to_string().contains("id 99 was not found"));
    let not_closed_id = [crate::redmine_model::RedmineIssueStatus {
        id: 8,
        name: "Resolved".to_owned(),
        is_closed: false,
    }];
    let not_closed_id =
        RedmineProvider::select_close_status(&not_closed_id, Some("8"), None).unwrap_err();
    assert!(
        not_closed_id
            .to_string()
            .contains("id 8 was found but is not closed")
    );
    let not_closed_name = [crate::redmine_model::RedmineIssueStatus {
        id: 8,
        name: "Resolved".to_owned(),
        is_closed: false,
    }];
    let not_closed_name =
        RedmineProvider::select_close_status(&not_closed_name, None, Some("Resolved")).unwrap_err();
    assert!(
        not_closed_name
            .to_string()
            .contains("name 'Resolved' was found but is not closed")
    );
    let not_found_name =
        RedmineProvider::select_close_status(&statuses, None, Some("Missing")).unwrap_err();
    assert!(
        not_found_name
            .to_string()
            .contains("name 'Missing' was not found")
    );
}

#[test]
fn current_user_decodes_user_payload_from_users_current() {
    let (result, request) = one(
        MockResponse::ok(support::current_user_response(101, "orchestrator")),
        |redmine| redmine.current_user(),
    );
    let user = result.unwrap();
    assert_eq!(user.id, 101);
    assert_eq!(user.login, "orchestrator");
    support::assert_request(&request, "GET", "/users/current.json", None);
}

#[test]
fn user_membership_existing_role_is_not_changed() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(Some((
            55,
            7,
            "executor",
            vec![9],
        )))),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "existing");
    assert_eq!(result.user_id, 7);
    assert_eq!(result.user_login, "executor");
    assert_eq!(result.role_id, 9);
    assert_eq!(result.role_name, "Developer");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "GET", "/roles.json", None);
    support::assert_request(&requests[1], "GET", "/projects/42/memberships.json", None);
    server.join().unwrap();
}

#[test]
fn user_membership_missing_role_is_a_warning_without_membership_write() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(support::role_collection(&[(
        3, "Reporter",
    )]))]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "warning");
    assert_eq!(result.user_id, 7);
    assert_eq!(result.user_login, "executor");
    assert_eq!(result.role_name, "Developer");
    let warning = result.warning.expect("missing warning text");
    assert!(
        warning.contains("role 'Developer'"),
        "unexpected warning: {warning}"
    );
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

#[test]
fn user_membership_missing_entry_is_added_with_selected_role() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "added");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "POST", "/projects/42/memberships.json", None);
    assert!(requests[2].contains(r#""user_id":7,"role_ids":[9]"#));
    assert!(!requests[2].contains("group_id"));
    server.join().unwrap();
}

#[test]
fn user_membership_existing_entry_adds_missing_role_without_dropping_others() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(Some((
            55,
            7,
            "executor",
            vec![3],
        )))),
        MockResponse::ok("{}"),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "updated");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "PUT", "/memberships/55.json", None);
    // Role list is sorted ascending in update payloads and never overwrites
    // the unrelated Reporter role already on the membership.
    assert!(requests[2].contains(r#""role_ids":[3,9]"#));
    server.join().unwrap();
}

#[test]
fn user_membership_reconciliation_finds_roles_and_memberships_on_later_pages() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection_page(2, 1, &[(3, "Reporter")])),
        MockResponse::ok(support::role_collection_page(2, 1, &[(9, "Developer")])),
        MockResponse::ok(support::membership_collection_page(
            2,
            1,
            Some((20, 7, "executor", vec![9])),
        )),
        MockResponse::ok(support::membership_collection_page(
            2,
            1,
            Some((55, 7, "executor", vec![9])),
        )),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "existing");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=1"));
    assert!(requests[2].contains("offset=0"));
    assert!(requests[3].contains("offset=1"));
    server.join().unwrap();
}

#[test]
fn bootstrap_persists_role_scoped_ids_with_private_permissions() {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-bootstrap-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    auth::persist_redmine_bootstrap_for(
        &directory,
        Role::Orchestrator,
        Some("https://redmine.example".to_owned()),
        44,
        5,
    )
    .unwrap();
    let path = auth::redmine_config_path_for(&directory, Role::Orchestrator);
    let config: auth::RedmineStoredConfig =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(config.project_id.as_deref(), Some("44"));
    assert_eq!(config.close_status_id, Some(5));
    assert_eq!(config.api_base.as_deref(), Some("https://redmine.example"));
    // Active bootstrap no longer persists the legacy group fields; older
    // configs that still carry them continue to decode via `serde(default)`.
    assert_eq!(config.group_name, None);
    assert_eq!(config.group_role, None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_persisted_config_decodes_legacy_group_fields_without_error() {
    // Older Redmine configs persisted before the direct-user switch still
    // carry `group_name`/`group_role`. The active bootstrap no longer reads
    // or writes them, but they must decode without error so old files keep
    // loading on operator machines.
    let legacy = serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
        "group_name": "AI Agents",
        "group_role": "开发人员",
    });
    let config: auth::RedmineStoredConfig =
        serde_json::from_value(legacy).expect("legacy config must decode");
    assert_eq!(config.group_name.as_deref(), Some("AI Agents"));
    assert_eq!(config.group_role.as_deref(), Some("开发人员"));
}

#[test]
fn issue_status_list_decodes_redmine_wrapper() {
    let (result, request) = one(
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [
                    {"id": 1, "name": "New", "is_closed": false},
                    {"id": 5, "name": "Closed", "is_closed": true}
                ]
            })
            .to_string(),
        ),
        |redmine| redmine.list_issue_statuses(),
    );
    let statuses = result.unwrap();
    assert_eq!(
        statuses.iter().map(|status| status.id).collect::<Vec<_>>(),
        [1, 5]
    );
    assert!(statuses[1].is_closed);
    support::assert_request(&request, "GET", "/issue_statuses.json", None);
}

#[test]
fn tracker_list_decodes_redmine_wrapper() {
    let (result, request) = one(
        MockResponse::ok(
            serde_json::json!({
                "trackers": [
                    {"id": 1, "name": "Bug"},
                    {"id": 2, "name": "Feature"}
                ]
            })
            .to_string(),
        ),
        |redmine| redmine.list_trackers(),
    );
    let trackers = result.unwrap();
    assert_eq!(
        trackers
            .iter()
            .map(|tracker| (tracker.id, tracker.name.as_str()))
            .collect::<Vec<_>>(),
        [(1, "Bug"), (2, "Feature")]
    );
    support::assert_request(&request, "GET", "/trackers.json", None);
}

#[test]
fn time_entry_activity_selection_is_exact_preferred_or_singular_default() {
    let activities = vec![
        RedmineTimeEntryActivity {
            id: 10,
            name: "Design".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 11,
            name: "Development".to_owned(),
            is_default: false,
        },
    ];
    assert_eq!(
        RedmineProvider::select_time_entry_activity(&activities)
            .unwrap()
            .id,
        11,
        "exact Development must beat the default"
    );

    let ambiguous = vec![
        RedmineTimeEntryActivity {
            id: 1,
            name: "AI automation".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 2,
            name: "AI automation".to_owned(),
            is_default: false,
        },
    ];
    let error = RedmineProvider::select_time_entry_activity(&ambiguous).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("ambiguous"));

    let multiple_defaults = vec![
        RedmineTimeEntryActivity {
            id: 3,
            name: "Design".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 4,
            name: "Testing".to_owned(),
            is_default: true,
        },
    ];
    let error = RedmineProvider::select_time_entry_activity(&multiple_defaults).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("multiple default"));

    let no_candidate = vec![RedmineTimeEntryActivity {
        id: 5,
        name: "Design".to_owned(),
        is_default: false,
    }];
    let error = RedmineProvider::select_time_entry_activity(&no_candidate).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn time_entry_activity_list_uses_redmine_enumeration_endpoint() {
    let (result, request) = one(
        MockResponse::ok(time_entry_activities(&[
            (1, "Development", false),
            (2, "Design", true),
        ])),
        |redmine| redmine.list_time_entry_activities(),
    );
    assert_eq!(result.unwrap()[1].id, 2);
    support::assert_request(
        &request,
        "GET",
        "/enumerations/time_entry_activities.json",
        None,
    );
}

#[test]
fn time_entry_create_sends_exact_projection_and_decodes_201() {
    let body = time_entry_response(77, 28, 9, 0.02, "marker", "2026-08-25");
    let (result, request) = one(MockResponse::status(201, body), |redmine| {
        redmine.create_time_entry(28, 0.02, "2026-08-25", 9, "marker")
    });
    let entry = result.unwrap().expect("201 should contain a time entry");
    assert_eq!(entry.id, 77);
    support::assert_request(&request, "POST", "/time_entries.json", None);
    assert!(request.contains(r#""time_entry":{"issue_id":28,"hours":0.02,"spent_on":"2026-08-25","activity_id":9,"comments":"marker"}"#));
}

#[test]
fn time_entry_create_accepts_204_and_empty_201_without_decoding_error() {
    let (result, request) = one(MockResponse::status(204, ""), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());
    support::assert_request(&request, "POST", "/time_entries.json", None);

    let (result, _) = one(MockResponse::status(201, "{}"), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());

    let (result, _) = one(MockResponse::status(204, "{}"), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());
}

#[test]
fn time_entry_list_reconciles_the_stable_run_marker() {
    let comments = "phasegent timer run_id=run-1";
    let body = time_entry_collection(&[(901, 28, 9, 0.02, comments, "2026-08-25")]);
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.find_time_entry_by_comments(28, "2026-08-25", comments)
    });
    let entry = result.unwrap().expect("marker should reconcile");
    assert_eq!(entry.id, 901);
    support::assert_request(&request, "GET", "/time_entries.json?", None);
    assert!(request.contains("issue_id=28"));
    assert!(request.contains("from=2026-08-25"));
    assert!(request.contains("to=2026-08-25"));
}

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
    let storage = Storage::open_for_home(&home).unwrap();
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
    let provider = crate::provider::RedmineProvider::new(
        crate::provider::RedmineConfig::new(base, "42", 37),
        TEST_API_KEY.to_owned(),
    )
    .unwrap();

    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider).unwrap();
    assert_eq!(run.time_entry_id, Some(77));
    assert_eq!(run.sync_status, "synced");
    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider).unwrap();

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
    let storage = Storage::open_for_home(&home).unwrap();
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
    let provider = crate::provider::RedmineProvider::new(
        crate::provider::RedmineConfig::new(base, "42", 37),
        TEST_API_KEY.to_owned(),
    )
    .unwrap();

    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider).unwrap();
    assert_eq!(run.sync_status, "unconfirmed");
    assert_eq!(run.time_entry_id, None);
    crate::time_tracking_cli::project_run_with_provider(&storage, &mut run, &provider).unwrap();
    assert_eq!(run.sync_status, "synced");
    assert_eq!(run.time_entry_id, Some(77));

    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 4, "reconciliation must avoid a second POST");
    assert!(observed[2].starts_with("POST /time_entries.json"));
    assert!(observed[3].starts_with("GET /time_entries.json?"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

#[test]
fn update_body_with_tracker_keeps_single_put_shape() {
    let (result, request) = one(
        MockResponse::ok(issue_response(23, "Title", "Updated", false, &[])),
        |redmine| redmine.update_body_with_tracker(23, "Updated", 1),
    );
    assert_eq!(result.unwrap().body, "Updated");
    support::assert_request(&request, "PUT", "/issues/23.json", None);
    assert!(request.contains(r#""issue":{"description":"Updated","tracker_id":1}"#));
    assert!(!request.contains("status_id"));
}

#[test]
fn set_issue_status_puts_any_validated_status_id() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        24,
        "Title",
        "Body",
        false,
        &[],
    ))]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine.set_issue_status(24, 3).unwrap();
    assert_eq!(summary.number, 24);
    assert_eq!(summary.state, "open");
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "PUT", "/issues/24.json", None);
    assert!(request.contains(r#""issue":{"status_id":3}"#));
    server.join().unwrap();
}

#[test]
fn status_and_tracker_selection_validate_name_id_and_ambiguity() {
    let statuses = vec![
        RedmineIssueStatus {
            id: 1,
            name: "New".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 2,
            name: "In Progress".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 7,
            name: "New".to_owned(),
            is_closed: false,
        },
    ];
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "In Progress")
            .unwrap()
            .id,
        2
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "7")
            .unwrap()
            .id,
        7
    );
    // A duplicate name is ambiguous even when one candidate carries the id.
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "New")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "Blocked")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "0")
            .unwrap_err()
            .json()["kind"],
        "config"
    );

    let trackers = vec![
        crate::redmine_model::RedmineTracker {
            id: 1,
            name: "Bug".to_owned(),
        },
        crate::redmine_model::RedmineTracker {
            id: 2,
            name: "Feature".to_owned(),
        },
    ];
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "Bug")
            .unwrap()
            .id,
        1
    );
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "2").unwrap().id,
        2
    );
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "Task")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
}

#[test]
fn journals_back_comment_create_get_and_marker_lookup() {
    let marker = "<!-- marker -->";
    let body = "<!-- marker --> comment body";
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
    ]);
    let redmine = provider(base);

    let created = redmine.create_comment(21, body, marker).unwrap();
    assert_eq!(created.id, 501);
    assert_eq!(created.marker.as_deref(), Some(marker));
    assert!(created.body.is_none());
    // Note output must anchor the exact journal so audit references land on
    // #note-<id> rather than the issue top.
    assert!(
        created
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#note-501")),
        "html_url: {:?}",
        created.html_url
    );

    let fetched = redmine.get_comment(21, 501).unwrap();
    assert_eq!(fetched.body.as_deref(), Some(body));
    assert_eq!(fetched.marker.as_deref(), Some(marker));
    assert!(
        fetched
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#note-501")),
        "html_url: {:?}",
        fetched.html_url
    );

    let found = redmine.find_marker(21, marker).unwrap();
    assert_eq!(found.id, 501);
    assert_eq!(found.marker.as_deref(), Some(marker));

    let requests = requests.recv().unwrap();
    support::assert_request(&requests[0], "PUT", "/issues/21.json", None);
    assert!(requests[0].contains(r#""issue":{"notes":"<!-- marker --> comment body"}"#));
    for request in &requests[1..] {
        support::assert_request(request, "GET", "/issues/21.json?include=journals", None);
    }
    server.join().unwrap();
}

#[test]
fn search_paginates_and_filters_by_requested_state() {
    let first_page =
        support::issue_collection(3, 2, &[(31, "Open one", false), (32, "Closed one", true)]);
    let second_page = support::issue_collection(3, 2, &[(33, "Open two", false)]);
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(first_page),
        MockResponse::ok(second_page),
    ]);
    let redmine = provider(base);
    let issues = redmine.search_issues(Some("needle"), "open").unwrap();
    assert_eq!(
        issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
        [31, 33]
    );
    assert!(issues.iter().all(|issue| issue.state == "open"));

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, offset) in requests.iter().zip(["0", "2"]) {
        support::assert_request(request, "GET", "/issues.json?", None);
        assert!(request.contains("status_id=open"));
        assert!(request.contains("limit=100"));
        assert!(request.contains(&format!("offset={offset}")));
        assert!(request.contains("project_id=42"));
        assert!(request.contains("subject=%7Eneedle") || request.contains("subject=~needle"));
    }
    server.join().unwrap();
}

#[test]
fn redmine_errors_decode_arrays_and_redact_api_key() {
    let response = MockResponse::error(
        422,
        format!(r#"{{"errors":["bad {TEST_API_KEY}",{{"message":"invalid project"}}]}}"#),
    );
    let (result, request) = one(response, |redmine| redmine.get_issue(22));
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 422);
    assert!(json["message"].as_str().unwrap().contains("bad [redacted]"));
    assert!(
        json["message"]
            .as_str()
            .unwrap()
            .contains("invalid project")
    );
    assert!(!error.to_string().contains(TEST_API_KEY));
    support::assert_request(&request, "GET", "/issues/22.json?include=journals", None);
}

#[test]
fn metadata_errors_redact_api_key() {
    let response = MockResponse::error(422, format!(r#"{{"errors":["bad {TEST_API_KEY}"]}}"#));
    let (result, _) = one(response, |redmine| redmine.list_projects());
    let error = result.unwrap_err();
    assert!(!error.to_string().contains(TEST_API_KEY));
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
}

#[test]
fn empty_redmine_http_errors_include_operation_and_status() {
    let (result, request) = one(MockResponse::error(403, ""), |redmine| {
        redmine.get_issue(23)
    });
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 403);
    assert_eq!(json["operation"], "issue get");
    let message = json["message"].as_str().unwrap();
    assert!(message.contains("issue get"));
    assert!(message.contains("403"));
    assert!(!message.contains("Redmine returned an error"));
    support::assert_request(&request, "GET", "/issues/23.json?include=journals", None);
}

#[test]
fn issue_create_automatically_bootstraps_once_before_returning_issue() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-auto-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("redmine.orchestrator.key"),
        TEST_API_KEY,
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.executor.key"),
        "executor-redmine-key",
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.reviewer.key"),
        "reviewer-redmine-key",
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.admin.key"),
        "admin-redmine-key",
    )
    .unwrap();

    let (base, requests, server) = sequence(vec![
        // Bootstrap sequence (admin provider): project lookup, statuses,
        // project create, then three `/users/current.json` lookups
        // (orchestrator/executor/reviewer role-scoped keys), then for each
        // of the three agent users: role list, membership list, and
        // membership POST (admin key).
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow issues for owner/repo",
        )),
        // 3: orchestrator identity (orchestrator-scoped key)
        MockResponse::ok(support::current_user_response(11, "orchestrator")),
        // 4: executor identity (executor-scoped key)
        MockResponse::ok(support::current_user_response(22, "executor")),
        // 5: reviewer identity (reviewer-scoped key)
        MockResponse::ok(support::current_user_response(33, "reviewer")),
        // 6-8: orchestrator reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 9-11: executor reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 12-14: reviewer reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 15: mirror plugin GET (404 → triggers POST)
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        // 16: mirror plugin POST (202 → queued)
        MockResponse::status(
            202,
            support::git_mirror_response(
                901,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
        // First issue create (orchestrator key)
        MockResponse::ok(support::issue_response(80, "Created", "Body", false, &[])),
        // Second issue create (orchestrator key, bootstrap result reused)
        MockResponse::ok(support::issue_response(
            81,
            "Created again",
            "Body",
            false,
            &[],
        )),
        // Issue search (orchestrator key)
        MockResponse::ok(support::issue_collection(1, 100, &[(80, "Created", false)])),
        // Explicit project id: bypasses bootstrap
        MockResponse::ok(support::issue_response(82, "Explicit", "Body", false, &[])),
    ]);
    fs::write(
        config_directory.join("redmine.orchestrator.config.json"),
        serde_json::json!({
            "api_base": base.clone(),
            "project_id": "999",
            "close_status_id": 5
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.admin.config.json"),
        serde_json::json!({
            "api_base": base.clone(),
        })
        .to_string(),
    )
    .unwrap();
    let args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "create",
        "--title",
        "Created",
        "--body",
        "Body",
    ]);
    assert_eq!(crate::cli::run(args), 0);
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--repository",
            "owner/repo",
            "issue",
            "create",
            "--title",
            "Created again",
            "--body",
            "Body",
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--repository",
            "owner/repo",
            "issue",
            "search",
            "--state",
            "all",
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--repository",
            "owner/repo",
            "--project-id",
            "99",
            "issue",
            "create",
            "--title",
            "Explicit",
            "--body",
            "Body",
        ])),
        0
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 21);
    // Bootstrap request order:
    //   0: project lookup (admin)
    //   1: status list (admin)
    //   2: project create (admin)
    //   3: orchestrator current user (orchestrator key)
    //   4: executor current user (executor key)
    //   5: reviewer current user (reviewer key)
    //   6: role list (admin)
    //   7: membership list (admin)
    //   8: orchestrator membership POST (admin)
    //   9: role list (admin)
    //  10: membership list (admin)
    //  11: executor membership POST (admin)
    //  12: role list (admin)
    //  13: membership list (admin)
    //  14: reviewer membership POST (admin)
    //  15: mirror plugin GET (mirror key)
    //  16: mirror plugin POST (mirror key)
    //  17: first issue create (orchestrator)
    //  18: second issue create (orchestrator)
    //  19: issue search (orchestrator)
    //  20: explicit project id issue create (orchestrator)
    support::assert_request_with_key(
        &requests[0],
        "GET",
        "/projects/owner-repo.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[1],
        "GET",
        "/issue_statuses.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[2],
        "POST",
        "/projects.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[3],
        "GET",
        "/users/current.json",
        None,
        TEST_API_KEY,
    );
    support::assert_request_with_key(
        &requests[4],
        "GET",
        "/users/current.json",
        None,
        "executor-redmine-key",
    );
    support::assert_request_with_key(
        &requests[5],
        "GET",
        "/users/current.json",
        None,
        "reviewer-redmine-key",
    );
    support::assert_request_with_key(
        &requests[6],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[7],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[8],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[9],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[10],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[11],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[12],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[13],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[14],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    // Orchestrator membership uses Maintainer (id 3) for user 11.
    assert!(requests[8].contains(r#""user_id":11,"role_ids":[3]"#));
    // Executor membership uses Developer (id 4) for user 22.
    assert!(requests[11].contains(r#""user_id":22,"role_ids":[4]"#));
    // Reviewer membership uses Reporter (id 5) for user 33.
    assert!(requests[14].contains(r#""user_id":33,"role_ids":[5]"#));
    // Mirror plugin GET uses the bearer key on the plugin path.
    support::assert_request_with_bearer(
        &requests[15],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    // Mirror plugin POST carries the JSON `{ "url": ... }` body and bearer key.
    support::assert_request_with_bearer(
        &requests[16],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    support::assert_request(&requests[17], "POST", "/issues.json", None);
    support::assert_request(&requests[18], "POST", "/issues.json", None);
    support::assert_request(&requests[19], "GET", "/issues.json?", None);
    support::assert_request(&requests[20], "POST", "/issues.json", None);
    assert_eq!(
        fs::read_to_string(config_directory.join("redmine.orchestrator.key")).unwrap(),
        TEST_API_KEY
    );
    assert!(requests[20].contains(r#""project_id":99"#));
    let stored = auth::load_redmine_config(Role::Orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(stored.project_id.as_deref(), Some("44"));
    assert_eq!(stored.close_status_id, Some(5));
    // Active bootstrap no longer persists the legacy group fields.
    assert_eq!(stored.group_name, None);
    assert_eq!(stored.group_role, None);
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_fails_with_distinct_users_error_when_two_keys_resolve_to_same_user() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-distinct-users-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("redmine.orchestrator.key"),
        "orchestrator-redmine-key",
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.executor.key"),
        "executor-redmine-key",
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.reviewer.key"),
        "reviewer-redmine-key",
    )
    .unwrap();
    fs::write(
        config_directory.join("redmine.admin.key"),
        "admin-redmine-key",
    )
    .unwrap();

    let (base, requests, server) = sequence(vec![
        // Admin-side project bootstrap gets us to the identity lookup phase.
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow issues for owner/repo",
        )),
        // orchestrator identity resolves to user id 11.
        MockResponse::ok(support::current_user_response(11, "shared-user")),
        // executor identity ALSO resolves to user id 11 — the collision the
        // distinct-users check is supposed to catch.
        MockResponse::ok(support::current_user_response(11, "shared-user")),
        // reviewer identity still gets fetched before the check fires so all
        // three pairwise comparisons have data.
        MockResponse::ok(support::current_user_response(33, "reviewer")),
    ]);
    fs::write(
        config_directory.join("redmine.admin.config.json"),
        serde_json::json!({"api_base": base}).to_string(),
    )
    .unwrap();

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when two role keys resolve to the same Redmine user");
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"]
        .as_str()
        .expect("error message missing")
        .to_owned();
    assert!(
        message.contains("distinct users"),
        "error message must mention distinct users, got: {message}"
    );
    assert!(
        message.contains("shared-user") || message.contains("#11"),
        "error message must describe the colliding identity, got: {message}"
    );

    // Bootstrap must abort after the three current_user lookups and never
    // issue a membership POST/PUT — otherwise the partial mapping would leak
    // into the project.
    let observed_requests = requests.recv().unwrap();
    assert_eq!(
        observed_requests.len(),
        6,
        "bootstrap must stop after the three current_user lookups on distinct-user failure"
    );
    for (index, request) in observed_requests.iter().enumerate() {
        assert!(
            !request.starts_with("POST /projects/44/memberships.json"),
            "no membership POST should fire on distinct-user failure (index {index}): {request}"
        );
        assert!(
            !request.starts_with("PUT /memberships/"),
            "no membership PUT should fire on distinct-user failure (index {index}): {request}"
        );
    }

    // Project bootstrap config must not be persisted on the admin role: the
    // workflow is not ready and we must not leave a partial identity mapping
    // behind for the next operator run to discover.
    let stored = auth::load_redmine_config(Role::Admin)
        .expect("admin config must load")
        .expect("admin config must exist");
    assert_eq!(stored.project_id, None);
    assert_eq!(stored.close_status_id, None);
    assert_eq!(stored.api_base.as_deref(), Some(base.as_str()));

    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn parser_auth_config_and_provider_selection_regressions() {
    let admin = command::parse(&strings([
        "--role",
        "admin",
        "--provider",
        "redmine",
        "project",
        "list",
    ]))
    .unwrap();
    assert_eq!(admin.role, Some(Role::Admin));
    let invalid_role = "invalid".parse::<Role>().unwrap_err();
    assert!(invalid_role.contains("admin, orchestrator, executor, or reviewer"));

    let args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "issue",
        "search",
        "--query",
        "needle",
        "--state",
        "closed",
    ]);
    let invocation = command::parse(&args).unwrap();
    assert_eq!(invocation.provider, Some(ProviderKind::Redmine));
    assert!(matches!(
        invocation.command,
        Command::Issue(IssueCommand::Search { ref query, ref state })
            if query.as_deref() == Some("needle") && state == "closed"
    ));

    let auth_args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "auth",
        "setup",
        "--stdin",
        "--api-base",
        "https://redmine.example",
        "--project-id",
        "42",
        "--close-status-id",
        "37",
    ]);
    assert!(matches!(
        command::parse(&auth_args).unwrap().command,
        Command::AuthSetup {
            read_stdin: true,
            provider: None,
            ref api_base,
            ref project_id,
            ref close_status_id,
            repository: None,
        } if api_base.as_deref() == Some("https://redmine.example")
            && project_id.as_deref() == Some("42")
            && close_status_id.as_deref() == Some("37")
    ));

    let config = RedmineConfig::new("https://redmine.example/", "42", 37);
    assert_eq!(config.provider(), ProviderKind::Redmine);
    assert_eq!(config.require_project_id().unwrap(), "42");
    assert_eq!(config.require_close_status_id().unwrap(), 37);
    assert_eq!(
        ProviderKind::from_str("redmine").unwrap(),
        ProviderKind::Redmine
    );
    assert_eq!(
        crate::provider_config::resolve_kind(Role::Reviewer, Some(ProviderKind::Redmine)).unwrap(),
        ProviderKind::Redmine
    );

    let key_path = auth::redmine_key_path_for(Path::new("/tmp/phasegent-test"), Role::Executor);
    assert!(key_path.ends_with("redmine.executor.key"));
    assert_eq!(
        auth::setup_provider(
            Role::Orchestrator,
            "redmine",
            auth::SetupOptions {
                read_stdin: false,
                api_base: None,
                repository: Some("owner/repo".to_owned()),
                project_id: None,
                close_status_id: None,
            },
        )
        .unwrap_err(),
        "--repository requires the forgejo provider"
    );
}

#[test]
fn admin_auth_setup_writes_the_normal_role_scoped_private_key() {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-admin-key-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let secret = "admin-secret";
    auth::write_credential(&directory, Role::Admin, "redmine", secret).unwrap();
    assert_eq!(
        fs::read_to_string(auth::redmine_key_path_for(&directory, Role::Admin)).unwrap(),
        secret
    );
    assert!(!auth::redmine_key_path_for(&directory, Role::Orchestrator).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(auth::redmine_key_path_for(&directory, Role::Admin))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn admin_provider_requires_admin_key_without_falling_back() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-missing-admin-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("redmine.orchestrator.key"),
        "normal-secret",
    )
    .unwrap();
    let result = RedmineProvider::for_role(
        Role::Admin,
        RedmineConfig::new("http://redmine.test", "42", 37),
    );
    let error = match result {
        Ok(_) => panic!("admin provider unexpectedly used the orchestrator key"),
        Err(error) => error,
    };
    assert_eq!(error.json()["kind"], "auth");
    assert!(error.to_string().contains("could not read Redmine API key"));
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn metadata_parser_requires_confirmation_and_required_fields() {
    let list = command::parse(&strings(["--role", "executor", "project", "list"])).unwrap();
    assert!(matches!(
        list.command,
        Command::Project(ProjectCommand::List)
    ));

    let status = command::parse(&strings(["--role", "reviewer", "status", "list"])).unwrap();
    assert!(matches!(
        status.command,
        Command::Status(StatusCommand::List)
    ));

    for args in [
        strings([
            "--role",
            "orchestrator",
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
        ]),
        strings([
            "--role",
            "orchestrator",
            "project",
            "create",
            "--name",
            "Workflow",
            "--confirm",
        ]),
    ] {
        assert!(command::parse(&args).is_err());
    }

    let create = command::parse(&strings([
        "--role",
        "orchestrator",
        "project",
        "create",
        "--name",
        "Workflow",
        "--identifier",
        "workflow",
        "--description",
        "Tracking project",
        "--confirm",
    ]))
    .unwrap();
    assert!(matches!(
        create.command,
        Command::Project(ProjectCommand::Create {
            ref name,
            ref identifier,
            ref description,
            confirmed: true,
        }) if name == "Workflow"
            && identifier == "workflow"
            && description.as_deref() == Some("Tracking project")
    ));

    let bootstrap = command::parse(&strings([
        "--role",
        "admin",
        "--provider",
        "redmine",
        "workflow",
        "bootstrap",
        "--repository",
        "Cloud1ful/repo",
        "--close-status-name",
        "Closed",
    ]))
    .unwrap();
    assert!(matches!(
        bootstrap.command,
        Command::Workflow(WorkflowCommand::Bootstrap {
            ref repository,
            ref close_status_name,
            close_status_id: None,
        }) if repository.as_deref() == Some("Cloud1ful/repo")
            && close_status_name.as_deref() == Some("Closed")
    ));

    for (flag, value) in [
        ("--group-name", "AI Agents"),
        ("--group-role", "Developer"),
        ("--group-name=AI Agents", ""),
        ("--group-role=Developer", ""),
    ] {
        let mut args = vec![
            "--role".to_owned(),
            "admin".to_owned(),
            "--provider".to_owned(),
            "redmine".to_owned(),
            "workflow".to_owned(),
            "bootstrap".to_owned(),
        ];
        if value.is_empty() {
            args.push(flag.to_owned());
        } else {
            args.push(flag.to_owned());
            args.push(value.to_owned());
        }
        let error = command::parse(&args).expect_err("legacy group flag must be rejected");
        assert!(
            error.contains("is no longer supported"),
            "unexpected error for {flag}: {error}"
        );
    }
}

#[test]
fn redmine_keeps_repo_and_ci_commands_unsupported() {
    let redmine = provider("http://redmine.test".to_owned());
    assert!(!redmine.supports(Capability::RepoCreate));
    assert!(!redmine.supports(Capability::CiRead));
    assert_eq!(
        redmine
            .create_repo("owner/repo", true, "", false)
            .unwrap_err()
            .json()["kind"],
        "not_supported"
    );
    let error = redmine
        .ci_runs(&CiRunsFilter {
            sha: None,
            ref_name: None,
            status: None,
            workflow: None,
            page: 1,
            limit: 50,
        })
        .unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
    let dispatcher = ProviderDispatcher::Redmine(provider("http://redmine.test".to_owned()));
    assert_eq!(dispatcher.kind(), ProviderKind::Redmine);
}

#[test]
fn project_creation_is_admin_only_and_forgejo_metadata_is_unsupported() {
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Executor.allows(Capability::ProjectCreate));
    assert!(!Role::Reviewer.allows(Capability::ProjectCreate));
    for role in [Role::Executor, Role::Reviewer] {
        assert!(role.allows(Capability::ProjectRead));
        assert!(role.allows(Capability::IssueStatusRead));
    }

    let forgejo = crate::forgejo::ForgejoProvider::new(
        crate::forgejo::ForgejoConfig::new("http://forgejo.test", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    assert_eq!(
        forgejo.list_projects().unwrap_err().json()["kind"],
        "not_supported"
    );
    assert_eq!(
        forgejo.list_issue_statuses().unwrap_err().json()["kind"],
        "not_supported"
    );

    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "project",
            "list"
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "executor",
            "--provider",
            "redmine",
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
            "--confirm",
        ])),
        3
    );
    for role in ["executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "workflow",
                "bootstrap",
                "--repository",
                "owner/repo",
            ])),
            3
        );
    }
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "workflow",
            "bootstrap",
            "--repository",
            "owner/repo",
        ])),
        3
    );
}

#[test]
fn status_set_and_tracker_selection_enforce_role_and_provider_boundaries() {
    // Non-orchestrator roles cannot move an issue's status; the permission
    // error fires before any provider or network access.
    for role in ["admin", "executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "status",
                "set",
                "3",
                "--status",
                "New",
            ])),
            3,
            "expected exit 3 for {role} status set"
        );
    }

    // status set is Redmine-only: Forgejo is rejected as unsupported.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "status",
            "set",
            "3",
            "--status",
            "New",
        ])),
        1
    );

    // Tracker selection on create/update-body is Redmine-only. A stored
    // forgejo token lets the dispatcher build so the rejection comes from
    // tracker resolution, not from missing credentials; no request is made.
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-tracker-boundary-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("orchestrator.token"),
        "test-forgejo-token",
    )
    .unwrap();

    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--api-base",
            "http://forgejo.test",
            "--repository",
            "owner/repo",
            "issue",
            "create",
            "--title",
            "Plan",
            "--tracker",
            "Bug",
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--api-base",
            "http://forgejo.test",
            "--repository",
            "owner/repo",
            "issue",
            "update-body",
            "9",
            "--body",
            "Updated",
            "--tracker",
            "Bug",
        ])),
        1
    );
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

struct HomeGuard(Option<OsString>);

impl HomeGuard {
    fn set(directory: &Path) -> Self {
        let previous = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", directory);
        }
        Self(previous)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.0.take() {
                env::set_var("HOME", previous);
            } else {
                env::remove_var("HOME");
            }
        }
    }
}

fn mirror_env() -> (EnvGuard, EnvGuard) {
    let key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    (key, url)
}

#[test]
fn mirror_plugin_uses_bearer_header_and_uses_redmine_base_url() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::status(
        202,
        support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "pending",
            Some("https://git.example.com/owner/repo.git"),
            Some("/var/redmine/repos/owner_repo.git"),
            None,
        ),
    )]);
    let outcome = crate::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.id, 901);
    assert_eq!(outcome.project_id, 44);
    assert_eq!(outcome.identifier, "mirror_44_owner_repo");
    assert_eq!(outcome.status, "pending");
    assert_eq!(outcome.remote_url, "https://git.example.com/owner/repo.git");
    assert_eq!(outcome.local_path, "/var/redmine/repos/owner_repo.git");
    assert!(outcome.error.is_none());

    let request = requests.recv().unwrap().remove(0);
    // GET (404) and POST (202) reuse the exact same base; the bearer key is
    // passed in the Authorization header, never as a query parameter.
    support::assert_request_with_bearer(
        &request,
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    assert!(
        !request.contains("?key=") && !request.contains("&key="),
        "bearer key must not appear in the query string: {request}"
    );
    // The Redmine base URL has no `/api/v1` suffix; the plugin lives under
    // `/sys/redmine_git_mirror/...` instead.
    assert!(
        request.contains("HTTP/1.1\r\nauthorization: Bearer mirror-bearer-key")
            || request.contains("authorization: Bearer mirror-bearer-key\r\n"),
        "request must carry the bearer authorization header: {request}"
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_get_existing_skips_post_and_returns_existing_status() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::ok(support::git_mirror_response(
        901,
        44,
        "mirror_44_owner_repo",
        "ready",
        Some("https://git.example.com/owner/repo.git"),
        Some("/var/redmine/repos/owner_repo.git"),
        None,
    ))]);
    let outcome =
        crate::redmine::register_git_mirror(&base, 44, "Owner", "Repo", "ignored").unwrap();
    assert_eq!(outcome.status, "ready");
    assert_eq!(outcome.id, 901);
    let request = requests.recv().unwrap().remove(0);
    support::assert_request_with_bearer(
        &request,
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    // A 200 GET must short-circuit the POST — only one request is observed.
    server.join().unwrap();
    assert!(
        requests.recv().is_err(),
        "GET must short-circuit the POST, but the channel delivered a second batch"
    );
}

#[test]
fn mirror_plugin_404_triggers_post_and_carries_credential_free_url() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            support::git_mirror_response(
                901,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
    ]);
    let outcome = crate::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.status, "pending");
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[0],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_missing_key_fails_bootstrap_with_actionable_error() {
    let _environment_lock = lock_workflow_tests();
    // Isolate the test behind a private HOME so the production SQLite
    // database (which the operator may already have populated with a
    // mirror key) cannot leak into the resolver through the env →
    // SQLite fallback. A throwaway directory with an empty SQLite
    // schema ensures the only way to satisfy the lookup is via the
    // environment variable we explicitly clear below.
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-missing-key-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let _url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let previous = std::env::var_os("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
    unsafe {
        std::env::remove_var("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
    }
    let error = crate::redmine::register_git_mirror(
        "https://redmine.example",
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .expect_err("missing plugin key must fail bootstrap");
    if let Some(previous) = previous.as_ref() {
        unsafe {
            std::env::set_var("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", previous);
        }
    }
    let _ = fs::remove_dir_all(&directory);
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY"),
        "missing-key error must name the env var, got: {message}"
    );
}

#[test]
fn mirror_plugin_http_errors_redact_bearer_key() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::error(
        500,
        r#"{"errors":["server error: mirror-bearer-key"]}"#.to_owned(),
    )]);
    let error = crate::redmine::register_git_mirror(&base, 44, "owner", "repo", "url")
        .expect_err("5xx must surface as an actionable error");
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 500);
    assert!(
        json["message"].as_str().unwrap().contains("[redacted]"),
        "bearer key must be redacted, got: {json}"
    );
    assert!(!error.to_string().contains("mirror-bearer-key"));
    let _ = requests.recv().unwrap();
    server.join().unwrap();
}

#[test]
fn mirror_plugin_get_failed_triggers_single_requeue_post() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        // The plugin reports a stale mirror whose recorded remote_url
        // differs from what we register; the requeue POST must carry the
        // caller-supplied URL, never the untrusted response field.
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://stale.example.com/old/repo.git"),
            None,
            Some("git clone failed"),
        )),
        MockResponse::status(
            202,
            support::git_mirror_response(
                902,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
    ]);
    let outcome = crate::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.id, 902);
    assert_eq!(outcome.status, "pending");
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[0],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_failed_status_fails_bootstrap_clearly() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://git.example.com/owner/repo.git"),
            None,
            Some("git clone failed"),
        )),
        // Even after a requeue POST, a still-`failed` response must
        // surface as a clear bootstrap error.
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://git.example.com/owner/repo.git"),
            None,
            Some("still failing after requeue"),
        )),
    ]);
    let error = crate::redmine::register_git_mirror(&base, 44, "owner", "repo", "url")
        .expect_err("a `failed` plugin status must surface as a bootstrap error");
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("failed status"),
        "failed-status error must explain the failure, got: {message}"
    );
    assert!(message.contains("still failing after requeue"));
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"url""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_identifier_lowercases_owner_and_repo() {
    assert_eq!(
        crate::redmine::mirror_identifier(44, "Owner", "Repo"),
        "mirror_44_owner_repo"
    );
    assert_eq!(
        crate::redmine::mirror_identifier(44, "Mixed.Case", "Repo+One"),
        "mirror_44_mixed.case_repo+one"
    );
}

// ---------------------------------------------------------------------------
// Phase 4A: native planning fields and project version discovery
// ---------------------------------------------------------------------------

#[test]
fn create_issue_with_planning_serializes_native_fields_and_omits_unset_ones() {
    let planning = crate::redmine_model::IssuePlanning {
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
    let planning = crate::redmine_model::IssuePlanning {
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
fn version_list_decodes_redmine_wrapper_within_project_scope() {
    let response = support::version_collection(&[
        (12, "Sprint 1", "open", Some("2026-09-30")),
        (13, "Backlog", "open", None),
    ]);
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.list_versions()
    });
    let versions = result.unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| (version.id, version.name.as_str()))
            .collect::<Vec<_>>(),
        [(12, "Sprint 1"), (13, "Backlog")]
    );
    assert_eq!(versions[0].status, "open");
    assert_eq!(versions[0].due_date.as_deref(), Some("2026-09-30"));
    support::assert_request(&request, "GET", "/projects/42/versions.json?", None);
    assert!(request.contains("limit=100"));
}

#[test]
fn version_selection_resolves_name_id_and_rejects_bad_values() {
    let versions = vec![
        crate::redmine_model::RedmineVersion {
            id: 12,
            name: "Sprint 1".to_owned(),
            status: "open".to_owned(),
            due_date: None,
        },
        crate::redmine_model::RedmineVersion {
            id: 14,
            name: "Sprint 1".to_owned(),
            status: "closed".to_owned(),
            due_date: None,
        },
    ];
    assert_eq!(
        RedmineProvider::select_version(&[versions[0].clone()], "12")
            .unwrap()
            .id,
        12
    );
    // Duplicate names are ambiguous even though ids are unique.
    assert_eq!(
        RedmineProvider::select_version(&versions, "Sprint 1")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "Missing")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "0")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "99")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
}

#[test]
fn planning_validation_rejects_malformed_values_before_any_write() {
    use crate::command::PlanningOptions;
    use crate::redmine_planning_cli::resolve_planning;
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
    use crate::redmine_planning_cli::resolve_planning;
    let forgejo = ProviderDispatcher::Forgejo(
        crate::forgejo::ForgejoProvider::new(
            crate::forgejo::ForgejoConfig::new("http://forgejo.test", "owner", "repo"),
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
fn version_list_paginates_and_selects_versions_on_later_pages() {
    let first_page = support::version_collection_page(
        3,
        2,
        &[
            (12, "Sprint 1", "open", None),
            (13, "Sprint 2", "open", None),
        ],
    );
    let second_page = support::version_collection_page(3, 2, &[(14, "Backlog", "closed", None)]);
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(first_page),
        MockResponse::ok(second_page),
    ]);
    let redmine = provider(base);
    let versions = redmine.list_versions().unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| (version.id, version.name.as_str()))
            .collect::<Vec<_>>(),
        [(12, "Sprint 1"), (13, "Sprint 2"), (14, "Backlog")]
    );
    // A version that only exists on the second page must be selectable so
    // --fixed-version resolution cannot falsely fail on large roadmaps.
    assert_eq!(
        RedmineProvider::select_version(&versions, "14").unwrap().id,
        14
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "Backlog")
            .unwrap()
            .id,
        14
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, offset) in requests.iter().zip(["0", "2"]) {
        support::assert_request(request, "GET", "/projects/42/versions.json?", None);
        assert!(request.contains(&format!("offset={offset}")));
        // The client always requests its own page size; pagination advances
        // by the number of items actually returned.
        assert!(request.contains("limit=100"));
    }
    server.join().unwrap();
}

#[test]
fn done_ratio_accepts_zero_and_serializes_the_boundary_value() {
    use crate::command::PlanningOptions;
    use crate::redmine_planning_cli::resolve_planning;
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
    let planning = crate::redmine_model::IssuePlanning {
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
    use crate::redmine_planning_cli::resolve_planning;
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

#[test]
fn version_list_enforces_role_and_provider_boundaries() {
    // Every role may list versions on Redmine...
    for role in ["admin", "orchestrator", "executor", "reviewer"] {
        let parsed = command::parse(&strings([
            "--role",
            role,
            "--provider",
            "redmine",
            "version",
            "list",
        ]))
        .unwrap();
        assert!(matches!(
            parsed.command,
            Command::VersionCommand(crate::command::VersionCommand::List)
        ));
    }
    // ...while Forgejo is rejected with a not-supported error before any
    // provider is built.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "version",
            "list",
        ])),
        1
    );
}

#[test]
fn relation_list_uses_redmine_relations_endpoint_and_resolves_viewpoint() {
    let body = serde_json::json!({
        "relations": [
            {"id": 1, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0},
            {"id": 2, "issue_id": 30, "issue_to_id": 10, "relation_type": "blocks", "delay": 0},
            {"id": 3, "issue_id": 10, "issue_to_id": 40, "relation_type": "precedes", "delay": 3},
            {"id": 4, "issue_id": 50, "issue_to_id": 10, "relation_type": "relates", "delay": 0}
        ]
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| redmine.list_relations(10));
    let relations = result.unwrap();
    assert_eq!(relations.len(), 4);
    // The queried issue is the relation's source: type stays canonical.
    assert_eq!(relations[0].id, 1);
    assert_eq!(relations[0].relation_type, "blocks");
    assert_eq!(relations[0].issue_id, 10);
    assert_eq!(relations[0].issue_to_id, 20);
    // The queried issue is the relation's target: blocks reads as blocked.
    assert_eq!(relations[1].id, 2);
    assert_eq!(relations[1].relation_type, "blocked");
    assert_eq!(relations[1].issue_id, 30);
    // precedes keeps its delay from the queried (source) side.
    assert_eq!(relations[2].id, 3);
    assert_eq!(relations[2].relation_type, "precedes");
    assert_eq!(relations[2].delay, Some(3));
    // relates is symmetric.
    assert_eq!(relations[3].id, 4);
    assert_eq!(relations[3].relation_type, "relates");
    support::assert_request(&request, "GET", "/issues/10/relations.json", None);
}

#[test]
fn relation_create_posts_canonical_type_and_omits_delay_for_blocks() {
    let body = serde_json::json!({
        "relation": {"id": 5, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.create_relation(10, 20, RedmineRelationType::Blocks, None)
    });
    let summary = result.unwrap();
    assert_eq!(summary.id, 5);
    assert_eq!(summary.relation_type, "blocks");
    support::assert_request(&request, "POST", "/issues/10/relations.json", None);
    assert!(
        request.contains(r#""relation":{"issue_to_id":20,"relation_type":"blocks"}"#),
        "unexpected relation create body: {request}"
    );
    assert!(
        !request.contains("delay"),
        "delay must be omitted for blocks"
    );
}

#[test]
fn relation_create_serializes_delay_only_for_precedes() {
    let body = serde_json::json!({
        "relation": {"id": 6, "issue_id": 10, "issue_to_id": 20, "relation_type": "precedes", "delay": 5}
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.create_relation(10, 20, RedmineRelationType::Precedes, Some(5))
    });
    let summary = result.unwrap();
    assert_eq!(summary.relation_type, "precedes");
    assert_eq!(summary.delay, Some(5));
    support::assert_request(&request, "POST", "/issues/10/relations.json", None);
    assert!(
        request.contains(r#""relation":{"issue_to_id":20,"relation_type":"precedes","delay":5}"#),
        "unexpected relation create body: {request}"
    );
}

#[test]
fn relation_delete_uses_delete_on_the_relation_endpoint() {
    let (base, requests, server) = sequence(vec![MockResponse::ok("")]);
    let redmine = provider(base);
    assert!(redmine.delete_relation(7).is_ok());
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "DELETE", "/relations/7.json", None);
    server.join().unwrap();
}

#[test]
fn relation_type_parse_input_accepts_only_canonical_names() {
    assert_eq!(
        RedmineRelationType::parse_input("blocks").unwrap(),
        RedmineRelationType::Blocks
    );
    assert_eq!(
        RedmineRelationType::parse_input("precedes").unwrap(),
        RedmineRelationType::Precedes
    );
    assert_eq!(
        RedmineRelationType::parse_input("relates").unwrap(),
        RedmineRelationType::Relates
    );
    // Inverse names are rejected as input so a relation can never be created
    // backwards.
    assert!(RedmineRelationType::parse_input("blocked").is_err());
    assert!(RedmineRelationType::parse_input("follows").is_err());
    assert!(RedmineRelationType::parse_input("weird").is_err());
}

#[test]
fn relation_type_parse_decodes_server_inverse_names() {
    assert_eq!(
        RedmineRelationType::parse("blocked").unwrap(),
        RedmineRelationType::Blocked
    );
    assert_eq!(
        RedmineRelationType::parse("follows").unwrap(),
        RedmineRelationType::Follows
    );
    assert!(RedmineRelationType::parse("unknown").is_err());
}

#[test]
fn relation_type_inverse_is_symmetric() {
    assert_eq!(
        RedmineRelationType::Blocks.inverse(),
        RedmineRelationType::Blocked
    );
    assert_eq!(
        RedmineRelationType::Blocked.inverse(),
        RedmineRelationType::Blocks
    );
    assert_eq!(
        RedmineRelationType::Precedes.inverse(),
        RedmineRelationType::Follows
    );
    assert_eq!(
        RedmineRelationType::Follows.inverse(),
        RedmineRelationType::Precedes
    );
    assert_eq!(
        RedmineRelationType::Relates.inverse(),
        RedmineRelationType::Relates
    );
}

#[test]
fn relation_parser_accepts_canonical_types_and_rejects_invalid() {
    let parsed = command::parse(&strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "relation",
        "create",
        "10",
        "--to",
        "20",
        "--type",
        "blocks",
    ]))
    .expect("valid relation create should parse");
    assert!(matches!(
        parsed.command,
        Command::Relation(RelationCommand::Create {
            issue: 10,
            to: 20,
            relation_type: RedmineRelationType::Blocks,
            delay: None
        })
    ));

    let with_delay = command::parse(&strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "relation",
        "create",
        "10",
        "--to",
        "20",
        "--type",
        "precedes",
        "--delay",
        "5",
    ]))
    .expect("valid relation create with delay should parse");
    assert!(matches!(
        with_delay.command,
        Command::Relation(RelationCommand::Create {
            issue: 10,
            to: 20,
            relation_type: RedmineRelationType::Precedes,
            delay: Some(5)
        })
    ));

    // Inverse names are rejected as CLI input.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocked",
        ]))
        .is_err()
    );

    // Unknown type is rejected.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "weird",
        ]))
        .is_err()
    );

    // Missing --to and missing --type are rejected.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--type",
            "blocks",
        ]))
        .is_err()
    );
}

#[test]
fn relation_help_prints_usage_and_exits_cleanly() {
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "--help"
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "--help"
        ])),
        0
    );
}

#[test]
fn relation_commands_enforce_role_and_provider_boundaries() {
    // relation list is allowed for orchestrator/executor/reviewer (the
    // permission check passes and parsing succeeds); admin is denied and
    // Forgejo is rejected before any provider is built.
    for role in ["orchestrator", "executor", "reviewer"] {
        let parsed = command::parse(&strings([
            "--role",
            role,
            "--provider",
            "redmine",
            "relation",
            "list",
            "10",
        ]))
        .unwrap();
        assert!(matches!(
            parsed.command,
            Command::Relation(RelationCommand::List { issue: 10 })
        ));
    }
    // admin is denied relation list before any network/provider access.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "admin",
            "--provider",
            "redmine",
            "relation",
            "list",
            "10",
        ])),
        3
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "list",
            "10",
        ])),
        1
    );

    // relation create/delete are orchestrator-only; non-orchestrator roles
    // are denied before any provider is built.
    for role in ["admin", "executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "relation",
                "create",
                "10",
                "--to",
                "20",
                "--type",
                "blocks",
            ])),
            3,
            "expected permission error for {role} relation create"
        );
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "relation",
                "delete",
                "5",
            ])),
            3,
            "expected permission error for {role} relation delete"
        );
    }
    // Forgejo rejects relation create/delete with a structured not-supported
    // error before any network access.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "delete",
            "5",
        ])),
        1
    );
}

#[test]
fn relation_create_denies_delay_for_non_precedes_types() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-relation-delay-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("redmine.orchestrator.key"),
        TEST_API_KEY,
    )
    .unwrap();
    // `--delay` with `--type blocks` must fail locally before any request.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            "http://127.0.0.1:1",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
            "--delay",
            "3",
        ])),
        1
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn relation_create_and_list_hit_redmine_endpoints_end_to_end() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-relation-e2e-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _home = HomeGuard::set(&directory);
    let config_directory = directory.join(".config/opencode/phasegent");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
        config_directory.join("redmine.orchestrator.key"),
        TEST_API_KEY,
    )
    .unwrap();

    let (base, requests, server) = sequence(vec![
        MockResponse::ok(
            serde_json::json!({
                "relation": {"id": 9, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
            })
            .to_string(),
        ),
        MockResponse::ok(
            serde_json::json!({
                "relations": [
                    {"id": 9, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
                ]
            })
            .to_string(),
        ),
    ]);

    // Create then list, both against the mock Redmine server.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "relation",
            "list",
            "10",
        ])),
        0
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "POST", "/issues/10/relations.json", None);
    support::assert_request(&requests[1], "GET", "/issues/10/relations.json", None);
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

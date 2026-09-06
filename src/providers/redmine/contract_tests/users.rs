#![allow(unused_imports)]
use super::support;
use super::support::{MockResponse, TEST_API_KEY, one, provider, sequence};
use crate::providers::redmine::model::{RedmineNewUser, RedmineUserResponse};

const USER_API_SECRET: &str = "user-secret-key-9f8e7d6c5b4a";
const DECODE_SECRET: &str = "user-secret-decode-abcdef123456";

fn user_response(id: u64, login: &str, api_key: Option<&str>) -> String {
    let mut user = serde_json::json!({
        "id": id,
        "login": login,
        "firstname": "Test",
        "lastname": "User",
        "mail": format!("{login}@example.test"),
        "created_on": "2026-01-01T00:00:00Z",
        "status": 1,
    });
    if let Some(key) = api_key {
        user["api_key"] = serde_json::Value::String(key.to_owned());
    }
    serde_json::json!({ "user": user }).to_string()
}

#[test]
fn user_create_posts_wrapped_payload_and_decodes_user() {
    let (result, request) = one(
        MockResponse::status(201, user_response(7, "executor", None)),
        |redmine| {
            redmine.create_user(
                "executor",
                "Test",
                "User",
                "executor@example.test",
                "s3cr3t-password",
            )
        },
    );
    let user = result.unwrap();
    assert_eq!(user.id, 7);
    assert_eq!(user.login, "executor");
    assert_eq!(user.firstname, "Test");
    assert_eq!(user.lastname, "User");
    assert_eq!(user.mail, "executor@example.test");
    // Creation responses rarely carry the key; the admin read fills it in.
    assert!(user.api_key_value().is_none());
    support::assert_request(&request, "POST", "/users.json", None);
    assert!(request.contains(r#""login":"executor""#), "{request}");
    assert!(request.contains(r#""firstname":"Test""#), "{request}");
    assert!(request.contains(r#""lastname":"User""#), "{request}");
    assert!(
        request.contains(r#""mail":"executor@example.test""#),
        "{request}"
    );
    assert!(
        request.contains(r#""password":"s3cr3t-password""#),
        "{request}"
    );
    assert!(
        !request.contains("api_key"),
        "create request must not carry an api_key field: {request}"
    );
}

#[test]
fn user_get_decodes_admin_exposed_api_key() {
    let (result, request) = one(
        MockResponse::ok(user_response(7, "executor", Some(USER_API_SECRET))),
        |redmine| redmine.get_user(7),
    );
    let user = result.unwrap();
    assert_eq!(user.id, 7);
    assert_eq!(user.login, "executor");
    assert_eq!(user.api_key_value(), Some(USER_API_SECRET));
    support::assert_request(&request, "GET", "/users/7.json", None);
}

#[test]
fn user_api_key_helper_returns_exposed_key() {
    let (result, request) = one(
        MockResponse::ok(user_response(7, "executor", Some(USER_API_SECRET))),
        |redmine| redmine.get_user_api_key(7),
    );
    assert_eq!(result.unwrap(), USER_API_SECRET);
    support::assert_request(&request, "GET", "/users/7.json", None);
}

#[test]
fn user_create_validation_rejects_empty_fields_without_http() {
    let redmine = provider("http://127.0.0.1:9".to_owned());
    for (login, firstname, lastname, mail, password, fragment) in [
        ("", "Test", "User", "a@example.test", "pw", "login"),
        ("executor", "", "User", "a@example.test", "pw", "firstname"),
        ("executor", "Test", "", "a@example.test", "pw", "lastname"),
        ("executor", "Test", "User", "", "pw", "mail"),
        ("executor", "Test", "User", "a@example.test", "", "password"),
        ("   ", "Test", "User", "a@example.test", "pw", "login"),
    ] {
        let error = redmine
            .create_user(login, firstname, lastname, mail, password)
            .unwrap_err();
        assert_eq!(error.json()["kind"], "config", "{fragment}");
        assert!(
            error.to_string().contains(fragment),
            "missing {fragment} in {error}"
        );
        assert!(
            !error.to_string().contains("s3cr3t"),
            "validation error must not echo secrets: {error}"
        );
    }
}

#[test]
fn user_get_validation_rejects_zero_id() {
    let redmine = provider("http://127.0.0.1:9".to_owned());
    let error = redmine.get_user(0).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("greater than zero"));
}

#[test]
fn user_create_surfaces_server_validation_errors() {
    let body = r#"{"errors":["Login has already been taken","Mail is invalid"]}"#;
    let (result, request) = one(MockResponse::error(422, body), |redmine| {
        redmine.create_user("executor", "Test", "User", "bad-mail", "s3cr3t-password")
    });
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 422);
    assert_eq!(json["operation"], "user create");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("Login has already been taken"),
        "{message}"
    );
    assert!(message.contains("Mail is invalid"), "{message}");
    support::assert_request(&request, "POST", "/users.json", None);
}

#[test]
fn user_get_surfaces_not_found() {
    let (result, request) = one(
        MockResponse::error(404, r#"{"errors":["User not found"]}"#),
        |redmine| redmine.get_user(99),
    );
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 404);
    assert_eq!(json["operation"], "user get");
    support::assert_request(&request, "GET", "/users/99.json", None);
}

#[test]
fn user_errors_redact_admin_api_key() {
    let (base, requests, server) = sequence(vec![
        MockResponse::error(422, format!(r#"{{"errors":["bad {TEST_API_KEY}"]}}"#)),
        MockResponse::error(403, format!(r#"{{"errors":["denied {TEST_API_KEY}"]}}"#)),
    ]);
    let redmine = provider(base);
    let create_error = redmine
        .create_user(
            "executor",
            "Test",
            "User",
            "executor@example.test",
            "s3cr3t-password",
        )
        .unwrap_err();
    let get_error = redmine.get_user(7).unwrap_err();
    for error in [&create_error, &get_error] {
        let rendered = error.json().to_string();
        assert!(!rendered.contains(TEST_API_KEY), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!error.to_string().contains(TEST_API_KEY));
    }
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "POST", "/users.json", None);
    support::assert_request(&requests[1], "GET", "/users/7.json", None);
    server.join().unwrap();
}

#[test]
fn user_debug_redacts_api_key_and_password() {
    let response: RedmineUserResponse =
        serde_json::from_str(&user_response(7, "executor", Some(USER_API_SECRET))).unwrap();
    let rendered = format!("{:?}", response.user);
    assert!(!rendered.contains(USER_API_SECRET), "{rendered}");
    assert!(rendered.contains("[redacted]"), "{rendered}");

    let payload = RedmineNewUser::new(
        "executor",
        "Test",
        "User",
        "executor@example.test",
        Some("super-secret-pw"),
    );
    let rendered = format!("{payload:?}");
    assert!(!rendered.contains("super-secret-pw"), "{rendered}");
    assert!(rendered.contains("[redacted]"), "{rendered}");
}

#[test]
fn user_decode_error_does_not_echo_api_key() {
    let body = format!("not-json {DECODE_SECRET}");
    let (result, _) = one(MockResponse::ok(body), |redmine| redmine.get_user(7));
    let error = result.unwrap_err();
    let rendered = error.json().to_string();
    assert_eq!(error.json()["kind"], "decode");
    assert_eq!(error.json()["operation"], "user get");
    assert!(!rendered.contains(DECODE_SECRET), "{rendered}");
}

#[test]
fn user_api_key_missing_is_decode_without_payload() {
    let (result, _) = one(
        MockResponse::ok(user_response(7, "executor", None)),
        |redmine| redmine.get_user_api_key(7),
    );
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "decode");
    assert_eq!(json["operation"], "user get");
    assert!(
        json["message"]
            .as_str()
            .unwrap()
            .contains("missing API key"),
        "{json}"
    );
    assert!(!error.to_string().contains(USER_API_SECRET));
}

fn user_list_response(users: &[(u64, &str)]) -> String {
    serde_json::json!({
        "users": users.iter().map(|(id, login)| serde_json::json!({
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        })).collect::<Vec<_>>(),
        "total_count": users.len(),
        "limit": 100,
    })
    .to_string()
}

#[test]
fn user_list_decodes_paginated_users() {
    let (result, request) = one(
        MockResponse::ok(user_list_response(&[(11, "phasegent-orchestrator")])),
        |redmine| redmine.list_users(),
    );
    let users = result.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, 11);
    assert_eq!(users[0].login, "phasegent-orchestrator");
    support::assert_request(&request, "GET", "/users.json?", None);
    assert!(request.contains("limit=100"), "{request}");
}

#[test]
fn user_find_by_login_returns_exact_match() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(user_list_response(&[
        (11, "phasegent-orchestrator"),
        (22, "phasegent-executor"),
    ]))]);
    let redmine = provider(base);
    let found = redmine
        .find_user_by_login("phasegent-executor")
        .unwrap()
        .expect("must find executor");
    assert_eq!(found.id, 22);
    assert_eq!(found.login, "phasegent-executor");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    support::assert_request(&requests[0], "GET", "/users.json?", None);
    server.join().unwrap();
}

#[test]
fn user_find_by_login_returns_none_when_absent() {
    let (result, request) = one(MockResponse::ok(user_list_response(&[])), |redmine| {
        redmine.find_user_by_login("phasegent-orchestrator")
    });
    assert!(result.unwrap().is_none());
    support::assert_request(&request, "GET", "/users.json?", None);
}

#[test]
fn user_find_by_login_rejects_blank_without_http() {
    let redmine = provider("http://127.0.0.1:9".to_owned());
    let error = redmine.find_user_by_login("   ").unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("login"));
}

#[test]
fn user_find_by_login_scans_second_page() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(
            serde_json::json!({
                "users": [{"id": 11, "login": "phasegent-orchestrator", "firstname": "Phasegent", "lastname": "Orchestrator", "mail": "phasegent-orchestrator@phasegent.local"}],
                "total_count": 2,
                "limit": 1,
            })
            .to_string(),
        ),
        MockResponse::ok(
            serde_json::json!({
                "users": [{"id": 22, "login": "phasegent-executor", "firstname": "Phasegent", "lastname": "Executor", "mail": "phasegent-executor@phasegent.local"}],
                "total_count": 2,
                "limit": 1,
            })
            .to_string(),
        ),
    ]);
    let redmine = provider(base);
    let found = redmine
        .find_user_by_login("phasegent-executor")
        .unwrap()
        .expect("must find on second page");
    assert_eq!(found.id, 22);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("offset=0"), "{}", requests[0]);
    assert!(requests[1].contains("offset=1"), "{}", requests[1]);
    server.join().unwrap();
}

#[test]
fn service_user_create_uses_generate_password_without_password_field() {
    let (result, request) = one(
        MockResponse::status(201, user_response(11, "phasegent-orchestrator", None)),
        |redmine| {
            redmine.create_service_user(
                "phasegent-orchestrator",
                "Phasegent",
                "Orchestrator",
                "phasegent-orchestrator@phasegent.local",
            )
        },
    );
    let user = result.unwrap();
    assert_eq!(user.id, 11);
    assert_eq!(user.login, "phasegent-orchestrator");
    support::assert_request(&request, "POST", "/users.json", None);
    assert!(
        request.contains(r#""login":"phasegent-orchestrator""#),
        "{request}"
    );
    assert!(
        request.contains(r#""generate_password":true"#),
        "service create must request generated password: {request}"
    );
    assert!(
        !request.contains(r#""password""#),
        "service create must not send a password field: {request}"
    );
    assert!(
        request.contains(r#""status":1"#),
        "service create must mark active: {request}"
    );
    assert!(
        request.contains(r#""admin":false"#),
        "service create must not grant admin: {request}"
    );
}

#[test]
fn service_user_create_validation_rejects_empty_without_http() {
    let redmine = provider("http://127.0.0.1:9".to_owned());
    for (login, firstname, lastname, mail) in [
        ("", "Phasegent", "Orchestrator", "a@phasegent.local"),
        (
            "phasegent-orchestrator",
            "",
            "Orchestrator",
            "a@phasegent.local",
        ),
        (
            "phasegent-orchestrator",
            "Phasegent",
            "",
            "a@phasegent.local",
        ),
        ("phasegent-orchestrator", "Phasegent", "Orchestrator", ""),
    ] {
        let error = redmine
            .create_service_user(login, firstname, lastname, mail)
            .unwrap_err();
        assert_eq!(error.json()["kind"], "config");
    }
}

#[test]
fn provisioning_metadata_is_deterministic_and_complete() {
    use crate::policy::Role;
    use crate::providers::redmine::model::{provisioned_roles, provisioning_metadata};
    let roles = provisioned_roles();
    assert_eq!(
        roles,
        [
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
            Role::Tester
        ]
    );
    let mut logins = std::collections::HashSet::new();
    for role in roles {
        let meta = provisioning_metadata(role)
            .unwrap_or_else(|| panic!("missing metadata for {}", role.as_str()));
        assert!(!meta.login.is_empty());
        assert!(meta.login.starts_with("phasegent-"), "{}", meta.login);
        assert!(!meta.mail.is_empty());
        assert!(meta.mail.contains('@'), "{}", meta.mail);
        assert!(logins.insert(meta.login), "duplicate login {}", meta.login);
    }
    assert!(
        provisioning_metadata(Role::Admin).is_none(),
        "admin must not have provisioning metadata"
    );
}

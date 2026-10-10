use super::user_list_response;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request, one, provider, sequence,
};

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
    assert_request(&request, "GET", "/users.json?", None);
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
    assert_request(&requests[0], "GET", "/users.json?", None);
    server.join().unwrap();
}

#[test]
fn user_find_by_login_returns_none_when_absent() {
    let (result, request) = one(MockResponse::ok(user_list_response(&[])), |redmine| {
        redmine.find_user_by_login("phasegent-orchestrator")
    });
    assert!(result.unwrap().is_none());
    assert_request(&request, "GET", "/users.json?", None);
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

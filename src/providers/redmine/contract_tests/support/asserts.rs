#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn assert_request(request: &str, method: &str, path: &str, body: Option<&str>) {
    assert_request_with_key(request, method, path, body, super::TEST_API_KEY);
}

pub(crate) fn assert_request_with_key(
    request: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    api_key: &str,
) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("x-redmine-api-key: {api_key}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing Redmine API key header: {request}"
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

pub(crate) fn assert_request_with_bearer(
    request: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    bearer_key: &str,
) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("authorization: bearer {bearer_key}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing bearer key header: {request}"
    );
    // The mirror plugin does not use the role-scoped Redmine API key.
    assert!(
        !request.to_ascii_lowercase().contains("x-redmine-api-key"),
        "mirror plugin request leaked the role-scoped Redmine API key header: {request}"
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

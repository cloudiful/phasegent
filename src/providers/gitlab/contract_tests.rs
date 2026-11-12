use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderValue};

use super::http::{GitlabHttp, PRIVATE_TOKEN};
use super::model::{Issue, Project};
use super::{GitlabProvider, next_page, page_fingerprint};
use crate::policy::Capability;
use crate::providers::IssueProvider;
use crate::providers::gitlab::GitlabConfig;

fn config() -> GitlabConfig {
    GitlabConfig::new("https://gitlab.example.com", "group/sub/project").unwrap()
}

fn provider() -> GitlabProvider {
    GitlabProvider::new(config(), "token-value").unwrap()
}

#[test]
fn request_encodes_the_whole_project_path_as_one_selector() {
    let http = GitlabHttp::new(&config(), "token-value").unwrap();
    let url = http
        .request(
            Method::GET,
            &["projects", "group/sub/project", "issues", "7"],
        )
        .unwrap()
        .build()
        .unwrap()
        .url()
        .clone();
    assert_eq!(
        url.as_str(),
        "https://gitlab.example.com/api/v4/projects/group%2Fsub%2Fproject/issues/7"
    );
    assert!(!url.as_str().contains("token-value"));
}

#[test]
fn private_token_header_is_sent_and_marked_sensitive() {
    let http = GitlabHttp::new(&config(), "token-value").unwrap();
    let request = http
        .request(Method::GET, &["projects", "group/sub/project"])
        .unwrap()
        .build()
        .unwrap();
    let header = request.headers().get(PRIVATE_TOKEN).unwrap();
    assert_eq!(header.as_bytes(), b"token-value");
    assert!(header.is_sensitive());
}

#[test]
fn next_page_requires_numeric_forward_pages_and_empty_terminal() {
    let mut headers = HeaderMap::new();
    headers.insert("x-next-page", HeaderValue::from_static("2"));
    assert_eq!(next_page(&headers, 1, 100, "issues").unwrap(), Some(2));

    headers.insert("x-next-page", HeaderValue::from_static(""));
    assert_eq!(next_page(&headers, 1, 100, "issues").unwrap(), None);

    headers.insert("x-next-page", HeaderValue::from_static("0"));
    assert!(next_page(&headers, 1, 100, "issues").is_err());

    headers.insert("x-next-page", HeaderValue::from_static("next"));
    assert!(next_page(&headers, 1, 100, "issues").is_err());

    let empty = HeaderMap::new();
    assert_eq!(next_page(&empty, 1, 42, "issues").unwrap(), None);
    assert!(
        next_page(&empty, 1, 100, "issues").is_err(),
        "a full page without pagination metadata cannot be proven complete"
    );
}

#[test]
fn pagination_rejects_skipped_pages() {
    let mut headers = HeaderMap::new();
    headers.insert("x-next-page", HeaderValue::from_static("3"));
    assert!(next_page(&headers, 1, 100, "issues").is_err());
}

#[test]
fn pagination_errors_redact_untrusted_header_values() {
    let mut headers = HeaderMap::new();
    headers.insert("x-next-page", HeaderValue::from_static("token-value"));
    let error = next_page(&headers, 1, 100, "issues").unwrap_err();
    assert!(!error.to_string().contains("token-value"));
}

#[test]
fn repeated_page_fingerprints_are_detected() {
    let first = page_fingerprint(r#"[{"id":1}]"#, "issues").unwrap();
    let second = page_fingerprint(r#" [ {"id" : 1} ] "#, "issues").unwrap();
    assert_eq!(first, second);
    assert!(page_fingerprint("not-json", "issues").is_err());
}

#[test]
fn dto_maps_iid_as_the_cli_number_and_native_state() {
    let provider = provider();
    let project = Project {
        id: 3,
        name: "test3".to_owned(),
        path_with_namespace: "group/sub/project".to_owned(),
        description: None,
        visibility: Some("private".to_owned()),
    };
    let issue = Issue {
        id: 9001,
        iid: 7,
        title: "A title".to_owned(),
        description: Some("body".to_owned()),
        state: "opened".to_owned(),
        web_url: Some("https://gitlab.example.com/group/sub/project/-/issues/7".to_owned()),
    };
    let summary = provider.summary(issue, &project, Some(7)).unwrap();
    assert_eq!(summary.id, 9001, "global id stays diagnostic only");
    assert_eq!(
        summary.number, 7,
        "the project-scoped iid is the CLI number"
    );
    assert_eq!(summary.state, "open");
    assert_eq!(summary.body, "body");
    assert_eq!(summary.project.as_ref().unwrap().id, 3);
    assert_eq!(
        summary.project.as_ref().unwrap().identifier.as_deref(),
        Some("group/sub/project")
    );

    let wrong_iid = Issue {
        id: 9001,
        iid: 8,
        title: "A title".to_owned(),
        description: None,
        state: "opened".to_owned(),
        web_url: None,
    };
    assert!(provider.summary(wrong_iid, &project, Some(7)).is_err());

    let unknown_state = Issue {
        id: 9001,
        iid: 7,
        title: "A title".to_owned(),
        description: None,
        state: "locked".to_owned(),
        web_url: None,
    };
    assert!(provider.summary(unknown_state, &project, Some(7)).is_err());
}

#[test]
fn unsupported_capabilities_are_rejected_without_io() {
    let provider = provider();
    for capability in [
        Capability::IssueRead,
        Capability::IssueSearch,
        Capability::IssueCreate,
        Capability::IssueUpdateBody,
        Capability::IssueClose,
        Capability::CommentCreate,
        Capability::CommentRead,
        Capability::CommentFindMarker,
        Capability::ProjectRead,
        Capability::IssueStatusRead,
        Capability::VersionRead,
    ] {
        assert!(provider.supports(capability), "{capability:?}");
    }
    for capability in [
        Capability::IssueAttachmentUpload,
        Capability::ProjectCreate,
        Capability::RelationRead,
        Capability::RelationCreate,
        Capability::RelationDelete,
    ] {
        assert!(!provider.supports(capability), "{capability:?}");
    }
}

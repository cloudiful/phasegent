use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::support::server;
use crate::policy::Capability;
use crate::providers::{IssueProvider, RedmineMetadataProvider};

#[test]
fn repository_milestones_and_status_labels_are_read_without_provisioning() {
    let (base, server) = server::sequence(vec![
        r#"{"id":17,"name":"repo","full_name":"owner/repo","private":true}"#.into(),
        r#"[{"id":2,"name":"status/In Progress"},{"id":3,"name":"bug"}]"#.into(),
        "[]".into(),
        r#"[{"id":4,"title":"v1","state":"open"}]"#.into(),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(
        provider.list_projects().unwrap()[0].identifier,
        "owner/repo"
    );
    assert_eq!(
        provider.list_issue_statuses().unwrap()[0].name,
        "In Progress"
    );
    assert_eq!(provider.list_project_versions().unwrap()[0].name, "v1");
    assert!(!provider.supports(Capability::RelationCreate));
    assert!(!provider.supports(Capability::IssueAttachmentUpload));
    assert!(
        provider
            .create_project("bad", "bad", None)
            .unwrap_err()
            .is_not_supported()
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

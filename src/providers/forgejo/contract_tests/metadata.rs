use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::support::server;
use crate::policy::Capability;
use crate::providers::{IssueProvider, RedmineMetadataProvider};

#[test]
fn repository_and_milestones_are_read_while_the_native_statuses_are_local() {
    let (base, server) = server::sequence(vec![
        r#"{"id":17,"name":"repo","full_name":"owner/repo","private":true}"#.into(),
        r#"[{"id":4,"title":"v1","state":"open"}]"#.into(),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(
        provider.list_projects().unwrap()[0].identifier,
        "owner/repo"
    );
    let statuses = provider.list_issue_statuses().unwrap();
    assert_eq!(
        statuses
            .iter()
            .map(|status| (status.id, status.name.as_str(), status.is_closed))
            .collect::<Vec<_>>(),
        vec![(1, "Open", false), (0, "Closed", true)]
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
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "the native status catalogue reads no label catalogue: {requests:?}"
    );
}

use super::*;

#[test]
fn role_policy_remains_capability_based() {
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Admin.allows(Capability::RepoCreate));
    assert!(!Role::Admin.allows(Capability::IssueSearch));
    assert!(!Role::Admin.allows(Capability::IssueCreate));
    assert!(Role::Orchestrator.allows(Capability::IssueClose));
    assert!(Role::Executor.allows(Capability::IssueRead));
    assert!(Role::Executor.allows(Capability::CommentRead));
    assert!(Role::Executor.allows(Capability::CommentFindMarker));
    assert!(Role::Executor.allows(Capability::CommentCreate));
    assert!(!Role::Executor.allows(Capability::IssueSearch));
    assert!(!Role::Executor.allows(Capability::IssueCreate));
    assert!(!Role::Executor.allows(Capability::IssueUpdateBody));
    assert!(!Role::Executor.allows(Capability::IssueClose));
    assert!(Role::Reviewer.allows(Capability::IssueRead));
    assert!(Role::Reviewer.allows(Capability::CommentRead));
    assert!(Role::Reviewer.allows(Capability::CommentFindMarker));
    assert!(Role::Reviewer.allows(Capability::CommentCreate));
    assert!(!Role::Reviewer.allows(Capability::IssueSearch));
    assert!(!Role::Reviewer.allows(Capability::IssueCreate));
    assert!(!Role::Reviewer.allows(Capability::IssueUpdateBody));
    assert!(!Role::Reviewer.allows(Capability::IssueClose));
    assert!(!Role::Executor.allows(Capability::RepoCreate));
    assert!(!Role::Reviewer.allows(Capability::RepoCreate));
    assert!(Role::Orchestrator.allows(Capability::RepoCreate));
    assert!(Role::Tester.allows(Capability::IssueRead));
    assert!(Role::Tester.allows(Capability::CommentRead));
    assert!(Role::Tester.allows(Capability::CommentFindMarker));
    assert!(Role::Tester.allows(Capability::CommentCreate));
    assert!(Role::Tester.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Tester.allows(Capability::IssueSearch));
    assert!(!Role::Tester.allows(Capability::IssueCreate));
    assert!(!Role::Tester.allows(Capability::IssueUpdateBody));
    assert!(!Role::Tester.allows(Capability::IssueClose));
    assert!(!Role::Tester.allows(Capability::RepoCreate));
    assert!(!Role::Tester.allows(Capability::ProjectCreate));
    assert!(!Role::Tester.allows(Capability::IssueStatusRead));
    assert!(!Role::Tester.allows(Capability::VersionRead));
    assert!(!Role::Tester.allows(Capability::RelationRead));
}

#[test]
fn repo_create_requires_private_and_valid_owner_repository() {
    let base = ["repo", "create", "owner/new-repo"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        command::parse_with_role_env(&base, Some("orchestrator")).unwrap_err(),
        "repo create requires --private"
    );

    for suffix in ["--public", "--unknown"] {
        let args = ["repo", "create", "owner/new-repo", "--private", suffix]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(command::parse_with_role_env(&args, Some("orchestrator")).is_err());
    }

    for target in ["", "/repo", "owner/", "owner/repo/extra", "owner/repo name"] {
        let args = ["repo", "create", target, "--private"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(
            command::parse_with_role_env(&args, Some("orchestrator")).is_err(),
            "accepted target {target:?}"
        );
    }

    let args = [
        "repo",
        "create",
        "owner/new-repo",
        "--private",
        "--description",
        "description",
        "--auto-init",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Repo(command::RepoCommand::Create {
            target,
            private,
            description,
            auto_init,
        }) => {
            assert_eq!(target, "owner/new-repo");
            assert!(private);
            assert_eq!(description, "description");
            assert!(auto_init);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn executor_and_reviewer_cannot_mutate_issues() {
    for role in [Role::Executor, Role::Reviewer] {
        for capability in [
            Capability::IssueCreate,
            Capability::IssueUpdateBody,
            Capability::IssueClose,
        ] {
            assert!(
                !role.allows(capability),
                "{role} unexpectedly allowed {capability:?}"
            );
        }
        assert!(role.allows(Capability::IssueRead));
        assert!(role.allows(Capability::CommentRead));
        assert!(role.allows(Capability::CommentFindMarker));
    }
    assert!(Role::Orchestrator.allows(Capability::IssueCreate));
    assert!(Role::Orchestrator.allows(Capability::IssueUpdateBody));
    assert!(Role::Orchestrator.allows(Capability::IssueClose));
}

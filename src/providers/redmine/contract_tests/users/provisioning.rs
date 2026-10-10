use super::user_response;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request, one, provider,
};

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
    assert_request(&request, "POST", "/users.json", None);
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
    use crate::providers::redmine::model::{
        default_redmine_role, provisioned_roles, provisioning_metadata,
    };
    let roles = provisioned_roles();
    assert_eq!(
        roles,
        [
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
            Role::Explore
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
        // Every provisioned role must also carry a project membership
        // default, so the bootstrap pass cannot enumerate a role it cannot
        // reconcile.
        assert!(
            default_redmine_role(role).is_some(),
            "missing default Redmine role for {}",
            role.as_str()
        );
    }
    assert!(
        provisioning_metadata(Role::Admin).is_none(),
        "admin must not have provisioning metadata"
    );
    assert!(
        default_redmine_role(Role::Admin).is_none(),
        "admin must not have a default project role"
    );
}

#[test]
fn default_redmine_roles_match_the_documented_mapping() {
    use crate::policy::Role;
    use crate::providers::redmine::model::default_redmine_role;
    use crate::providers::redmine::model::project::{
        DEFAULT_REDMINE_ROLE_EXECUTOR, DEFAULT_REDMINE_ROLE_EXPLORE,
        DEFAULT_REDMINE_ROLE_ORCHESTRATOR, DEFAULT_REDMINE_ROLE_REVIEWER,
    };
    assert_eq!(DEFAULT_REDMINE_ROLE_ORCHESTRATOR, "Maintainer");
    assert_eq!(DEFAULT_REDMINE_ROLE_EXECUTOR, "Developer");
    assert_eq!(DEFAULT_REDMINE_ROLE_REVIEWER, "Reporter");
    assert_eq!(DEFAULT_REDMINE_ROLE_EXPLORE, "Reporter");
    assert_eq!(
        default_redmine_role(Role::Orchestrator),
        Some(DEFAULT_REDMINE_ROLE_ORCHESTRATOR)
    );
    assert_eq!(
        default_redmine_role(Role::Executor),
        Some(DEFAULT_REDMINE_ROLE_EXECUTOR)
    );
    assert_eq!(
        default_redmine_role(Role::Reviewer),
        Some(DEFAULT_REDMINE_ROLE_REVIEWER)
    );
    assert_eq!(
        default_redmine_role(Role::Explore),
        Some(DEFAULT_REDMINE_ROLE_EXPLORE)
    );
}

#[test]
fn explore_provisioning_metadata_is_deterministic_and_distinct() {
    use crate::policy::Role;
    use crate::providers::redmine::model::provisioning_metadata;
    let explore = provisioning_metadata(Role::Explore).expect("explore must be provisioned");
    assert_eq!(explore.login, "phasegent-explore");
    assert_eq!(explore.firstname, "Phasegent");
    assert_eq!(explore.lastname, "Explore");
    assert_eq!(explore.mail, "phasegent-explore@phasegent.local");
    for role in [Role::Orchestrator, Role::Executor, Role::Reviewer] {
        let other = provisioning_metadata(role).expect("agent role metadata");
        assert_ne!(other.login, explore.login, "login collision with {role}");
        assert_ne!(other.mail, explore.mail, "mail collision with {role}");
    }
}

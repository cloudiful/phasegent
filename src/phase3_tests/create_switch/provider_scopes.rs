//! Local-only provider scoping for the default create path (issue 628).
//!
//! Moved verbatim from the parent module. Scope derivation never touches
//! the network or provider clients: `provider_scope` reads local
//! dispatcher config and `explicit_scope` reads explicit CLI args only.
//! A Redmine dispatcher without a project id fails explicitly instead
//! of guessing a scope.

use super::{canonical_key, current_branch, pin_temp_db, switch_repo};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::lifecycle;

#[test]
fn provider_scope_comes_from_local_config_without_requests() {
    use crate::providers::ProviderDispatcher;
    use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
    use crate::providers::gitlab::GitlabProvider;
    use crate::providers::gitlab::http::GitlabHttp;
    use crate::providers::index_store::provider_scope;
    use crate::providers::{GitlabConfig, RedmineConfig, RedmineProvider};

    let forgejo = ProviderDispatcher::Forgejo(
        ForgejoProvider::new(
            ForgejoConfig::new("https://forge.example", "acme", "widgets"),
            "test-key".to_owned(),
        )
        .expect("forgejo provider builds without I/O"),
    );
    let scope = provider_scope(&forgejo).expect("forgejo scope");
    assert_eq!(scope.source, "forgejo");
    assert_eq!(scope.project, "acme/widgets");

    let redmine = ProviderDispatcher::Redmine(
        RedmineProvider::new(
            RedmineConfig::new("https://redmine.example", "tools-phasegent", 2),
            "test-key".to_owned(),
        )
        .expect("redmine provider builds without I/O"),
    );
    let scope = provider_scope(&redmine).expect("redmine scope");
    assert_eq!(scope.source, "redmine");
    assert_eq!(scope.project, "tools-phasegent");

    // Redmine without a project id fails explicitly (config-kind
    // error naming --project-id) instead of guessing a scope.
    let mut bare = RedmineConfig::new("https://redmine.example", "tools-phasegent", 2);
    bare.project_id = None;
    let redmine_bare = ProviderDispatcher::Redmine(
        RedmineProvider::new(bare, "test-key".to_owned()).expect("bare redmine builds"),
    );
    let error = provider_scope(&redmine_bare).expect_err("missing project must fail");
    assert_eq!(error.json()["kind"], serde_json::json!("config"));
    assert!(
        error.json()["message"]
            .as_str()
            .is_some_and(|message| message.contains("--project-id")),
        "the error must name the fix: {error}"
    );

    let gitlab = ProviderDispatcher::Gitlab(GitlabProvider {
        config: GitlabConfig::new("https://gitlab.example/api/v4", 42),
        http: GitlabHttp::new(
            "https://gitlab.example/api/v4".to_owned(),
            "test-key".to_owned(),
        )
        .expect("gitlab http builds without I/O"),
    });
    let scope = provider_scope(&gitlab).expect("gitlab scope");
    assert_eq!(scope.source, "gitlab");
    assert_eq!(scope.project, "42");
    // Local is covered by construction: its arm reads no config and
    // yields ("local", "default"), but `LocalProvider` holds a private
    // connection so no dispatcher is built here.
}

#[test]
fn explicit_scope_needs_only_cli_args_and_rejects_guesses() {
    use crate::providers::ProviderKind;
    use crate::providers::index_store::explicit_scope;

    let forgejo = explicit_scope(Some(ProviderKind::Forgejo), Some("acme/widgets"), None)
        .expect("forgejo derives from --repository");
    assert_eq!(forgejo.source, "forgejo");
    assert_eq!(forgejo.project, "acme/widgets");
    assert!(
        explicit_scope(Some(ProviderKind::Forgejo), Some("bare-repo"), None).is_none(),
        "malformed owner/repo never yields a scope"
    );
    assert!(
        explicit_scope(Some(ProviderKind::Forgejo), None, None).is_none(),
        "missing repository never yields a scope"
    );
    let redmine = explicit_scope(Some(ProviderKind::Redmine), None, Some("tools-phasegent"))
        .expect("redmine derives from --project-id");
    assert_eq!(redmine.source, "redmine");
    assert_eq!(redmine.project, "tools-phasegent");
    assert!(
        explicit_scope(Some(ProviderKind::Redmine), None, None).is_none(),
        "missing project never yields a scope"
    );
    let gitlab = explicit_scope(Some(ProviderKind::Gitlab), None, Some("42"))
        .expect("gitlab derives from a numeric project id");
    assert_eq!(gitlab.project, "42");
    assert!(
        explicit_scope(Some(ProviderKind::Gitlab), None, Some("not-a-number")).is_none(),
        "non-numeric GitLab ids never yield a scope"
    );
    assert!(
        explicit_scope(Some(ProviderKind::Local), None, None).is_none()
            && explicit_scope(None, Some("acme/widgets"), Some("1")).is_none(),
        "local and unknown kinds fall back to global scope"
    );
}

#[test]
fn create_switch_forgejo_scope_links_and_moves() {
    // The scoped link/switch helper is provider-agnostic: a Forgejo
    // scope links under ("forgejo", "acme/widgets") and the safe
    // switch gates apply unchanged.
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("forgejo-scope") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let params = lifecycle::IssueSwitchParams {
        repo_path: &repo.0,
        issue_id: 628,
        branch: "feat/628",
        base: None,
        scope_provider: "forgejo",
        scope_project: Some("acme/widgets"),
        explicit_repository: None,
        session: None,
    };
    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    assert_eq!(
        outcome,
        lifecycle::CreateSwitchOutcome::Switched {
            branch: "feat/628".to_owned(),
            issue_id: 628,
        }
    );
    assert_eq!(current_branch(&repo), "feat/628");
    let storage = crate::infra::storage::Storage::open_at(&db.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    let rows = crate::branch_links::issues_for_branch(
        &storage.connection,
        &canonical_key(),
        "feat/628",
        false,
        &crate::branch_links::UnknownState,
    )
    .expect("read must work");
    assert_eq!(rows.len(), 1, "exactly one link row: {rows:?}");
    assert_eq!(rows[0].issue.provider, "forgejo");
    assert_eq!(rows[0].issue.project, "acme/widgets");
    assert_eq!(rows[0].issue_number, 628);
}

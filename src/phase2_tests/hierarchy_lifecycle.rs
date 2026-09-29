//! Focused execution-lifecycle tests for the native hierarchy commands
//! (issue 641): provider-boundary exit codes through the CLI executor,
//! dispatcher routing without network, and the no-cascade-on-close rule.
//! Moved out of `issue_commands.rs` so each hierarchy file stays under the
//! size budget. Parser shapes and role gates live in `hierarchy_cli`.

use super::owned_args;
use super::support::close_cli_root;
use super::*;

#[test]
fn hierarchy_rejects_unsupported_providers_before_any_access() {
    // Local and Forgejo expose no native hierarchy surface: the CLI rejects
    // with a structured not-supported error (exit 1) before any provider
    // build, credential lookup, or network access, so no storage setup is
    // needed for these cases.
    for argv in [
        vec!["--provider", "local", "hierarchy", "get", "641"],
        vec!["--provider", "forgejo", "hierarchy", "get", "641"],
        vec![
            "--provider",
            "local",
            "hierarchy",
            "set",
            "--parent",
            "640",
            "--child",
            "641",
        ],
        vec![
            "--provider",
            "forgejo",
            "hierarchy",
            "set",
            "--parent",
            "640",
            "--child",
            "641",
        ],
        vec![
            "--provider",
            "local",
            "hierarchy",
            "unset",
            "--child",
            "641",
        ],
        vec![
            "--provider",
            "forgejo",
            "hierarchy",
            "unset",
            "--child",
            "641",
        ],
    ] {
        let args = owned_args(&argv);
        assert_eq!(
            crate::cli::run_with_role(args, Some("orchestrator")),
            1,
            "unsupported provider must fail with not-supported"
        );
    }
    // Role denials keep the permission exit code without provider access.
    assert_eq!(
        crate::cli::run_with_role(
            owned_args(&["hierarchy", "set", "--parent", "640", "--child", "641"]),
            Some("executor")
        ),
        3
    );
    assert_eq!(
        crate::cli::run_with_role(owned_args(&["hierarchy", "get", "641"]), Some("admin")),
        3
    );
}

#[test]
fn hierarchy_dispatcher_rejects_local_without_network() {
    // The dispatcher Local arm returns structured not_supported for every
    // hierarchy operation before touching the provider, so a store with no
    // Redmine/GitLab credentials proves no request leaves the process.
    let provider = crate::providers::local::LocalProvider::open_at(
        &close_cli_root("hierarchy-local").join("local.sqlite3"),
    )
    .unwrap();
    let dispatcher = crate::providers::ProviderDispatcher::local(provider);
    let get = dispatcher.get_hierarchy_page(641).unwrap_err();
    assert_eq!(get.json()["kind"], "not_supported");
    assert_eq!(get.json()["operation"], "issue hierarchy get");
    let set = dispatcher.set_hierarchy_parent_by_id(641, 640).unwrap_err();
    assert_eq!(set.json()["kind"], "not_supported");
    assert_eq!(set.json()["operation"], "issue hierarchy update");
    let unset = dispatcher.unset_hierarchy_parent_by_id(641).unwrap_err();
    assert_eq!(unset.json()["kind"], "not_supported");
    assert_eq!(unset.json()["operation"], "issue hierarchy update");
}

/// Issue 641 P4b no-cascade regression: closing a parent issue never
/// closes (or otherwise mutates) its children. Each child keeps its own
/// lifecycle state; the parent completion reads verified child outcomes
/// instead of cascading.
#[test]
fn closing_a_parent_issue_never_cascades_to_its_children() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("hierarchy-no-cascade");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let index_db = root.join("phasegent-index.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _index_guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        index_db.to_string_lossy().as_ref(),
    );

    // Seed one parent and one child directly in the local provider. The
    // local schema has no parent column: the parent/child edge is a
    // provider-native hierarchy concept, and this regression pins the
    // close path, which must stay silent about it either way.
    let provider = crate::providers::local::LocalProvider::open().unwrap();
    let parent = provider.create_issue("Umbrella parent", "body").unwrap();
    let child = provider.create_issue("Focused child", "body").unwrap();
    // `New -> Closed` is not allowed by policy, so move both to a
    // closable status; only the parent gets closed below.
    for number in [parent.number, child.number] {
        provider
            .with_conn("seed", |conn| {
                conn.execute(
                    "UPDATE local_issues SET status='Resolved' WHERE id=?1",
                    rusqlite::params![number as i64],
                )?;
                Ok(())
            })
            .unwrap();
    }

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number: parent.number,
            worktree_session: None,
        },
    );
    assert_eq!(exit, 0, "the parent close must succeed");

    // The parent is closed; the child keeps its own open, independently
    // controlled state.
    let closed_parent = provider.get_issue(parent.number).unwrap();
    assert_eq!(closed_parent.state, "closed");
    let untouched_child = provider.get_issue(child.number).unwrap();
    assert_eq!(
        untouched_child.state, "open",
        "closing the parent must never cascade to the child"
    );
    assert_eq!(untouched_child.title, "Focused child");
    let _ = fs::remove_dir_all(root);
}

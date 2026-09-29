//! Focused execution-lifecycle tests for the native hierarchy commands
//! (issue 641 P4a): provider-boundary exit codes through the CLI executor
//! and dispatcher routing without network. Moved out of `issue_commands.rs`
//! so each hierarchy file stays under the size budget. Parser shapes and
//! role gates live in `hierarchy_cli`.

use super::owned_args;
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
fn hierarchy_dispatcher_rejects_forgejo_without_network() {
    // The dispatcher Forgejo arm returns structured not_supported for every
    // hierarchy operation before touching the provider, so a closed-port
    // base proves no request leaves the process.
    let provider = ForgejoProvider::new(
        ForgejoConfig::new("http://127.0.0.1:1", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let dispatcher = crate::providers::ProviderDispatcher::Forgejo(provider);
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

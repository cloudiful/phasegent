//! Explorer delegation surface tests (issue 685 P2).
//!
//! Two things are under test and they are deliberately separate. The *gate*
//! tests drive the advertised and dispatched tool names per role, so a role
//! that may not delegate can neither see nor call the surface, and a denial
//! names the delegation operation. The *binding* tests drive the delegation
//! contract itself — session validation and the session-bound lease
//! resolution — because that is where a model-supplied value would have to
//! become an authorized one.
//!
//! Nothing here spawns an ACP process, opens a transport, or reads a
//! credential: the run lifecycle is covered by the `mcp::agent` tests, and the
//! wire shape of a run is covered by the projection tests below.

use super::*;
use crate::command::explorer::{self, DELEGATION_ROLES, HOST_SESSION_FIELD};
use crate::infra::storage::Storage;
use crate::mcp::agent::store_tests::open_storage;
use crate::mcp::explorer_tools::{
    ExplorerResumeParams, ExplorerRunParams, ExplorerStartParams, ExplorerWaitParams,
};
use crate::mcp::tools::{McpConfig, PhasegentMcpServer};
use crate::policy::Role;
use crate::worktree::leases::{NewLease, insert_lease};
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, ErrorCode, InitializeRequestParams, NumberOrString, ProtocolVersion,
};
use rmcp::service::{Peer, RequestContext};

const ALL_ROLES: &[Role] = &[
    Role::Admin,
    Role::Orchestrator,
    Role::Executor,
    Role::Reviewer,
    Role::Tester,
];

const EXPLORER_NAMES: &[&str] = &[
    "explorer_start",
    "explorer_status",
    "explorer_wait",
    "explorer_cancel",
    "explorer_resume",
];

fn server(role: Role) -> PhasegentMcpServer {
    PhasegentMcpServer::new(McpConfig::new(role, None, None, None, None, None, false))
}

/// Seed one `active` lease for `(issue, session)`.
fn seed_lease(storage: &Storage, issue: u64, session: &str, worktree: &str) {
    insert_lease(
        storage,
        NewLease {
            lease_id: &format!("lease-{issue}-{session}"),
            identity: "repo-identity",
            issue,
            session,
            checkout_path: worktree,
            worktree_path: worktree,
            branch: &format!("phasegent/{issue}"),
            status: crate::worktree::LEASE_STATUS_ACTIVE,
            created_at: 1,
            heartbeat_at: 1,
        },
    )
    .expect("seed lease");
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// The delegation roles are advertised; `admin` and `tester` are not, and a
/// denied role never sees the surface over the protocol path either.
#[test]
fn the_explorer_surface_is_advertised_only_to_delegation_roles() {
    for role in ALL_ROLES {
        let handler = server(*role);
        for name in EXPLORER_NAMES {
            assert_eq!(
                handler.get_tool(name).is_some(),
                DELEGATION_ROLES.contains(role),
                "{name} visibility for {role}"
            );
        }
        assert_eq!(
            handler.gate(tool_registry::EXPLORER_START).is_ok(),
            DELEGATION_ROLES.contains(role),
            "explorer_start gate for {role}"
        );
    }
}

/// Every explorer tool shares one declared gate, so a role that may delegate
/// one may delegate all five and a role that may delegate none can reach none.
#[test]
fn every_explorer_tool_shares_the_delegation_gate() {
    for tool in tool_registry::EXPLORER_TOOLS {
        for role in ALL_ROLES {
            assert_eq!(
                tool.allows_role(*role),
                DELEGATION_ROLES.contains(role),
                "{} for {role}",
                tool.name
            );
        }
    }
}

/// A denied delegation names the delegation operation and the stable
/// permission wording, exactly like a denied tracking tool.
#[test]
fn a_denied_delegation_names_the_delegation_operation() {
    for role in [Role::Admin, Role::Tester] {
        for tool in tool_registry::EXPLORER_TOOLS {
            let error = server(role)
                .gate(*tool)
                .expect_err("a denied delegation must fail closed");
            assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
            assert!(
                error.message.contains(explorer::EXPLORER_OPERATION),
                "{} denial for {role}: {}",
                tool.name,
                error.message
            );
            assert!(
                error.message.contains("is not allowed to perform"),
                "{} denial for {role}: {}",
                tool.name,
                error.message
            );
        }
    }
}

async fn protocol_peer(role: Role) -> Peer<RoleServer> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let params = serde_json::to_value(InitializeRequestParams::default()).expect("initialize");
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": params,
    });
    let mut client_io = client_io;
    tokio::io::AsyncWriteExt::write_all(&mut client_io, format!("{initialize}\n").as_bytes())
        .await
        .expect("write initialize request");
    let running = rmcp::serve_server(server(role), server_io)
        .await
        .expect("in-process MCP handshake");
    let peer = running.peer().clone();
    let _ = running.cancel().await;
    peer
}

fn protocol_context(peer: Peer<RoleServer>) -> RequestContext<RoleServer> {
    let mut context = RequestContext::new(NumberOrString::Number(1), peer);
    context
        .meta
        .set_protocol_version(ProtocolVersion::V_2026_07_28);
    context
}

/// The protocol path agrees with the handler gate in both directions: a
/// delegation role is advertised, and a denied role's `tools/call` is rejected
/// before the router deserializes the arguments.
#[tokio::test]
async fn the_protocol_path_agrees_with_the_delegation_gate() {
    for role in ALL_ROLES {
        let allowed = DELEGATION_ROLES.contains(role);
        let handler = server(*role);
        let result = handler
            .list_tools(None, protocol_context(protocol_peer(*role).await))
            .await
            .expect("tools/list must succeed");
        let listed: Vec<&str> = result.tools.iter().map(|tool| tool.name.as_ref()).collect();
        for name in EXPLORER_NAMES {
            assert_eq!(
                listed.contains(name),
                allowed,
                "tools/list {name} for {role}: {listed:?}"
            );
        }
        if allowed {
            continue;
        }
        let error = handler
            .call_tool(
                CallToolRequestParams::new("explorer_start").with_arguments(
                    serde_json::json!({
                        "issue": 685,
                        "session": "ses_x",
                        "prompt": "recon",
                    })
                    .as_object()
                    .expect("arguments object")
                    .clone(),
                ),
                protocol_context(protocol_peer(*role).await),
            )
            .await
            .expect_err("a denied delegation must be rejected before dispatch");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert!(
            error.message.contains(explorer::EXPLORER_OPERATION),
            "protocol denial for {role}: {}",
            error.message
        );
    }
}

// ---------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------

/// The issue is only a selector: exactly one active lease for the bound
/// session resolves, and a lease owned by another session resolves to nothing.
#[test]
fn a_lease_is_resolved_only_for_the_bound_session() {
    let storage = open_storage("binding-session");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    seed_lease(&storage, 685, "ses_owner", "/tmp/wt-owner");

    let owner = explorer::BoundTarget::resolve(&storage, 685, "ses_owner").expect("owner binding");
    assert_eq!(owner.issue, 685);
    assert_eq!(owner.worktree, std::path::PathBuf::from("/tmp/wt-owner"));

    let foreign = explorer::BoundTarget::resolve(&storage, 685, "ses_other")
        .expect_err("another session's lease is not resolvable");
    assert!(
        foreign.message().contains("no active worktree lease"),
        "{foreign:?}"
    );

    // A different issue with the same session is a different selector, so it
    // resolves to nothing rather than to the issue the session happens to hold.
    assert!(
        explorer::BoundTarget::resolve(&storage, 686, "ses_owner").is_err(),
        "the issue selector must not fall back to another issue"
    );
}

/// Two active leases for one session and issue are ambiguous, never resolved
/// to the newest row.
#[test]
fn two_active_leases_are_ambiguous_rather_than_newest_wins() {
    let storage = open_storage("binding-ambiguous");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    seed_lease(&storage, 685, "ses_owner", "/tmp/wt-a");
    // A second lease for the same triple on a different checkout: the
    // active-only path index is per path, so both rows exist.
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-685-ses_owner-b",
            identity: "repo-identity",
            issue: 685,
            session: "ses_owner",
            checkout_path: "/tmp/wt-b",
            worktree_path: "/tmp/wt-b",
            branch: "phasegent/685-b",
            status: crate::worktree::LEASE_STATUS_ACTIVE,
            created_at: 2,
            heartbeat_at: 2,
        },
    )
    .expect("seed second lease");

    let error = explorer::BoundTarget::resolve(&storage, 685, "ses_owner")
        .expect_err("two active leases must fail closed");
    let message = error.message();
    assert!(
        message.contains("more than one active worktree lease"),
        "{message}"
    );
    assert!(!message.contains("/tmp/wt"), "{message}");
}

/// The issue-only selector is never authorization: with no lease at all the
/// resolution fails, and the refusal names neither a path nor a session.
#[test]
fn a_missing_lease_fails_closed_without_naming_a_path_or_session() {
    let storage = open_storage("binding-missing");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    let error = explorer::BoundTarget::resolve(&storage, 685, "ses_owner")
        .expect_err("no lease must fail closed");
    let message = error.message();
    assert!(message.contains("no active worktree lease"), "{message}");
    assert!(!message.contains("ses_owner"), "{message}");
    assert!(!message.contains('/'), "{message}");
    // A zero issue is refused before the ledger is even read.
    assert!(
        explorer::BoundTarget::resolve(&storage, 0, "ses_owner").is_err(),
        "a zero selector must be refused"
    );
}

/// The host-bound session is validated at the boundary, and a blank or
/// malformed one never becomes a lookup key.
#[test]
fn a_blank_or_malformed_host_session_never_becomes_a_lookup_key() {
    let storage = open_storage("binding-session-invalid");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    seed_lease(&storage, 685, "ses_owner", "/tmp/wt-owner");
    for raw in ["   ", "", "ses\n1", &"s".repeat(400)] {
        assert!(
            explorer::BoundTarget::resolve(&storage, 685, raw).is_err(),
            "host session {raw:?} must be refused"
        );
    }
    // Surrounding whitespace is trimmed, not rejected, so one identity is
    // never spelled two ways.
    assert!(
        explorer::BoundTarget::resolve(&storage, 685, " ses_owner ").is_ok(),
        "a padded but otherwise valid session must resolve"
    );
}

/// Each served tool name is the delegation contract's operation name behind
/// the `explorer_` prefix. OpenCode exposes the tool as
/// `sanitize(server) + "_" + sanitize(tool)`, so this spelling is the host
/// bridge's contract: a rename here without a matching bridge change would
/// silently stop the bridge from binding the session.
#[test]
fn every_tool_name_is_its_operation_behind_the_explorer_prefix() {
    let actions: Vec<String> = explorer::Operation::ALL
        .iter()
        .map(|operation| format!("explorer_{}", operation.action()))
        .collect();
    let served: Vec<&str> = tool_registry::EXPLORER_TOOLS
        .iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(served, actions);
}

/// The host-bound session field is spelled `session` on the wire, and every
/// explorer params struct requires it. A renamed or optional field would let a
/// call reach the server with no host identity at all.
#[test]
fn every_explorer_params_struct_requires_the_host_session_field() {
    let session = serde_json::json!({ HOST_SESSION_FIELD: "ses_x" });
    let run = serde_json::json!({ "run_id": "explorer-1", HOST_SESSION_FIELD: "ses_x" });

    let start = serde_json::json!({ "issue": 685, "prompt": "recon" });
    let mut start_with_session = start.clone();
    start_with_session[HOST_SESSION_FIELD] = session[HOST_SESSION_FIELD].clone();
    serde_json::from_value::<ExplorerStartParams>(start_with_session)
        .expect("explorer_start accepts the host session field");
    assert!(
        serde_json::from_value::<ExplorerStartParams>(start).is_err(),
        "explorer_start must not accept a call without the host session"
    );

    for params in [
        serde_json::from_value::<ExplorerRunParams>(run.clone()).is_ok(),
        serde_json::from_value::<ExplorerWaitParams>(run.clone()).is_ok(),
    ] {
        assert!(params, "run-scoped params accept the host session");
    }
    let without_session = serde_json::json!({ "run_id": "explorer-1" });
    assert!(
        serde_json::from_value::<ExplorerRunParams>(without_session.clone()).is_err(),
        "explorer_status must not accept a call without the host session"
    );
    assert!(
        serde_json::from_value::<ExplorerWaitParams>(without_session.clone()).is_err(),
        "explorer_wait must not accept a call without the host session"
    );

    let resume = serde_json::json!({ "run_id": "explorer-1", "prompt": "continue" });
    let mut resume_with_session = resume.clone();
    resume_with_session[HOST_SESSION_FIELD] = session[HOST_SESSION_FIELD].clone();
    serde_json::from_value::<ExplorerResumeParams>(resume_with_session)
        .expect("explorer_resume accepts the host session field");
    assert!(
        serde_json::from_value::<ExplorerResumeParams>(resume).is_err(),
        "explorer_resume must not accept a call without the host session"
    );
}

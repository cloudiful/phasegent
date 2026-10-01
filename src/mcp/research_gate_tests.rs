//! Research delegation surface tests (issue 692 P1).
//!
//! Two things are under test and they are deliberately separate. The *gate*
//! tests drive the advertised and dispatched tool names per role, so a role
//! that may not delegate can neither see nor call the surface, and a denial
//! names the delegation operation. The *contract* tests pin the generic
//! request shape: there is no issue, worktree, checkout, repository, or
//! location field anywhere in the public surface, and the host-bound session
//! is the only identity the bridge may add.
//!
//! Nothing here spawns an ACP process, opens a transport, or reads a
//! credential: the run lifecycle is covered by the `mcp::agent` tests, and the
//! wire shape of a run is covered by the projection tests in
//! [`crate::mcp::research_tools`].

use super::*;
use crate::command::research::{self, DELEGATION_ROLES, HOST_SESSION_FIELD};
use crate::mcp::research_tools::{
    ResearchResumeParams, ResearchRunParams, ResearchStartParams, ResearchWaitParams,
};
use crate::mcp::tools::{McpConfig, PhasegentMcpServer};
use crate::policy::Role;
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

const RESEARCH_NAMES: &[&str] = &[
    "research_start",
    "research_status",
    "research_wait",
    "research_cancel",
    "research_resume",
];

fn server(role: Role) -> PhasegentMcpServer {
    PhasegentMcpServer::new(McpConfig::new(role, None, None, None, None, None, false))
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// The delegation roles are advertised; `admin` and `tester` are not, and a
/// denied role never sees the surface over the protocol path either.
#[test]
fn the_research_surface_is_advertised_only_to_delegation_roles() {
    for role in ALL_ROLES {
        let handler = server(*role);
        for name in RESEARCH_NAMES {
            assert_eq!(
                handler.get_tool(name).is_some(),
                DELEGATION_ROLES.contains(role),
                "{name} visibility for {role}"
            );
        }
        assert_eq!(
            handler.gate(tool_registry::RESEARCH_START).is_ok(),
            DELEGATION_ROLES.contains(role),
            "research_start gate for {role}"
        );
    }
}

/// Every research tool shares one declared gate, so a role that may delegate
/// one may delegate all five and a role that may delegate none can reach none.
#[test]
fn every_research_tool_shares_the_delegation_gate() {
    for tool in tool_registry::RESEARCH_TOOLS {
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
        for tool in tool_registry::RESEARCH_TOOLS {
            let error = server(role)
                .gate(*tool)
                .expect_err("a denied delegation must fail closed");
            assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
            assert!(
                error.message.contains(research::RESEARCH_OPERATION),
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
        for name in RESEARCH_NAMES {
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
                CallToolRequestParams::new("research_start").with_arguments(
                    serde_json::json!({ "session": "ses_x", "prompt": "summarize the topic" })
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
            error.message.contains(research::RESEARCH_OPERATION),
            "protocol denial for {role}: {}",
            error.message
        );
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Each served tool name is the delegation contract's operation name behind the
/// `research_` prefix. OpenCode exposes the tool as
/// `sanitize(server) + "_" + sanitize(tool)`, so this spelling is the host
/// bridge's contract: a rename here without a matching bridge change would
/// silently stop the bridge from binding the session.
#[test]
fn every_tool_name_is_its_operation_behind_the_research_prefix() {
    let actions: Vec<String> = research::Operation::ALL
        .iter()
        .map(|operation| format!("research_{}", operation.action()))
        .collect();
    let served: Vec<&str> = tool_registry::RESEARCH_TOOLS
        .iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(served, actions);
}

/// The host-bound session field is spelled `session` on the wire, and every
/// research params struct requires it. A renamed or optional field would let a
/// call reach the server with no host identity at all.
#[test]
fn every_research_params_struct_requires_the_host_session_field() {
    let session = serde_json::json!({ HOST_SESSION_FIELD: "ses_x" });
    let run = serde_json::json!({ "run_id": "research-1", HOST_SESSION_FIELD: "ses_x" });

    let start = serde_json::json!({ "prompt": "summarize the topic" });
    let mut start_with_session = start.clone();
    start_with_session[HOST_SESSION_FIELD] = session[HOST_SESSION_FIELD].clone();
    serde_json::from_value::<ResearchStartParams>(start_with_session)
        .expect("research_start accepts the host session field");
    assert!(
        serde_json::from_value::<ResearchStartParams>(start).is_err(),
        "research_start must not accept a call without the host session"
    );

    assert!(
        serde_json::from_value::<ResearchRunParams>(run.clone()).is_ok(),
        "run-scoped params accept the host session"
    );
    assert!(
        serde_json::from_value::<ResearchWaitParams>(run.clone()).is_ok(),
        "wait params accept the host session"
    );
    let without_session = serde_json::json!({ "run_id": "research-1" });
    assert!(
        serde_json::from_value::<ResearchRunParams>(without_session.clone()).is_err(),
        "research_status must not accept a call without the host session"
    );
    assert!(
        serde_json::from_value::<ResearchWaitParams>(without_session.clone()).is_err(),
        "research_wait must not accept a call without the host session"
    );

    let resume = serde_json::json!({ "run_id": "research-1", "prompt": "continue" });
    let mut resume_with_session = resume.clone();
    resume_with_session[HOST_SESSION_FIELD] = session[HOST_SESSION_FIELD].clone();
    serde_json::from_value::<ResearchResumeParams>(resume_with_session)
        .expect("research_resume accepts the host session field");
    assert!(
        serde_json::from_value::<ResearchResumeParams>(resume).is_err(),
        "research_resume must not accept a call without the host session"
    );
}

/// The public start request carries the research user prompt and a bounded
/// runtime budget only. No issue selector, worktree, checkout, repository, or
/// path field exists, so a caller cannot name a location or a bound issue even
/// if a legacy value is sent: the schema is the contract.
#[test]
fn the_start_contract_carries_no_issue_or_worktree_field() {
    let schema = schemars::schema_for!(ResearchStartParams);
    let json = serde_json::to_value(&schema).expect("start schema");
    let properties = json
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("start schema properties");
    let keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    assert_eq!(keys, ["prompt", "session", "timeout_secs"]);
    for forbidden in [
        "issue",
        "worktree",
        "worktree_path",
        "checkout_path",
        "repo_identity",
        "repository",
        "lease_id",
        "cwd",
        "directory",
        "path",
        "run_id",
        "owner",
        "owner_session",
    ] {
        assert!(
            !properties.contains_key(forbidden),
            "research_start must not expose {forbidden}: {keys:?}"
        );
    }
}

/// No served research tool name reintroduces an issue or worktree prerequisite;
/// the surface is the generic research lifecycle only. Tracking tools keep
/// their own names and are out of scope.
#[test]
fn no_research_tool_names_a_location_or_issue() {
    for tool in tool_registry::RESEARCH_TOOLS {
        for needle in ["issue", "worktree", "checkout", "repo", "lease", "owner"] {
            assert!(
                !tool.name.contains(needle),
                "MCP tool {} must not expose {needle}",
                tool.name
            );
        }
    }
}

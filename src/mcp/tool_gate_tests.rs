//! MCP tool-surface tests: the descriptor table, the advertised
//! `capabilities` list, and every handler role gate must agree, and the
//! `comment_create --authorized` gate stays separate from the role gate.
//!
//! Split from `tools.rs` so the handler module stays focused and under the
//! repository's file-size budget; it is compiled as the `tests` child module
//! of `tools`, so it can still reach the private `require_tool` gate.

use super::*;
use rmcp::model::{ErrorCode, NumberOrString};
use rmcp::service::Peer;

const ALL_ROLES: &[Role] = &[
    Role::Admin,
    Role::Orchestrator,
    Role::Executor,
    Role::Reviewer,
    Role::Tester,
];

fn server(role: Role, authorized: bool) -> PhasegentMcpServer {
    PhasegentMcpServer::new(McpConfig::new(
        role, None, None, None, None, None, authorized,
    ))
}

/// The generated rmcp router and the descriptor table must expose exactly
/// the same tool names, so a `#[tool]` added, renamed, or removed without
/// the shared table (or vice versa) fails the suite.
#[test]
fn advertised_tool_names_match_the_served_router() {
    let mut served: Vec<String> = PhasegentMcpServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    served.sort();
    let mut registered: Vec<String> = tool_registry::TOOLS
        .iter()
        .map(|tool| tool.name.to_owned())
        .collect();
    registered.sort();
    assert_eq!(
        served, registered,
        "the #[tool] router and the MCP descriptor table must expose the same names"
    );
}

#[test]
fn allowed_tools_never_expose_excluded_operations() {
    for role in ALL_ROLES {
        let tools = allowed_tools(*role);
        for forbidden in [
            "status_advance",
            "timer",
            "worktree",
            "admin",
            "hooks",
            "plugin",
            "role",
        ] {
            assert!(
                !tools.iter().any(|tool| tool.contains(forbidden)),
                "role {} exposed {forbidden}: {tools:?}",
                role.as_str()
            );
        }
    }
}

/// The advertised allowlist per role, pinned against the descriptor table and
/// its registry-backed gates: the custom `capabilities` payload, `--help mcp`,
/// and the protocol-level `tools/list` all render this set.
const EXPECTED_TOOLS: &[(Role, &[&str])] = &[
    (
        Role::Orchestrator,
        &[
            "capabilities",
            "issue_get",
            "issue_search",
            "status_next",
            "comment_create",
            "notify_send",
        ],
    ),
    (
        Role::Executor,
        &[
            "capabilities",
            "issue_get",
            "status_next",
            "comment_create",
            "notify_send",
        ],
    ),
    (
        Role::Reviewer,
        &[
            "capabilities",
            "issue_get",
            "status_next",
            "comment_create",
            "notify_send",
        ],
    ),
    (
        Role::Tester,
        &["capabilities", "issue_get", "comment_create", "notify_send"],
    ),
    (Role::Admin, &["capabilities", "status_next"]),
];

/// The advertised `capabilities` list is the descriptor table filtered by
/// the shared registry gate, pinned per role.
#[test]
fn allowed_tools_follow_the_descriptor_table_for_every_role() {
    for (role, expected) in EXPECTED_TOOLS {
        assert_eq!(allowed_tools(*role), *expected, "allowed_tools for {role}");
    }
}

/// Every handler gate resolves through the descriptor table plus the shared
/// policy, so a denied role gets the stable operation-named permission
/// error before any provider access.
#[test]
fn handler_gates_match_the_descriptor_table_for_every_role() {
    for role in ALL_ROLES {
        let server = server(*role, false);
        for tool in tool_registry::TOOLS {
            let result = server.require_tool(*tool);
            if tool.allows_role(*role) {
                assert!(result.is_ok(), "{} must be allowed for {role}", tool.name);
                continue;
            }
            let error = result.expect_err("a denied tool must return a permission error");
            let operation = tool.capability().expect("denied tool is gated").operation();
            assert!(
                error.message.contains(operation),
                "{} denial for {role} must name {operation:?}: {}",
                tool.name,
                error.message
            );
            assert!(
                error.message.contains("is not allowed to perform"),
                "{} denial for {role} must use the stable permission wording: {}",
                tool.name,
                error.message
            );
        }
    }
}

/// `notify_send` is gated only by `Capability::Notify`: admin is denied on
/// both the advertised list and the handler gate, every workflow role is
/// allowed.
#[test]
fn notify_send_denial_follows_the_notify_capability() {
    assert!(!Role::Admin.allows(Capability::Notify));
    let error = server(Role::Admin, false)
        .require_tool(tool_registry::NOTIFY_SEND)
        .expect_err("admin notify_send must be denied");
    assert!(
        error.message.contains("notify send"),
        "message={}",
        error.message
    );
    assert!(!allowed_tools(Role::Admin).contains(&"notify_send"));
    for role in [
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
        Role::Tester,
    ] {
        assert!(
            server(role, false)
                .require_tool(tool_registry::NOTIFY_SEND)
                .is_ok(),
            "notify_send must stay allowed for {role}"
        );
        assert!(
            allowed_tools(role).contains(&"notify_send"),
            "notify_send must stay advertised for {role}"
        );
    }
}

/// The role gate and the server-side `--authorized` gate stay separate:
/// an allowed role without `--authorized` is refused before any provider
/// dispatch, and the authorization wording is preserved.
#[test]
fn comment_create_keeps_the_authorized_gate_after_the_role_gate() {
    let error = server(Role::Executor, false)
        .comment_create(Parameters(CommentCreateParams {
            issue: 1,
            body: "body with marker".to_owned(),
            marker: "marker".to_owned(),
        }))
        .expect_err("executor without --authorized must be refused");
    assert!(
        error.message.contains("requires server-side --authorized"),
        "message={}",
        error.message
    );
}

#[test]
fn notify_phase_rejects_setting_and_secret_names() {
    assert!(reject_notify_setting_phase("mcp-phase").is_ok());
    assert!(reject_notify_setting_phase("").is_ok());
    assert!(reject_notify_setting_phase("PHASEGENT_NOTIFY_CHANNEL").is_err());
    assert!(reject_notify_setting_phase("PHASEGENT_NOTIFY_WEBHOOK_TOKEN").is_err());
}

// ---------------------------------------------------------------------------
// Protocol-level `tools/list` / `tools/call` surface (issue 597 P4 repair).
// Every case drives the rmcp `ServerHandler` list/call path through an
// in-memory handshake: no transport, provider, credential, storage, or
// network is touched.
// ---------------------------------------------------------------------------

/// One in-process MCP handshake, returning the negotiated server peer.
async fn protocol_peer(role: Role) -> Peer<RoleServer> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let params =
        serde_json::to_value(InitializeRequestParams::default()).expect("initialize params");
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
    let running = rmcp::serve_server(server(role, false), server_io)
        .await
        .expect("in-process MCP handshake");
    let peer = running.peer().clone();
    let _ = running.cancel().await;
    peer
}

/// A request context that negotiated the modern (SEP-2549 cache-hint) protocol.
fn protocol_context(peer: Peer<RoleServer>) -> RequestContext<RoleServer> {
    let mut context = RequestContext::new(NumberOrString::Number(1), peer);
    context
        .meta
        .set_protocol_version(ProtocolVersion::V_2026_07_28);
    context
}

/// `tools/list` and `get_tool` expose only the startup role's allowlist, and
/// the rmcp 3.4.1 result contract (result type, empty pagination, protocol
/// cache hints) survives the filter.
#[tokio::test]
async fn protocol_tools_list_and_get_follow_the_role_allowlist() {
    for (role, expected) in EXPECTED_TOOLS {
        let handler = server(*role, false);
        let result = handler
            .list_tools(None, protocol_context(protocol_peer(*role).await))
            .await
            .expect("tools/list must succeed");
        let mut listed: Vec<&str> = result.tools.iter().map(|tool| tool.name.as_ref()).collect();
        listed.sort_unstable();
        let mut expected: Vec<&str> = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(listed, expected, "protocol tools/list for {role}");
        for tool in tool_registry::TOOLS {
            assert_eq!(
                handler.get_tool(tool.name).is_some(),
                expected.contains(&tool.name),
                "get_tool {} for {role}",
                tool.name
            );
        }
        assert!(
            handler.get_tool("timer_start").is_none(),
            "unknown tool must not resolve for {role}"
        );
        assert_eq!(
            result.result_type,
            Some(ResultType::COMPLETE),
            "result type {role}"
        );
        assert_eq!(result.next_cursor, None, "pagination {role}");
        assert_eq!(result.ttl_ms, Some(0), "cache ttl {role}");
        assert_eq!(
            result.cache_scope,
            Some(CacheScope::Public),
            "cache scope {role}"
        );
    }
}

/// An unadvertised tool is rejected before the router deserializes arguments
/// or any handler or provider runs. The denied calls pass no arguments on
/// purpose: only the protocol-level gate can produce the stable permission
/// error here, while a name with no route keeps the router's not-found error.
#[tokio::test]
async fn protocol_tools_call_rejects_unadvertised_tools_before_dispatch() {
    for (role, allowed) in EXPECTED_TOOLS {
        for tool in tool_registry::TOOLS {
            if allowed.contains(&tool.name) {
                continue;
            }
            let error = server(*role, false)
                .call_tool(
                    CallToolRequestParams::new(tool.name),
                    protocol_context(protocol_peer(*role).await),
                )
                .await
                .expect_err("an unadvertised tool must be rejected before dispatch");
            let operation = tool.capability().expect("denied tool is gated").operation();
            assert_eq!(
                error.code,
                ErrorCode::INTERNAL_ERROR,
                "{role} {}",
                tool.name
            );
            assert!(
                error.message.contains("is not allowed to perform")
                    && error.message.contains(operation),
                "{} denial for {role}: {}",
                tool.name,
                error.message
            );
        }
    }

    let error = server(Role::Orchestrator, false)
        .call_tool(
            CallToolRequestParams::new("timer_start"),
            protocol_context(protocol_peer(Role::Orchestrator).await),
        )
        .await
        .expect_err("an unknown tool must stay not-found");
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(error.message, "tool not found");
}

/// The role gate admits the tools the role may call: `capabilities` stays
/// reachable over the protocol path and reports the role allowlist, while the
/// separate server-side `--authorized` gate still refuses `comment_create`.
#[tokio::test]
async fn protocol_tools_call_dispatches_allowed_tools_and_keeps_the_authorized_gate() {
    let response = server(Role::Executor, false)
        .call_tool(
            CallToolRequestParams::new("capabilities"),
            protocol_context(protocol_peer(Role::Executor).await),
        )
        .await
        .expect("capabilities must stay callable for every role");
    let CallToolResponse::Complete(result) = response else {
        panic!("capabilities must return a complete tool result");
    };
    let text = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("capabilities text");
    let payload: serde_json::Value = serde_json::from_str(&text.text).expect("capabilities JSON");
    assert_eq!(payload["role"], "executor");
    let tools: Vec<&str> = payload["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.as_str())
        .collect();
    assert_eq!(tools, allowed_tools(Role::Executor));

    let arguments = serde_json::json!({"issue": 1, "body": "body with marker", "marker": "marker"});
    let error = server(Role::Executor, false)
        .call_tool(
            CallToolRequestParams::new("comment_create")
                .with_arguments(arguments.as_object().expect("arguments object").clone()),
            protocol_context(protocol_peer(Role::Executor).await),
        )
        .await
        .expect_err("executor without --authorized must be refused");
    assert!(
        error.message.contains("requires server-side --authorized"),
        "message={}",
        error.message
    );
}

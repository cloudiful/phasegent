//! Verified host identity: the CLI's side of the host broker.
//!
//! The caller identity of a managed session is no longer read from a
//! self-selected environment variable. The host plugin owns the facts
//! (the agent id and the session id it observed in a host tool event) and
//! exposes them through a bounded, loopback-only broker; this module is
//! the client that asks the broker to verify the current invocation.
//!
//! The protocol is versioned newline-delimited JSON over one TCP
//! connection per invocation:
//!
//! ```text
//! -> {"v":1,"op":"identify","token":"<opaque>","session":"<session id>"}
//! <- {"v":1,"ok":true,"role":"executor","session":"<session id>"}
//! <- {"v":1,"ok":false,"error":"unknown token"}
//! ```
//!
//! Two rules make the request safe to send: the request never carries a
//! role (the broker derives the role from its own stored host facts), and
//! the reply is accepted only when the broker echoes the session id the
//! caller presented. When the host also set its own `OPENCODE_SESSION_ID`
//! marker, the reply must equal that value too, so a stale or mismatched
//! broker cannot answer for another managed session. A caller that cannot
//! reach a broker, presents an unknown or expired token, or gets a reply for
//! a different session is denied; there is no fallback to operator authority.
//!
//! Every bound is explicit: the endpoint must be a loopback address, the
//! request and reply are size-capped, and the connect and read phases have
//! their own timeouts, so an unresponsive broker can never hang the CLI.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::policy::{CallerContext, Role};

/// The endpoint of the host broker, as `host:port`. Set by the host
/// plugin for a managed shell; never read from a model-visible command.
pub(crate) const BROKER_ADDR_ENV: &str = "PHASEGENT_BROKER_ADDR";
/// The opaque bearer token of the current managed tool call. It
/// authenticates the request to the broker and carries no role.
pub(crate) const BROKER_TOKEN_ENV: &str = "PHASEGENT_BROKER_TOKEN";

/// The protocol version this client speaks.
const PROTOCOL_VERSION: u32 = 1;
/// The largest request line the client sends.
const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// The largest reply line the client accepts.
const MAX_REPLY_BYTES: usize = 4 * 1024;
/// The connect phase budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// The read phase budget.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The verification request the client sends. It carries the opaque token
/// and the presented session id, and deliberately carries no role: the
/// broker derives the role from the host facts it stored.
#[derive(Debug, Serialize)]
struct IdentifyRequest<'a> {
    v: u32,
    op: &'a str,
    token: &'a str,
    session: &'a str,
}

/// The verification reply the client accepts.
#[derive(Debug, Deserialize)]
struct IdentifyReply {
    v: u32,
    ok: bool,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    session: Option<String>,
}

/// Why a managed context could not be verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Denied {
    /// No broker endpoint or token is present at all.
    Missing,
    /// The broker context is incomplete: an endpoint without a token, or
    /// a token without an endpoint.
    Partial,
    /// The broker could not be reached, or the reply was unusable.
    Unreachable(String),
    /// The broker refused the presented token.
    Rejected(String),
    /// The broker answered for a different session.
    SessionMismatch,
    /// The broker answered with a role this build does not know, or with
    /// a role no host agent can hold.
    UnknownRole(String),
}

impl Denied {
    /// A bounded, non-secret diagnostic. It never echoes the token, the
    /// endpoint, or the reply body.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Missing => "no host broker context is available".to_owned(),
            Self::Partial => "the host broker context is incomplete".to_owned(),
            Self::Unreachable(detail) => {
                format!("the host broker could not be reached: {detail}")
            }
            Self::Rejected(detail) => format!("the host broker refused the request: {detail}"),
            Self::SessionMismatch => "the host broker answered for a different session".to_owned(),
            Self::UnknownRole(role) => {
                format!("the host broker reported an unknown agent role '{role}'")
            }
        }
    }
}

/// The denial of the last resolution, kept so the CLI can explain the
/// fail-closed decision without re-running the broker round trip.
static LAST_DENIAL: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();

fn denial_slot() -> &'static std::sync::Mutex<Option<String>> {
    LAST_DENIAL.get_or_init(|| std::sync::Mutex::new(None))
}

/// The reason the last verification failed, when it failed.
pub(crate) fn last_denial() -> Option<String> {
    denial_slot().lock().ok().and_then(|slot| slot.clone())
}

fn record_denial(denied: &Denied) {
    if let Ok(mut slot) = denial_slot().lock() {
        *slot = Some(denied.reason());
    }
}

/// Whether any broker context is present in the environment. A partial
/// context (an endpoint without a token, or the reverse) still marks the
/// invocation as managed, so it fails closed instead of becoming an
/// ordinary operator CLI.
fn broker_context_present() -> bool {
    std::env::var_os(BROKER_ADDR_ENV).is_some() || std::env::var_os(BROKER_TOKEN_ENV).is_some()
}

/// Whether the invocation carries any managed marker: the session marker
/// (either kind) or the legacy role marker.
fn managed_marker_present() -> bool {
    crate::policy::session_present() || crate::policy::legacy_role_marker_present()
}

/// Resolve the caller context of this invocation from the host broker.
///
/// A managed shell always carries the broker endpoint and token, so the
/// broker is the only source of an agent identity. An unmarked ordinary
/// CLI carries neither and stays an ordinary operator. A managed session
/// without a usable broker context is denied; it never falls back to
/// operator authority.
pub(crate) fn resolve_caller() -> CallerContext {
    match verify() {
        Ok(role) => CallerContext::Agent(role),
        Err(denied) => {
            record_denial(&denied);
            if broker_context_present() || managed_marker_present() {
                // A managed session whose broker context is absent or
                // unusable cannot be verified, so it fails closed instead of
                // becoming operator.
                CallerContext::ManagedWithoutRole
            } else {
                CallerContext::Operator
            }
        }
    }
}

/// Verify the current invocation against the host broker and return the
/// verified role.
pub(crate) fn verify() -> Result<Role, Denied> {
    let endpoint = std::env::var(BROKER_ADDR_ENV).ok().unwrap_or_default();
    let token = std::env::var(BROKER_TOKEN_ENV).ok().unwrap_or_default();
    let session = std::env::var(crate::policy::SESSION_ENV).unwrap_or_default();
    let endpoint = endpoint.trim();
    let token = token.trim();
    if endpoint.is_empty() && token.is_empty() {
        return Err(Denied::Missing);
    }
    if endpoint.is_empty() || token.is_empty() {
        return Err(Denied::Partial);
    }
    // A managed session must present its session id, and the broker echoes
    // it back, so a reply for another session can never be accepted.
    let session = session.trim();
    if session.is_empty() {
        return Err(Denied::Missing);
    }
    let host_session = std::env::var(crate::policy::HOST_SESSION_ENV).ok();
    let host_session = host_session
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    request_role(endpoint, token, session, host_session)
}

/// The agent roles a host broker may report. `admin` is the human
/// bootstrap role and is never an agent, so a broker that claims it is
/// treated as an unknown role.
fn agent_role(value: &str) -> Option<Role> {
    match value {
        "orchestrator" => Some(Role::Orchestrator),
        "executor" => Some(Role::Executor),
        "reviewer" => Some(Role::Reviewer),
        "explore" => Some(Role::Explore),
        _ => None,
    }
}

/// One verification round trip.
fn request_role(
    endpoint: &str,
    token: &str,
    session: &str,
    host_session: Option<&str>,
) -> Result<Role, Denied> {
    let address = loopback_address(endpoint)?;
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|error| Denied::Unreachable(error.to_string()))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| Denied::Unreachable(error.to_string()))?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(|error| Denied::Unreachable(error.to_string()))?;
    let request = IdentifyRequest {
        v: PROTOCOL_VERSION,
        op: "identify",
        token,
        session,
    };
    let mut payload =
        serde_json::to_string(&request).map_err(|error| Denied::Unreachable(error.to_string()))?;
    if payload.len() + 1 > MAX_REQUEST_BYTES {
        return Err(Denied::Unreachable(
            "the request exceeds the size limit".to_owned(),
        ));
    }
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .map_err(|error| Denied::Unreachable(error.to_string()))?;
    stream
        .flush()
        .map_err(|error| Denied::Unreachable(error.to_string()))?;

    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| Denied::Unreachable(error.to_string()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REPLY_BYTES {
            return Err(Denied::Unreachable(
                "the reply exceeds the size limit".to_owned(),
            ));
        }
        if buffer.contains(&b'\n') {
            break;
        }
    }
    let line = buffer
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() {
        return Err(Denied::Unreachable(
            "the broker closed the connection".to_owned(),
        ));
    }
    let reply: IdentifyReply =
        serde_json::from_str(line).map_err(|error| Denied::Unreachable(error.to_string()))?;
    if reply.v != PROTOCOL_VERSION {
        return Err(Denied::Unreachable(format!(
            "the broker speaks protocol version {}",
            reply.v
        )));
    }
    if !reply.ok {
        return Err(Denied::Rejected(
            reply.role.unwrap_or_else(|| "unknown token".to_owned()),
        ));
    }
    let Some(replied_session) = reply.session.as_deref() else {
        return Err(Denied::SessionMismatch);
    };
    if !session_matches(replied_session, session, host_session) {
        return Err(Denied::SessionMismatch);
    }
    let Some(role) = reply.role.as_deref() else {
        return Err(Denied::UnknownRole("none".to_owned()));
    };
    agent_role(role).ok_or_else(|| Denied::UnknownRole(role.to_owned()))
}

/// The reply must echo the presented session and, when the host set its own
/// session marker, match that too. The second check is defense in depth: it
/// catches a stale or mismatched broker without changing the managed path.
fn session_matches(replied: &str, presented: &str, host_session: Option<&str>) -> bool {
    replied == presented
        && match host_session {
            Some(host_session) => replied == host_session,
            None => true,
        }
}

/// Parse the broker endpoint and require a loopback address, so a
/// verification request never leaves the machine.
fn loopback_address(endpoint: &str) -> Result<SocketAddr, Denied> {
    let address: SocketAddr = endpoint
        .parse()
        .map_err(|_| Denied::Unreachable("the broker endpoint is malformed".to_owned()))?;
    if !address.ip().is_loopback() {
        return Err(Denied::Unreachable(
            "the broker endpoint is not a loopback address".to_owned(),
        ));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_loopback_endpoint_is_refused() {
        let error = loopback_address("10.0.0.1:9000").unwrap_err();
        assert!(matches!(error, Denied::Unreachable(_)));
        assert!(loopback_address("127.0.0.1:9000").is_ok());
        assert!(loopback_address("not-an-endpoint").is_err());
    }

    #[test]
    fn the_request_carries_no_role() {
        let request = IdentifyRequest {
            v: PROTOCOL_VERSION,
            op: "identify",
            token: "t",
            session: "s",
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains("role"));
        assert!(encoded.contains("\"token\":\"t\""));
        assert!(encoded.contains("\"session\":\"s\""));
    }

    #[test]
    fn only_agent_roles_are_accepted_from_a_broker() {
        assert_eq!(agent_role("executor"), Some(Role::Executor));
        assert_eq!(agent_role("orchestrator"), Some(Role::Orchestrator));
        assert_eq!(agent_role("reviewer"), Some(Role::Reviewer));
        assert_eq!(agent_role("explore"), Some(Role::Explore));
        // The human bootstrap role is never an agent.
        assert_eq!(agent_role("admin"), None);
        assert_eq!(agent_role("wizard"), None);
    }

    #[test]
    fn an_unknown_agent_role_is_reported_as_unknown() {
        let denied = Denied::UnknownRole("wizard".to_owned());
        assert!(denied.reason().contains("wizard"));
    }

    #[test]
    fn the_reply_must_match_the_host_session_marker() {
        assert!(session_matches("s1", "s1", None));
        assert!(session_matches("s1", "s1", Some("s1")));
        assert!(!session_matches("s2", "s1", None));
        assert!(!session_matches("s1", "s1", Some("s2")));
        assert!(!session_matches("s2", "s1", Some("s1")));
    }

    #[test]
    fn every_denial_has_a_bounded_reason() {
        for denied in [
            Denied::Missing,
            Denied::Partial,
            Denied::Unreachable("refused".to_owned()),
            Denied::Rejected("unknown token".to_owned()),
            Denied::SessionMismatch,
            Denied::UnknownRole("wizard".to_owned()),
        ] {
            let reason = denied.reason();
            assert!(!reason.is_empty());
            assert!(reason.len() < 200, "{reason}");
        }
    }
}

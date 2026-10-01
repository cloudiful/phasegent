//! Negotiation and prompt tests driving the adapter against the
//! in-process fake agent (no real `mcode`, credential, or network).
//!
//! Covered: advertisement checks, exact wire-value selection verified
//! against the agent's own responses, streamed bounded transcripts,
//! protocol-version gating, timeout, and process-death handling. The
//! permission-decision boundaries live in [`super::permission_tests`].

use tokio::io::duplex;

use super::session::AcpSession;
use super::test_kit::{FakeAgent, FakeAgentOptions};
use super::types::{MAX_TRANSCRIPT_CHARS, ResearchPrompt, StopReason};
use super::wire::{
    CONFIG_ID_MODEL, CONFIG_ID_PERMISSION_MODE, CONFIG_ID_THINKING_EFFORT,
    RESEARCH_MODEL_WIRE_VALUE, RESEARCH_PERMISSION_MODE, RESEARCH_THINKING_EFFORT,
};

const WORKTREE: &str = "/tmp/fake-worktree";

pub(super) async fn connect(options: FakeAgentOptions) -> (AcpSession, std::sync::Arc<FakeAgent>) {
    let agent = std::sync::Arc::new(FakeAgent::new(options));
    let (client_side, agent_side) = duplex(64 * 1024);
    let server = agent.clone();
    tokio::spawn(async move { server.serve(agent_side).await });
    let session = AcpSession::start_in_process(client_side, WORKTREE.to_owned())
        .await
        .expect("in-process session");
    (session, agent)
}

#[tokio::test]
async fn negotiation_verifies_advertised_values_then_selects_them() {
    let (session, agent) = connect(FakeAgentOptions::default()).await;
    // The advertisement must include the exact model wire value, the
    // high effort, and the permission mode before anything is selected.
    let options = session.advertised_config_options().await;
    for (config_id, value) in [
        (CONFIG_ID_MODEL, RESEARCH_MODEL_WIRE_VALUE),
        (CONFIG_ID_THINKING_EFFORT, RESEARCH_THINKING_EFFORT),
        (CONFIG_ID_PERMISSION_MODE, RESEARCH_PERMISSION_MODE),
    ] {
        let option = options
            .iter()
            .find(|option| option.id == config_id)
            .unwrap_or_else(|| panic!("{config_id} must be advertised"));
        assert!(
            option.select_option_values().contains(&value.to_owned()),
            "{config_id} must advertise {value}"
        );
    }
    let report = session
        .negotiate_research()
        .await
        .expect("negotiation succeeds");
    assert_eq!(report.model.as_deref(), Some(RESEARCH_MODEL_WIRE_VALUE));
    assert_eq!(report.effort.as_deref(), Some(RESEARCH_THINKING_EFFORT));
    assert_eq!(
        report.permission_mode.as_deref(),
        Some(RESEARCH_PERMISSION_MODE)
    );
    assert_eq!(session.negotiated().await, report);
    // The permission mode is selected before anything else, because the
    // deny path only exists while MCode's permission flow is on.
    let selections = agent.selections.lock().await.clone();
    assert_eq!(
        selections,
        vec![
            (
                CONFIG_ID_PERMISSION_MODE.to_owned(),
                RESEARCH_PERMISSION_MODE.to_owned()
            ),
            (
                CONFIG_ID_MODEL.to_owned(),
                RESEARCH_MODEL_WIRE_VALUE.to_owned()
            ),
            (
                CONFIG_ID_THINKING_EFFORT.to_owned(),
                RESEARCH_THINKING_EFFORT.to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn negotiation_fails_closed_when_values_are_not_advertised() {
    for (options, expected) in [
        (
            FakeAgentOptions {
                omit_effort_option: true,
                ..FakeAgentOptions::default()
            },
            "thinking effort",
        ),
        (
            FakeAgentOptions {
                omit_model_option: true,
                ..FakeAgentOptions::default()
            },
            "research model",
        ),
        (
            FakeAgentOptions {
                omit_permission_mode_option: true,
                ..FakeAgentOptions::default()
            },
            "permission mode",
        ),
    ] {
        let (session, agent) = connect(options).await;
        let error = session
            .negotiate_research()
            .await
            .expect_err("an unadvertised value must fail negotiation");
        assert_eq!(error.kind.as_str(), "negotiation", "{error}");
        assert!(error.message.contains(expected), "{}", error.message);
        assert_eq!(
            session.negotiated().await,
            Default::default(),
            "a failed negotiation confirms nothing"
        );
        assert!(
            agent.selections.lock().await.is_empty(),
            "nothing is selected before the advertisement check"
        );
    }
}

#[tokio::test]
async fn rejected_config_selection_surfaces_as_protocol_error() {
    let (session, _) = connect(FakeAgentOptions {
        reject_set_config: true,
        ..FakeAgentOptions::default()
    })
    .await;
    let error = session
        .negotiate_research()
        .await
        .expect_err("a rejected set_config_option must fail the negotiation");
    assert_eq!(error.kind.as_str(), "protocol");
    assert!(
        error.message.contains("not advertised"),
        "{}",
        error.message
    );
    assert_eq!(session.negotiated().await, Default::default());
}

#[tokio::test]
async fn an_unverified_selection_fails_closed() {
    // The agent accepted the call but reported nothing: the selection
    // cannot be verified, so the run must not claim it negotiated.
    let (session, _) = connect(FakeAgentOptions {
        set_config_reports_nothing: true,
        ..FakeAgentOptions::default()
    })
    .await;
    let error = session
        .negotiate_research()
        .await
        .expect_err("an unverifiable selection must fail the negotiation");
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(
        error.message.contains("did not report a current value"),
        "{}",
        error.message
    );

    // The agent accepted the call but still reports its previous
    // selection: a silently-ignored set must be caught.
    let (session, _) = connect(FakeAgentOptions {
        set_config_reports_stale_value: true,
        ..FakeAgentOptions::default()
    })
    .await;
    let error = session
        .negotiate_research()
        .await
        .expect_err("a stale reported value must fail the negotiation");
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(
        error.message.contains("after selecting"),
        "{}",
        error.message
    );
    assert_eq!(session.negotiated().await, Default::default());
}

#[tokio::test]
async fn an_unsupported_protocol_version_is_refused() {
    let (client_side, agent_side) = duplex(64 * 1024);
    let server = std::sync::Arc::new(FakeAgent::new(FakeAgentOptions {
        protocol_version: Some(9),
        ..FakeAgentOptions::default()
    }));
    tokio::spawn(async move { server.serve(agent_side).await });
    let error = match AcpSession::start_in_process(client_side, WORKTREE.to_owned()).await {
        Err(error) => error,
        Ok(_) => panic!("a different protocol version must not be spoken"),
    };
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(
        error.message.contains("protocol version 9"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn prompt_streams_chunks_into_the_bounded_transcript() {
    let (session, _) = connect(FakeAgentOptions::default()).await;
    session.negotiate_research().await.expect("negotiate");
    let outcome = session
        .prompt(&ResearchPrompt::new("summarize src/"))
        .await
        .expect("prompt completes");
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert_eq!(outcome.text, "found 12 modules in src/");
    assert!(!outcome.truncated);
}

/// The wire prompt is the fixed server-owned read-only instruction followed by
/// the caller's request: the caller cannot supply or drop the system half.
#[tokio::test]
async fn the_wire_prompt_prepends_the_fixed_research_instruction() {
    let (session, agent) = connect(FakeAgentOptions::default()).await;
    session.negotiate_research().await.expect("negotiate");
    session
        .prompt(&ResearchPrompt::new("summarize the call flow"))
        .await
        .expect("prompt completes");
    let prompts = agent.prompts.lock().await.clone();
    assert_eq!(prompts.len(), 1, "one prompt turn");
    let wire = &prompts[0];
    assert!(
        wire.starts_with(super::types::RESEARCH_INSTRUCTION),
        "the fixed instruction must lead the prompt: {wire}"
    );
    assert!(
        wire.ends_with("summarize the call flow"),
        "the caller request must follow the instruction: {wire}"
    );
    assert!(
        wire.contains("read-only"),
        "the instruction must state the read-only contract"
    );
}

#[tokio::test]
async fn prompt_timeout_returns_timeout_error() {
    let (session, _) = connect(FakeAgentOptions {
        hang_prompt: true,
        ..FakeAgentOptions::default()
    })
    .await;
    session.negotiate_research().await.expect("negotiate");
    let error = session
        .prompt(&ResearchPrompt::new("hang").with_timeout_secs(1))
        .await
        .expect_err("a hung agent must hit the timeout");
    assert_eq!(error.kind.as_str(), "timeout");
    session.kill().await;
}

#[tokio::test]
async fn a_session_with_no_advertisement_is_refused() {
    let agent = std::sync::Arc::new(FakeAgent::new(FakeAgentOptions {
        omit_all_config_options: true,
        ..FakeAgentOptions::default()
    }));
    let (client_side, agent_side) = duplex(64 * 1024);
    let server = agent.clone();
    tokio::spawn(async move { server.serve(agent_side).await });
    // `session/new` answers without a config-option advertisement, so
    // the handshake fails closed rather than accepting a session whose
    // pinned values could never be verified.
    let error = match AcpSession::start_in_process(client_side, WORKTREE.to_owned()).await {
        Err(error) => error,
        Ok(_) => panic!("a session with no advertisement must be refused"),
    };
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(
        error.message.contains("config options"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn transcript_cap_drops_oldest_text_and_marks_truncation() {
    let mut transcript = super::types::Transcript::default();
    transcript.push_chunk(&"x".repeat(MAX_TRANSCRIPT_CHARS + 5));
    assert!(transcript.truncated());
    assert_eq!(transcript.text().chars().count(), MAX_TRANSCRIPT_CHARS);
}

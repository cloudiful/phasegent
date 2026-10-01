//! Config-option negotiation, verified against the agent's responses.
//!
//! The pinned values are only half the contract: the issue requires
//! that the resulting *session state* is verified, not that the client
//! remembers what it sent. MCode answers every `session/set_config_option`
//! with the session's re-read `configOptions`, whose `currentValue` is
//! the agent's own report of what it is now running with, and answers
//! `session/load` with a fresh advertisement. So each selection is
//! checked against that response, and an agent that reports nothing is
//! a negotiation failure rather than an unchecked success.
//!
//! Order matters: `permissionMode` first, because the deny path this
//! adapter relies on only exists while MCode's permission flow is
//! enabled; then the model, which `thinkingEffort` requires to be
//! selected first.

use super::super::error::{AgentError, AgentResult};
use super::super::types::NegotiatedReport;
use super::super::wire::{
    self, CONFIG_ID_MODEL, CONFIG_ID_PERMISSION_MODE, CONFIG_ID_THINKING_EFFORT,
    RESEARCH_MODEL_WIRE_VALUE, RESEARCH_PERMISSION_MODE, RESEARCH_THINKING_EFFORT,
    SetConfigOptionResult,
};

use super::AcpSession;

/// The exact wire values this adapter selects, in the order they are
/// applied.
const SELECTION: [(&str, &str); 3] = [
    (CONFIG_ID_PERMISSION_MODE, RESEARCH_PERMISSION_MODE),
    (CONFIG_ID_MODEL, RESEARCH_MODEL_WIRE_VALUE),
    (CONFIG_ID_THINKING_EFFORT, RESEARCH_THINKING_EFFORT),
];

impl AcpSession {
    /// Verify the agent advertised every pinned value, select each
    /// exact wire value, and confirm each selection from the response.
    /// A successful return is the agent-reported session state.
    pub async fn negotiate_research(&self) -> AgentResult<NegotiatedReport> {
        let advertised = self.advertised_config_options().await;
        for (config_id, value) in SELECTION {
            if !advertises(&advertised, config_id, value) {
                return Err(AgentError::negotiation(match config_id {
                    CONFIG_ID_PERMISSION_MODE => {
                        format!("permission mode {value} is not advertised")
                    }
                    CONFIG_ID_THINKING_EFFORT => {
                        format!("thinking effort {value} is not advertised")
                    }
                    _ => format!("research model {value} is not advertised"),
                }));
            }
        }
        for (config_id, value) in SELECTION {
            self.select_and_verify(config_id, value).await?;
        }
        let report = self.negotiated().await;
        if !report.is_complete() {
            return Err(AgentError::negotiation(
                "agent did not confirm every negotiated value",
            ));
        }
        Ok(report)
    }

    /// Send one `set_config_option` and require the response to report
    /// the same value as the session's current selection.
    async fn select_and_verify(&self, config_id: &str, value: &str) -> AgentResult<()> {
        let session_id = self.session_id().await;
        let payload = self
            .request(
                wire::METHOD_SESSION_SET_CONFIG_OPTION,
                serde_json::to_value(wire::SetConfigOptionParams {
                    sessionId: &session_id,
                    configId: config_id,
                    value,
                })
                .expect("serialize set_config_option"),
            )
            .await?;
        let reported: SetConfigOptionResult = serde_json::from_value(payload).map_err(|error| {
            AgentError::protocol(format!("bad set_config_option result: {error}"))
        })?;
        let options = reported.into_options();
        match reported_value(&options, config_id) {
            Some(reported) if reported == value => {}
            Some(reported) => {
                return Err(AgentError::negotiation(format!(
                    "agent reported {reported} for {config_id} after selecting {value}"
                )));
            }
            None => {
                return Err(AgentError::negotiation(format!(
                    "agent did not report a current value for {config_id}"
                )));
            }
        }
        self.record_selection(config_id, value, options);
        Ok(())
    }
}

/// Whether the agent's advertisement offers `value` for `config_id`.
pub(crate) fn advertises(options: &[wire::ConfigOption], config_id: &str, value: &str) -> bool {
    options
        .iter()
        .find(|option| option.id == config_id)
        .is_some_and(|option| {
            option
                .select_option_values()
                .iter()
                .any(|choice| choice == value)
        })
}

fn reported_value<'a>(options: &'a [wire::ConfigOption], config_id: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|option| option.id == config_id)
        .and_then(|option| option.current_value())
}

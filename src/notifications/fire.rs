//! Best-effort delivery boundary for agent notifications.
//!
//! Triggers persist a [`NotificationIntent`] row in
//! `notification_deliveries` before any network work, then deliver
//! through `cloudiful-notifier` on a scoped current-thread runtime.
//! The sync CLI entry point stays sync: a fresh async
//! `reqwest::Client` (separate from the blocking provider clients)
//! drives `Notifier::send` inside `block_on`. Delivery failures never
//! fail the surrounding workflow operation; callers surface the
//! bounded warning on stderr next to their normal JSON output.

use crate::infra::storage::Storage;
use crate::notifications::config::{NotifyChannel, NotifyConfig};
use crate::notifications::envelope::NotificationIntent;

/// Persisted delivery outcome. Manual `notify send` maps this to JSON
/// or a structured error; trigger hooks map failures to a local
/// stderr warning only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireOutcome {
    pub delivered: bool,
    pub channel: &'static str,
    pub row_id: i64,
    pub warning: Option<String>,
}

/// Persist the intent, then deliver when configured. Returns the
/// outcome including the storage row id. Never fails the caller for a
/// delivery error: `delivered=false` plus a bounded `warning` carries
/// the signal. Configuration errors (enabled but misconfigured) are
/// also warnings, never hard errors, so a bad notify setting cannot
/// break issue/status/comment/workflow/timer flows.
pub fn fire_best_effort(storage: &Storage, intent: &NotificationIntent) -> FireOutcome {
    match fire_notification_result(storage, intent) {
        Ok(outcome) => outcome,
        Err(warning) => {
            // Best effort: even persistence failures must not break the
            // workflow op. Record a synthetic row id of 0 so callers
            // can still log the warning.
            FireOutcome {
                delivered: false,
                channel: "none",
                row_id: 0,
                warning: Some(bound_warning(&warning)),
            }
        }
    }
}

/// Strict variant for manual `notify send`: persistence problems are
/// still soft warnings, but misconfiguration and delivery failures
/// are returned as `Err` so the explicit send is observable.
pub fn fire_notification_result(
    storage: &Storage,
    intent: &NotificationIntent,
) -> Result<FireOutcome, String> {
    let config = crate::notifications::config::load(storage)?;
    let Some(config) = config else {
        // Disabled or unconfigured: persist a skipped row so the
        // intent stays observable, then report not-delivered without
        // warning so existing CLI behaviour stays quiet.
        let row_id = record_intent(storage, intent, "none", "skipped", None).unwrap_or(0);
        return Ok(FireOutcome {
            delivered: false,
            channel: "none",
            row_id,
            warning: None,
        });
    };
    let row_id = record_intent(storage, intent, config.channel.as_str(), "pending", None)?;
    match deliver_with_config(&config, intent) {
        Ok(()) => {
            mark_result(storage, row_id, "delivered", None);
            Ok(FireOutcome {
                delivered: true,
                channel: config.channel.as_str(),
                row_id,
                warning: None,
            })
        }
        Err(error) => {
            let warning = bound_warning(&error);
            mark_result(storage, row_id, "failed", Some(&warning));
            Ok(FireOutcome {
                delivered: false,
                channel: config.channel.as_str(),
                row_id,
                warning: Some(warning),
            })
        }
    }
}

fn record_intent(
    storage: &Storage,
    intent: &NotificationIntent,
    channel: &str,
    status: &str,
    error: Option<&str>,
) -> Result<i64, String> {
    storage.record_notification(
        intent.event.as_str(),
        channel,
        &intent.title,
        &intent.body,
        intent.issue_id,
        status,
        error,
    )
}

fn mark_result(storage: &Storage, row_id: i64, status: &str, error: Option<&str>) {
    let _ = storage.update_notification_result(row_id, status, error);
}

/// Deliver one envelope through the configured channel. The async
/// client is built fresh per call so provider blocking clients are
/// never reused across runtimes. `block_on` uses a scoped
/// current-thread runtime; it must not run inside an existing Tokio
/// runtime (the CLI is sync, so this holds).
fn deliver_with_config(config: &NotifyConfig, intent: &NotificationIntent) -> Result<(), String> {
    let envelope = intent.to_message_envelope();
    let async_client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("could not build notification client: {e}"))?;
    let notifier = cloudiful_notifier::Notifier::new(async_client);
    match config.channel {
        NotifyChannel::Ntfy => {
            let channel = cloudiful_notifier::NtfyChannel {
                base_url: config.ntfy_base_url.clone().unwrap_or_default(),
                topic: config.ntfy_topic.clone().unwrap_or_default(),
                auth_token: config.ntfy_token.clone(),
            };
            block_on(notifier.send(&channel, &envelope))
                .map(|_| ())
                .map_err(|e| format!("ntfy delivery failed: {e}"))
        }
        NotifyChannel::Webhook => {
            let channel = cloudiful_notifier::WebhookChannel {
                url: config.webhook_url.clone().unwrap_or_default(),
                bearer_token: config.webhook_token.clone(),
                extra_headers: std::collections::BTreeMap::new(),
            };
            block_on(notifier.send(&channel, &envelope))
                .map(|_| ())
                .map_err(|e| format!("webhook delivery failed: {e}"))
        }
        NotifyChannel::Dingtalk => {
            #[cfg(feature = "notify-dingtalk")]
            {
                let msg_type = match config.dingtalk_msg_type.as_str() {
                    "markdown" => cloudiful_notifier::DingtalkMessageType::Markdown,
                    _ => cloudiful_notifier::DingtalkMessageType::Text,
                };
                let channel = cloudiful_notifier::DingtalkChannel {
                    webhook_url: config.dingtalk_webhook_url.clone().unwrap_or_default(),
                    secret: config.dingtalk_secret.clone(),
                    keywords: config.dingtalk_keywords.clone(),
                    message_type: msg_type,
                };
                block_on(notifier.send(&channel, &envelope))
                    .map(|_| ())
                    .map_err(|e| format!("dingtalk delivery failed: {e}"))
            }
            #[cfg(not(feature = "notify-dingtalk"))]
            {
                Err("channel dingtalk requires the notify-dingtalk Cargo feature".to_owned())
            }
        }
        NotifyChannel::Email => {
            #[cfg(feature = "notify-email")]
            {
                deliver_email(config, &envelope)
            }
            #[cfg(not(feature = "notify-email"))]
            {
                Err("channel email requires the notify-email Cargo feature".to_owned())
            }
        }
    }
}

#[cfg(feature = "notify-email")]
fn deliver_email(
    config: &NotifyConfig,
    intent_envelope: &cloudiful_notifier::MessageEnvelope,
) -> Result<(), String> {
    let tls_mode = match config.email_tls.as_deref().unwrap_or("starttls") {
        "implicit" => cloudiful_notifier::EmailTlsMode::ImplicitTls,
        "plain" => cloudiful_notifier::EmailTlsMode::Plain,
        _ => cloudiful_notifier::EmailTlsMode::StartTls,
    };
    let channel = cloudiful_notifier::EmailChannel {
        smtp_host: config.email_smtp_host.clone().unwrap_or_default(),
        smtp_port: config.email_smtp_port,
        tls_mode,
        username: config.email_username.clone(),
        password: config.email_password.clone(),
        from: config.email_from.clone().unwrap_or_default(),
        to: config.email_to.clone(),
        reply_to: config.email_reply_to.clone(),
    };
    let async_client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("could not build notification client: {e}"))?;
    let notifier = cloudiful_notifier::Notifier::new(async_client);
    block_on(notifier.send(&channel, intent_envelope))
        .map(|_| ())
        .map_err(|e| format!("email delivery failed: {e}"))
}

/// Scoped `block_on` mirroring `issue_index_backend::block_on`: fresh
/// current-thread runtime, `!Send`-safe, panics with a clear message
/// when called from inside a runtime instead of deadlocking.
fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        panic!("notification block_on called from within a Tokio runtime; use async send instead");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("could not build tokio runtime for notifications")
        .block_on(future)
}

/// Bound a delivery/config warning: single line, no control chars,
/// at most 300 chars, never echoes secret values (callers pass only
/// the formatted notifier error, which never contains secrets).
fn bound_warning(raw: &str) -> String {
    let single = raw.replace(['\n', '\r'], " ");
    let cleaned: String = single.chars().filter(|c| !c.is_control()).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    const LIMIT: usize = 300;
    if collapsed.chars().count() <= LIMIT {
        return collapsed;
    }
    let keep = LIMIT.saturating_sub(3);
    let truncated: String = collapsed.chars().take(keep).collect();
    format!("{truncated}...")
}

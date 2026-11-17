use crate::infra::config_file::runtime;
use crate::policy::{CallerContext, Role};
use serde_json::Value;

use super::{ConfigClearOutcome, is_role_scoped_setting};

/// Clear a setting. For global settings the row is deleted;
/// for role-scoped the field is nulled. Returns whether a
/// value was actually removed. An ordinary Operator clears through the
/// admin scope or a human-selected target storage role; a managed session
/// without a role fails closed for role-scoped clears.
pub fn clear_setting(role: Option<Role>, canonical: &str) -> Result<Value, String> {
    let effective = if is_role_scoped_setting(canonical) {
        match CallerContext::from_role(role) {
            CallerContext::Operator => Some(Role::Admin),
            CallerContext::Agent(role) => Some(role),
            CallerContext::ManagedWithoutRole => {
                return Err(crate::cli::managed_denial_message(&format!(
                    "config clear {canonical}"
                )));
            }
        }
    } else {
        role
    };
    let cleared = persist_clear_value(effective, canonical)?;
    let outcome = ConfigClearOutcome {
        setting: canonical.to_owned(),
        role: effective.map(|r| r.as_str().to_owned()),
        cleared,
    };
    serde_json::to_value(outcome).map_err(|e| format!("could not encode clear outcome: {e}"))
}

fn persist_clear_value(role: Option<Role>, canonical: &str) -> Result<bool, String> {
    match canonical {
        "redmine.apiBase"
        | "redmine.repositoryUrl"
        | "redmine.gitMirror.apiKey"
        | "provider"
        | "index.postgres.url"
        | "notifications.enabled"
        | "notifications.channel"
        | "notifications.ntfy.baseUrl"
        | "notifications.ntfy.topic"
        | "notifications.ntfy.token"
        | "notifications.webhook.url"
        | "notifications.webhook.token"
        | "notifications.dingtalk.webhookUrl"
        | "notifications.dingtalk.secret"
        | "notifications.dingtalk.keywords"
        | "notifications.dingtalk.messageType"
        | "notifications.email.smtp.host"
        | "notifications.email.smtp.port"
        | "notifications.email.smtp.tls"
        | "notifications.email.username"
        | "notifications.email.password"
        | "notifications.email.from"
        | "notifications.email.to"
        | "notifications.email.replyTo" => {
            let cleared = runtime::delete_global_setting(canonical)?;
            Ok(cleared)
        }
        "PHASEGENT_PROVIDER" => {
            let role = role.expect("role required");
            let current = runtime::load_role_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.provider.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.provider = None;
                runtime::save_role_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let current = runtime::load_redmine_config(role)?;
            if current.as_ref().and_then(|c| c.close_status_id).is_none() {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.close_status_id = None;
                runtime::save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let current = runtime::load_redmine_config(role)?;
            if current.as_ref().and_then(|c| c.close_status_id).is_none() {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.close_status_id = None;
                runtime::save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        _ => Err(format!("unknown setting '{canonical}'")),
    }
}

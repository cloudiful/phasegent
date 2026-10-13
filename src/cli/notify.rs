use crate::command::NotifyCommand;
use crate::policy::{CallerContext, Capability, Role};

#[allow(dead_code)]
pub(crate) fn execute_notify(role_value: Option<Role>, command: NotifyCommand) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_notify_with_caller(caller, command)
}

pub(crate) fn execute_notify_with_caller(caller: CallerContext, command: NotifyCommand) -> i32 {
    // Managed sessions without a role fail closed before any storage access.
    if caller.is_managed_without_role() {
        return super::managed_denial("notify send");
    }
    // Operator and Agents authorize through the shared capability row;
    // Operator allows every capability, Agents delegate to Role::allows.
    // The credential role is only needed for the notification metadata.
    let credential = match caller.credential_role() {
        Some(role) => role,
        None => return super::managed_denial("notify send"),
    };
    if !caller.allows(Capability::Notify) {
        // Explicit Agent denial keeps the exact legacy shape (admin stays
        // out because it only bootstraps).
        return super::structured_error(
            serde_json::json!({
                "kind": "permission",
                "role": credential.as_str(),
                "operation": "notify send",
                "message": "notify send requires orchestrator, executor, or reviewer"
            }),
            3,
        );
    }
    let role = credential;
    let NotifyCommand::Send {
        event,
        title,
        body,
        issue,
        phase,
    } = command;
    let storage = match super::open_storage() {
        Ok(storage) => storage,
        Err(message) => {
            return super::structured_error(
                serde_json::json!({"kind": "config", "message": message}),
                1,
            );
        }
    };
    let mut intent = crate::notifications::NotificationIntent::new(event, title, body);
    if let Some(issue) = issue {
        intent = intent.with_issue(issue);
    }
    if let Some(phase) = phase {
        intent = intent.with_meta("phase", phase);
    }
    intent = intent.with_meta("role", role.as_str());
    match crate::notifications::fire_notification_result(&storage, &intent) {
        Ok(outcome) => {
            if outcome.delivered {
                super::print_json(&serde_json::json!({
                    "notified": true,
                    "event": intent.event.as_str(),
                    "channel": outcome.channel,
                    "notification_id": outcome.row_id,
                }))
            } else if outcome.channel == "none" {
                // Disabled/unconfigured: persist done, delivery skipped.
                // Surface as JSON so callers can distinguish skipped
                // from delivered without parsing stderr.
                super::print_json(&serde_json::json!({
                    "notified": false,
                    "event": intent.event.as_str(),
                    "channel": "none",
                    "notification_id": outcome.row_id,
                    "skipped": true,
                }))
            } else {
                super::structured_error(
                    serde_json::json!({
                        "kind": "notification",
                        "operation": "notify send",
                        "event": intent.event.as_str(),
                        "channel": outcome.channel,
                        "message": outcome.warning.unwrap_or_else(|| "delivery failed".to_owned()),
                    }),
                    1,
                )
            }
        }
        Err(message) => super::structured_error(
            serde_json::json!({
                "kind": "notification",
                "operation": "notify send",
                "event": intent.event.as_str(),
                "message": message,
            }),
            1,
        ),
    }
}

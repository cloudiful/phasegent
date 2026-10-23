use std::io::Read;

use crate::command::NotifyCommand;
use crate::policy::{CallerContext, Capability, Role};

#[allow(dead_code)]
pub(crate) fn execute_notify(role_value: Option<Role>, command: NotifyCommand) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_notify_with_caller(caller, command)
}

pub(crate) fn execute_notify_with_caller(caller: CallerContext, command: NotifyCommand) -> i32 {
    let mut stdin = std::io::stdin();
    execute_notify_with_reader(caller, command, &mut stdin)
}

/// Fixed, payload-free structured error for rejected stdin question input.
fn notify_input_error(message: &str) -> i32 {
    super::structured_error(
        serde_json::json!({
            "kind": "input",
            "operation": "notify send",
            "message": message,
        }),
        2,
    )
}

/// Testable entry point: caller/capability gates run before `reader` is
/// touched, so a denied role never reads stdin.
fn execute_notify_with_reader(
    caller: CallerContext,
    command: NotifyCommand,
    reader: &mut dyn Read,
) -> i32 {
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
        stdin,
        event,
        title,
        body,
        issue,
        phase,
    } = command;
    let intent = if stdin {
        // Authority is already checked, so reading stdin is safe here. The
        // bounded read and typed parse keep malformed or oversized documents
        // out of storage and delivery.
        let bytes = match crate::notifications::read_question_input(reader) {
            Ok(bytes) => bytes,
            Err(error) => return notify_input_error(error.category()),
        };
        match crate::notifications::parse_question_input(&bytes) {
            Ok(question) => question.to_intent(),
            Err(error) => return notify_input_error(error.category()),
        }
    } else {
        let (Some(event), Some(title)) = (event, title) else {
            return notify_input_error("notification event and title are required");
        };
        let mut intent =
            crate::notifications::NotificationIntent::new(event, title, body.unwrap_or_default());
        if let Some(issue) = issue {
            intent = intent.with_issue(issue);
        }
        if let Some(phase) = phase {
            intent = intent.with_meta("phase", phase);
        }
        intent
    };
    let intent = intent.with_meta("role", role.as_str());
    let storage = match super::open_storage() {
        Ok(storage) => storage,
        Err(message) => {
            return super::structured_error(
                serde_json::json!({"kind": "config", "message": message}),
                1,
            );
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that panics if the CLI touches stdin — used to prove the
    /// capability gate runs first.
    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            panic!("stdin must not be read before the notify capability check");
        }
    }

    fn stdin_command() -> NotifyCommand {
        NotifyCommand::Send {
            stdin: true,
            event: None,
            title: None,
            body: None,
            issue: None,
            phase: None,
        }
    }

    #[test]
    fn denied_role_does_not_read_stdin() {
        let caller = CallerContext::from_role(Some(Role::Admin));
        let mut reader = PanicReader;
        let exit = execute_notify_with_reader(caller, stdin_command(), &mut reader);
        assert_eq!(exit, 3);
    }

    #[test]
    fn oversized_input_is_rejected_with_fixed_category() {
        let caller = CallerContext::from_role(Some(Role::Executor));
        let over = vec![b'a'; crate::notifications::QUESTION_INPUT_LIMIT_BYTES + 1];
        let mut reader = over.as_slice();
        let exit = execute_notify_with_reader(caller, stdin_command(), &mut reader);
        assert_eq!(exit, 2);
    }

    #[test]
    fn invalid_stdin_json_is_rejected_before_storage() {
        let caller = CallerContext::from_role(Some(Role::Executor));
        let mut reader = b"not json".as_slice();
        let exit = execute_notify_with_reader(caller, stdin_command(), &mut reader);
        assert_eq!(exit, 2);
    }
}

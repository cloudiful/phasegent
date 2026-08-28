use crate::command::HooksCommand;

pub(crate) fn execute_hooks(command: HooksCommand) -> i32 {
    match command {
        HooksCommand::Install => match crate::hooks::install() {
            Ok(outcome) => super::print_json(&serde_json::json!({
                "installed": outcome.installed,
                "updated": outcome.updated,
                "warnings": outcome.warnings,
            })),
            Err(error) => super::structured_error(error.json(), 2),
        },
        // Invoked by generated hook scripts; no role, provider, or network.
        HooksCommand::Run {
            hook,
            message_file,
            source,
        } => match crate::hooks::run(hook, &message_file, source.as_deref()) {
            Ok(value) => super::print_json(&value),
            Err(error) => super::structured_error(error.json(), 1),
        },
    }
}

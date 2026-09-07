use crate::policy::Role;

pub(crate) fn print_notify_help(role: Option<Role>) {
    println!(
        "Agent notifications for {}:\n\n  send --event <EVENT> --title <TITLE> [--body <BODY>] [--issue <ID>] [--phase <PHASE>]  Deliver a bounded envelope\n\nEvents: completion, blocked, failure, interruption_suspected, publish_failed.\nTitles/bodies are truncated (140/2000 chars) and the intent is persisted before delivery.\nDisabled or unconfigured channels persist a skipped row and print {{\"notified\": false}} without failing.\nManual-only: no automatic triggers; other commands never send notifications.\n\nUse 'phasegent --help notify send' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_notify_command_help(role: Option<Role>, command: &str) {
    match command {
        "send" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent --role {role_text} notify send --event <EVENT> --title <TITLE> [--body <BODY>] [--issue <ID>] [--phase <PHASE>]\n\nDelivers one bounded notification envelope on the configured channel (ntfy, webhook, dingtalk with notify-dingtalk, email with notify-email).\n\n  --event   completion | blocked | failure | interruption_suspected | publish_failed\n  --title   short summary (required, truncated to 140 chars)\n  --body    bounded detail (optional, truncated to 2000 chars)\n  --issue   optional positive issue id added to body/metadata\n  --phase   optional phase label stored as metadata\n\nConfigure with `config set notify-enabled true`, `config set notify-channel <name>`, plus per-channel fields (ntfy-base-url, ntfy-topic, webhook-url, ...). Secrets (ntfy-token, webhook-token, dingtalk-secret, email-password) require --stdin. No automatic triggers; explicit send only. Manual send returns a structured notification error when delivery fails."
            );
        }
        _ => print_notify_help(role),
    }
}

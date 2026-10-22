use crate::policy::Role;

pub(crate) fn print_notify_help(role: Option<Role>) {
    println!(
        "Agent notifications for {}:\n\n  send --event <EVENT> --title <TITLE> [--body <BODY>] [--issue <ID>] [--phase <PHASE>]  Deliver a bounded envelope\n\nEvents: completion, blocked, failure, interruption_suspected, publish_failed.\nTitles/bodies are truncated (140/2000 chars) and the intent is persisted before delivery.\nDisabled or unconfigured channels persist a skipped row and print {{\"notified\": false}} without failing.\nThe CLI sends only on explicit invocation; the OpenCode adapter automatically invokes it for each new local pending question form, using the current notification settings and subject to ambient caller policy.\n\nUse 'phasegent --help notify send' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_notify_command_help(role: Option<Role>, command: &str) {
    match command {
        "send" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: PHASEGENT_ROLE={role_text} phasegent notify send --event <EVENT> --title <TITLE> [--body <BODY>] [--issue <ID>] [--phase <PHASE>]\n\nDelivers one bounded notification envelope on the configured channel (ntfy, webhook, dingtalk with notify-dingtalk, email with notify-email).\n\n  --event   completion | blocked | failure | interruption_suspected | publish_failed\n  --title   short summary (required, truncated to 140 chars)\n  --body    bounded detail (optional, truncated to 2000 chars)\n  --issue   optional positive issue id added to body/metadata\n  --phase   optional phase label stored as metadata\n\nConfigure with `admin config set notify-enabled true`, `admin config set notify-channel <name>`, plus per-channel fields (ntfy-base-url, ntfy-topic, webhook-url, ...). Email fields are notify-email-smtp-host, notify-email-smtp-port, notify-email-tls, notify-email-username, notify-email-password, notify-email-from, notify-email-to, and notify-email-reply-to. With notify-email-tls unset or auto, port 465 uses implicit TLS and every other port requires STARTTLS; explicit implicit, starttls, and plain override the port. Cloudflare Email Sending uses smtp.mx.cloudflare.net:465 with username api_token and the API token as the password. Secrets (ntfy-token, webhook-token, dingtalk-secret, email-password) use --stdin; email-password also accepts a direct value, which then appears in shell history and process listings. The admin group is human-operator only. The CLI sends only on explicit invocation; the OpenCode adapter automatically invokes it for each new local pending question form, using the current notification settings and subject to ambient caller policy. Manual send returns a structured notification error when delivery fails."
            );
        }
        _ => print_notify_help(role),
    }
}

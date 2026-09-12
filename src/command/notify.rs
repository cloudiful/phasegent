use super::prelude::*;

pub(crate) fn parse_notify(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Notify));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::NotifyCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "send" => {
            validate_options(
                args,
                0,
                &["--event", "--title", "--body", "--issue", "--phase"],
                &[],
                "notify send",
            )?;
            let event_raw = required_nonempty_option(args, "--event", "notify send")?;
            let event = crate::notifications::NotificationEvent::parse(&event_raw)?;
            let title = required_nonempty_option(args, "--title", "notify send")?;
            let body = optional_option(args, "--body").unwrap_or_default();
            let issue = match optional_option(args, "--issue") {
                Some(raw) => {
                    let parsed: u64 = raw
                        .trim()
                        .parse()
                        .map_err(|_| "notify send --issue must be a positive integer".to_owned())?;
                    if parsed == 0 {
                        return Err("notify send --issue must be greater than zero".to_owned());
                    }
                    Some(parsed)
                }
                None => None,
            };
            let phase = optional_option(args, "--phase")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            if let Some(ref phase) = phase {
                if crate::notifications::is_notify_secret(phase) {
                    return Err("notify send --phase must not be a notify secret name".to_owned());
                }
                if crate::notifications::is_notify_setting(phase) {
                    return Err("notify send --phase must not be a notify setting name".to_owned());
                }
            }
            if title.chars().count() > 2000 {
                return Err("notify send --title is too long".to_owned());
            }
            if body.chars().count() > 10000 {
                return Err("notify send --body is too long".to_owned());
            }
            Ok(Command::Notify(NotifyCommand::Send {
                event,
                title,
                body,
                issue,
                phase,
            }))
        }
        value => Err(format!("unknown notify command '{value}'")),
    }
}

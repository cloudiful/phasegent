use super::prelude::*;

pub(crate) fn parse_comment(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Comment));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::CommentCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "create" => {
            validate_options(
                args,
                1,
                &["--body", "--body-file", "--marker"],
                &["--authorized", "--keep-body-file"],
                "comment create",
            )?;
            let (body, body_file, keep_body_file) =
                crate::body_file::parse_body_flags(args, "comment create", true)?;
            Ok(Command::Comment(CommentCommand::Create {
                issue: positional_number(args, 1, "comment create")?,
                body,
                body_file,
                keep_body_file,
                marker: required_nonempty_option(args, "--marker", "comment create")?,
                authorized: has_flag(args, "--authorized"),
            }))
        }
        "get" => {
            require_exact_positionals(args, 3, "comment get")?;
            Ok(Command::Comment(CommentCommand::Get {
                issue: positional_number(args, 1, "comment get")?,
                comment: positional_number(args, 2, "comment get")?,
            }))
        }
        "list" => {
            require_exact_positionals(args, 2, "comment list")?;
            Ok(Command::Comment(CommentCommand::List {
                issue: positional_number(args, 1, "comment list")?,
            }))
        }
        "find-marker" => {
            validate_options(args, 1, &["--marker"], &[], "comment find-marker")?;
            Ok(Command::Comment(CommentCommand::FindMarker {
                issue: positional_number(args, 1, "comment find-marker")?,
                marker: required_nonempty_option(args, "--marker", "comment find-marker")?,
            }))
        }
        value => Err(format!("unknown comment command '{value}'")),
    }
}

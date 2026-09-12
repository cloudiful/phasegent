use super::prelude::*;

pub(crate) fn help_topic(
    value: &str,
    subcommand: Option<&str>,
    nested_subcommand: Option<&str>,
) -> Result<HelpTopic, String> {
    match value {
        "gui" => match subcommand {
            None => Ok(HelpTopic::Gui),
            Some(value) => Err(format!("unknown gui help topic '{value}'")),
        },
        "issue" => match (subcommand, nested_subcommand) {
            (None, _) => Ok(HelpTopic::Issue),
            (Some(value), _)
                if [
                    "get",
                    "search",
                    "create",
                    "update-body",
                    "close",
                    "upload-attachment",
                ]
                .contains(&value) =>
            {
                Ok(HelpTopic::IssueCommand(value.to_owned()))
            }
            (Some(value), _) => Err(format!("unknown issue help topic '{value}'")),
        },
        "comment" => match subcommand {
            None => Ok(HelpTopic::Comment),
            Some(value) if ["create", "get", "list", "find-marker"].contains(&value) => {
                Ok(HelpTopic::CommentCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown comment help topic '{value}'")),
        },
        "project" => match subcommand {
            None => Ok(HelpTopic::Project),
            Some(value) if ["list", "create"].contains(&value) => {
                Ok(HelpTopic::ProjectCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown project help topic '{value}'")),
        },
        "status" => match subcommand {
            None => Ok(HelpTopic::Status),
            Some("list") | Some("next") | Some("set") | Some("advance") => {
                Ok(HelpTopic::StatusCommand(subcommand.unwrap().to_owned()))
            }
            Some(value) => Err(format!("unknown status help topic '{value}'")),
        },
        "version" => match subcommand {
            None => Ok(HelpTopic::Version),
            Some("list") => Ok(HelpTopic::VersionCommand("list".to_owned())),
            Some(value) => Err(format!("unknown version help topic '{value}'")),
        },
        "relation" => match subcommand {
            None => Ok(HelpTopic::Relation),
            Some(value) if ["list", "create", "delete"].contains(&value) => {
                Ok(HelpTopic::RelationCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown relation help topic '{value}'")),
        },
        "timer" => match subcommand {
            None => Ok(HelpTopic::Timer),
            Some(value) if ["start", "finish", "list", "get", "recover"].contains(&value) => {
                Ok(HelpTopic::TimerCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown timer help topic '{value}'")),
        },
        "workflow" => match subcommand {
            None => Ok(HelpTopic::Workflow),
            Some("bootstrap") => Ok(HelpTopic::WorkflowCommand("bootstrap".to_owned())),
            Some(value) => Err(format!("unknown workflow help topic '{value}'")),
        },
        "admin" => match (subcommand, nested_subcommand) {
            (None, _) => Ok(HelpTopic::Admin),
            (Some("auth"), _) => Ok(HelpTopic::Auth),
            (Some("workflow"), _) => Ok(HelpTopic::Workflow),
            (Some("config"), None) => Ok(HelpTopic::Config),
            (Some("config"), Some(value)) if ["show", "set", "clear"].contains(&value) => {
                Ok(HelpTopic::ConfigCommand(value.to_owned()))
            }
            (Some("config"), Some("provider")) => Ok(HelpTopic::ConfigProvider),
            (Some(value), _) => Err(format!("unknown admin help topic '{value}'")),
        },
        "doctor" => match subcommand {
            None => Ok(HelpTopic::Doctor),
            Some(value) => Err(format!("unknown doctor help topic '{value}'")),
        },
        "auth" => Ok(HelpTopic::Auth),
        "config" => match subcommand {
            None => Ok(HelpTopic::Config),
            Some("show") | Some("set") | Some("clear") => {
                Ok(HelpTopic::ConfigCommand(subcommand.unwrap().to_owned()))
            }
            Some("provider") => match nested_subcommand {
                None => Ok(HelpTopic::ConfigProvider),
                Some("get") | Some("set") | Some("clear") => Ok(HelpTopic::ConfigProviderCommand(
                    nested_subcommand.unwrap().to_owned(),
                )),
                Some(value) => Err(format!("unknown config provider help topic '{value}'")),
            },
            Some(value) => Err(format!("unknown config help topic '{value}'")),
        },
        "repo" => match subcommand {
            None => Ok(HelpTopic::Repo),
            Some("create") => Ok(HelpTopic::RepoCommand("create".to_owned())),
            Some(value) => Err(format!("unknown repo help topic '{value}'")),
        },
        "hooks" => match subcommand {
            None => Ok(HelpTopic::Hooks),
            Some("install") => Ok(HelpTopic::HooksCommand("install".to_owned())),
            Some(value) => Err(format!("unknown hooks help topic '{value}'")),
        },
        "plugin" => match subcommand {
            None => Ok(HelpTopic::Plugin),
            Some(value) if ["install", "status", "uninstall"].contains(&value) => {
                Ok(HelpTopic::PluginCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown plugin help topic '{value}'")),
        },
        "notify" => match subcommand {
            None => Ok(HelpTopic::Notify),
            Some("send") => Ok(HelpTopic::NotifyCommand("send".to_owned())),
            Some(value) => Err(format!("unknown notify help topic '{value}'")),
        },
        "mcp" => match subcommand {
            None => Ok(HelpTopic::Mcp),
            Some("serve") => Ok(HelpTopic::McpCommand("serve".to_owned())),
            Some(value) => Err(format!("unknown mcp help topic '{value}'")),
        },
        "worktree" => match subcommand {
            None => Ok(HelpTopic::Worktree),
            Some(value) if ["acquire", "release", "status", "list", "prune"].contains(&value) => {
                Ok(HelpTopic::WorktreeCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown worktree help topic '{value}'")),
        },
        _ => Err(format!("unknown help topic '{value}'")),
    }
}

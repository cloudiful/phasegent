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
                    "update",
                    "close",
                    "sync",
                    "upload-attachment",
                    "bind",
                    "unbind",
                    "status",
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
            Some("list") | Some("next") | Some("set") | Some("advance") | Some("transition") => {
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
            // The admin config group is the human-operator write surface; it
            // gets its own topics so a role-denied request cannot fall back to
            // the read-only top-level `config` page and leak write flags.
            (Some("config"), None) => Ok(HelpTopic::AdminConfig),
            (Some("config"), Some(value)) if ["set", "clear"].contains(&value) => {
                Ok(HelpTopic::AdminConfigCommand(value.to_owned()))
            }
            (Some("config"), Some("provider")) => Ok(HelpTopic::AdminConfigProvider),
            (Some(value), _) => Err(format!("unknown admin help topic '{value}'")),
        },
        "doctor" => match subcommand {
            None => Ok(HelpTopic::Doctor),
            Some(value) => Err(format!("unknown doctor help topic '{value}'")),
        },
        "auth" => Ok(HelpTopic::Auth),
        "config" => match subcommand {
            None => Ok(HelpTopic::Config),
            Some("show") => Ok(HelpTopic::ConfigCommand("show".to_owned())),
            // The top-level `set`/`clear` writes moved under `admin config`;
            // their help pages stay admin-scoped even though the read-only
            // `config` group remains role-open.
            Some("set") | Some("clear") => Ok(HelpTopic::AdminConfigCommand(
                subcommand.unwrap().to_owned(),
            )),
            Some("provider") => match nested_subcommand {
                None => Ok(HelpTopic::ConfigProvider),
                Some("get") => Ok(HelpTopic::ConfigProviderCommand("get".to_owned())),
                Some("set") | Some("clear") => Ok(HelpTopic::AdminConfigProviderCommand(
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
            Some(value)
                if [
                    "acquire",
                    "release",
                    "heartbeat",
                    "status",
                    "list",
                    "probe",
                    "prune",
                ]
                .contains(&value) =>
            {
                Ok(HelpTopic::WorktreeCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown worktree help topic '{value}'")),
        },
        _ => Err(format!("unknown help topic '{value}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The admin write surface gets its own topics so a role-denied request
    /// cannot fall back to the role-open top-level `config` page.
    #[test]
    fn admin_config_help_topics_are_distinct_from_top_level_config() {
        assert!(matches!(
            help_topic("admin", Some("config"), None),
            Ok(HelpTopic::AdminConfig)
        ));
        assert!(matches!(
            help_topic("admin", Some("config"), Some("set")),
            Ok(HelpTopic::AdminConfigCommand(command)) if command == "set"
        ));
        assert!(matches!(
            help_topic("admin", Some("config"), Some("clear")),
            Ok(HelpTopic::AdminConfigCommand(command)) if command == "clear"
        ));
        assert!(matches!(
            help_topic("admin", Some("config"), Some("provider")),
            Ok(HelpTopic::AdminConfigProvider)
        ));

        assert!(matches!(
            help_topic("config", None, None),
            Ok(HelpTopic::Config)
        ));
        assert!(matches!(
            help_topic("config", Some("show"), None),
            Ok(HelpTopic::ConfigCommand(command)) if command == "show"
        ));
        assert!(matches!(
            help_topic("config", Some("provider"), None),
            Ok(HelpTopic::ConfigProvider)
        ));
        assert!(matches!(
            help_topic("config", Some("provider"), Some("get")),
            Ok(HelpTopic::ConfigProviderCommand(command)) if command == "get"
        ));
        // Moved top-level writes resolve to the admin write topics.
        assert!(matches!(
            help_topic("config", Some("set"), None),
            Ok(HelpTopic::AdminConfigCommand(command)) if command == "set"
        ));
        assert!(matches!(
            help_topic("config", Some("provider"), Some("clear")),
            Ok(HelpTopic::AdminConfigProviderCommand(command)) if command == "clear"
        ));
        // `admin config show` is not a valid write command; help rejects it
        // rather than serving a page.
        assert!(help_topic("admin", Some("config"), Some("show")).is_err());
    }
}

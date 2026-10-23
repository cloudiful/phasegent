/// `plugin` subcommands. `path` is the explicit plugin directory named by
/// `--path DIR`: the directory that contains (or will contain)
/// `phasegent-worktree.js`, not the file itself. It is mutually exclusive
/// with the `--global` / `--project` scope selectors; when it is absent the
/// existing scope defaults apply.
#[derive(Debug)]
pub enum PluginCommand {
    Install {
        global: bool,
        project: bool,
        force: bool,
        path: Option<String>,
    },
    Status {
        path: Option<String>,
    },
    Uninstall {
        global: bool,
        project: bool,
        path: Option<String>,
    },
}

/// Agent notification send. `event` is the structured kind
/// (completion, blocked, failure, interruption_suspected,
/// publish_failed); `title`/`body` are bounded at the envelope layer
/// and persisted before delivery. `stdin` selects the standalone
/// structured question document instead of the legacy arguments; the
/// parser guarantees the legacy fields for a non-`stdin` send.
#[derive(Debug)]
pub enum NotifyCommand {
    Send {
        stdin: bool,
        event: Option<crate::notifications::NotificationEvent>,
        title: Option<String>,
        body: Option<String>,
        issue: Option<u64>,
        phase: Option<String>,
    },
}

//! The scoped output context: format resolution, terminal preferences, and the
//! human view selection.
//!
//! The context is presentation only. It never decides permissions, never
//! changes the caller identity, and never touches the provider, storage, or
//! credential layers. A run installs the scope before parsing so parser and
//! permission failures already know the requested format, and the parsed
//! command refines the scope with its own view and legacy format override
//! before dispatch.

use std::cell::RefCell;
use std::io::IsTerminal;

use crate::command::Command;
use crate::policy::{CallerContext, Role};

/// The output format a run produces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode {
    /// Human-readable text.
    Text,
    /// The stable machine JSON document.
    Json,
}

/// The `--output` selection: an explicit choice or the automatic default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputSelection {
    /// Resolve the format from the caller and the terminal state.
    Auto,
    /// Always human-readable text.
    Text,
    /// Always the machine JSON document.
    Json,
}

impl OutputSelection {
    /// Parse an `--output` value. The accepted set is closed, so an unknown
    /// value is a parser error instead of a silent fallback.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err("--output must be 'auto', 'text', or 'json'".to_owned()),
        }
    }

    /// The selection's command-line spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

impl OutputMode {
    /// The mode's command-line spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// The presentation options of one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputOptions {
    /// The global `--output` selection.
    pub selection: OutputSelection,
    /// `--no-progress` disables the progress surface without changing the
    /// warning and error channels.
    pub no_progress: bool,
    /// An explicit legacy per-command format flag (for example
    /// `worktree prune --format json`) that overrides the resolved global
    /// format for that command only.
    pub legacy: Option<OutputMode>,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            selection: OutputSelection::Auto,
            no_progress: false,
            legacy: None,
        }
    }
}

/// The inputs of the format decision. The decision is pure so the matrix can
/// be tested without a terminal, an environment, or a running command.
#[derive(Clone, Copy, Debug)]
pub struct FormatInputs {
    /// The global `--output` selection.
    pub selection: OutputSelection,
    /// The resolved caller context.
    pub caller: CallerContext,
    /// Whether stdout is a terminal.
    pub stdout_is_terminal: bool,
    /// Whether a CI environment is active.
    pub ci_active: bool,
}

/// The format decision.
///
/// An explicit selection always wins. The automatic default keeps every agent
/// role on machine JSON — even on a terminal — and gives the human operator
/// text only on a real terminal outside CI. The stderr stream never
/// influences the stdout format.
pub fn resolve_format(inputs: &FormatInputs) -> OutputMode {
    match inputs.selection {
        OutputSelection::Text => OutputMode::Text,
        OutputSelection::Json => OutputMode::Json,
        OutputSelection::Auto => match inputs.caller {
            // A managed session without a role fails closed and stays
            // machine-readable.
            CallerContext::ManagedWithoutRole => OutputMode::Json,
            // The human operator and the explicit admin role read text only on
            // a real terminal outside CI.
            CallerContext::Operator => human_or_machine(inputs),
            CallerContext::Agent(Role::Admin) => human_or_machine(inputs),
            // Every other agent role reads machine JSON, even on a terminal.
            CallerContext::Agent(_) => OutputMode::Json,
        },
    }
}

fn human_or_machine(inputs: &FormatInputs) -> OutputMode {
    if inputs.stdout_is_terminal && !inputs.ci_active {
        OutputMode::Text
    } else {
        OutputMode::Json
    }
}

/// The human view of a command's result. The selection is exhaustive over
/// [`Command`], so a new command can never reach an emitter without a view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HumanView {
    /// The generic recursive key/value rendering.
    Generic,
    /// The curated `worktree prune` rendering.
    WorktreePrune,
    /// The curated `worktree list` and `worktree status` rendering.
    WorktreeLeaseList,
    /// The curated `worktree probe` rendering.
    WorktreeProbe,
    /// The curated `worktree acquire` rendering.
    WorktreeAcquire,
    /// The curated `issue get` rendering.
    IssueDetail,
    /// The curated `issue search` rendering.
    IssueSearch,
    /// The curated `issue create`, `issue update`, and `issue close` rendering.
    IssueWrite,
    /// The curated `status next` rendering.
    StatusNext,
    /// The curated `status set` and `status advance` rendering.
    StatusTransition,
    /// The curated `status list` rendering.
    StatusList,
    /// The curated `config show` rendering.
    Config,
    /// The curated `doctor` rendering.
    Doctor,
}

/// The resolved presentation state of one run.
#[derive(Clone, Debug)]
pub struct OutputContext {
    /// The resolved output format.
    pub mode: OutputMode,
    /// The human view selected for the command.
    pub view: HumanView,
    /// A short human label of the command.
    pub label: String,
    /// The usable terminal width in columns.
    pub width: usize,
    /// Whether the stdout stream may carry color.
    pub color: bool,
    /// Whether the stderr stream may carry color.
    pub stderr_color: bool,
    /// Whether stdout is a terminal.
    pub stdout_is_terminal: bool,
    /// Whether stderr is a terminal.
    pub stderr_is_terminal: bool,
    /// Whether a CI environment is active.
    pub ci_active: bool,
    /// Whether the terminal supports cursor control.
    pub term_capable: bool,
    /// Whether the caller is a human operator (an agent role never gets a
    /// human affordance).
    pub human_caller: bool,
    /// Whether the progress surface is allowed.
    #[allow(dead_code)] // read by the progress surface
    pub progress: bool,
    /// An explicit legacy format that overrides `mode` for this command.
    pub legacy: Option<OutputMode>,
}

impl OutputContext {
    /// The effective format: an explicit legacy format wins over the resolved
    /// global format.
    pub fn effective_mode(&self) -> OutputMode {
        self.legacy.unwrap_or(self.mode)
    }
}

/// The safe default used when no scope is installed: machine JSON, no color,
/// no progress, and a fixed width.
fn default_context() -> OutputContext {
    OutputContext {
        mode: OutputMode::Json,
        view: HumanView::Generic,
        label: String::new(),
        width: 80,
        color: false,
        stderr_color: false,
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        ci_active: false,
        term_capable: false,
        human_caller: false,
        progress: false,
        legacy: None,
    }
}

thread_local! {
    static SCOPES: RefCell<Vec<OutputContext>> = const { RefCell::new(Vec::new()) };
}

/// The RAII guard of one scoped run. Dropping it restores the previous scope,
/// so nested runs never leak presentation state into each other.
pub struct OutputScope {
    active: bool,
}

impl Drop for OutputScope {
    fn drop(&mut self) {
        if self.active {
            SCOPES.with(|scopes| {
                scopes.borrow_mut().pop();
            });
        }
    }
}

/// Install the scoped presentation state for one run.
pub fn install(options: &OutputOptions, caller: Option<CallerContext>) -> OutputScope {
    let context = resolve_context(options, caller);
    SCOPES.with(|scopes| {
        scopes.borrow_mut().push(context);
    });
    OutputScope { active: true }
}

/// Refresh the installed scope with the parsed options and the authoritative
/// caller. The parser resolves the caller once; this keeps the presentation
/// state in step with it instead of re-deciding anything.
pub fn apply(options: &OutputOptions, caller: CallerContext) {
    let context = resolve_context(options, Some(caller));
    update(|scope| *scope = context);
}

/// Update the installed scope in place. A missing scope is a no-op, so the
/// safe default stays in force.
pub fn update(update: impl FnOnce(&mut OutputContext)) {
    SCOPES.with(|scopes| {
        if let Some(scope) = scopes.borrow_mut().last_mut() {
            update(scope);
        }
    });
}

/// The installed scope, or `None` when no run is active.
pub fn current() -> Option<OutputContext> {
    SCOPES.with(|scopes| scopes.borrow().last().cloned())
}

/// The installed scope, or the safe default when no run is active.
pub fn current_or_default() -> OutputContext {
    current().unwrap_or_else(default_context)
}

/// The effective format of a command that carries a legacy format selection.
///
/// A scoped run already carries the resolved global format and the explicit
/// legacy override, so the scope decides. Without a scope the explicit
/// parameter is the only signal.
pub fn effective_mode(legacy: &str) -> OutputMode {
    match current() {
        Some(context) => context.effective_mode(),
        None => match legacy {
            "json" => OutputMode::Json,
            _ => OutputMode::Text,
        },
    }
}

fn resolve_context(options: &OutputOptions, caller: Option<CallerContext>) -> OutputContext {
    let mode = resolve_format(&FormatInputs {
        selection: options.selection,
        // A caller that could not be resolved stays machine-readable.
        caller: caller.unwrap_or(CallerContext::ManagedWithoutRole),
        stdout_is_terminal: stdout_is_terminal(),
        ci_active: ci_active(),
    });
    let stdout_is_terminal = stdout_is_terminal();
    let stderr_is_terminal = stderr_is_terminal();
    OutputContext {
        mode,
        view: HumanView::Generic,
        label: String::new(),
        width: terminal_width(),
        color: color_enabled(stdout_is_terminal),
        stderr_color: color_enabled(stderr_is_terminal),
        stdout_is_terminal,
        stderr_is_terminal,
        ci_active: ci_active(),
        term_capable: term_capable(),
        human_caller: caller.is_some_and(is_human_caller),
        progress: !options.no_progress,
        legacy: options.legacy,
    }
}

/// Whether the caller is a human operator. The human operator and the
/// explicit admin role are the only callers that get the human affordances.
fn is_human_caller(caller: CallerContext) -> bool {
    matches!(
        caller,
        CallerContext::Operator | CallerContext::Agent(Role::Admin)
    )
}

/// Whether stdout is a terminal. The stderr stream never decides the stdout
/// format.
fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether stderr is a terminal.
fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

/// Whether the terminal supports cursor control. A `dumb` terminal never gets
/// an animated surface.
fn term_capable() -> bool {
    !std::env::var("TERM").is_ok_and(|value| value.trim().eq_ignore_ascii_case("dumb"))
}

/// Whether a CI environment is active. Both conventional markers count, and a
/// disabled value (`0` or `false`) does not activate CI.
fn ci_active() -> bool {
    ["CI", "CONTINUOUS_INTEGRATION"].iter().any(|name| {
        std::env::var(name).is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty()
                && !value.eq_ignore_ascii_case("0")
                && !value.eq_ignore_ascii_case("false")
        })
    })
}

/// Whether a stream may carry color: a terminal, no `NO_COLOR` policy, and a
/// terminal that is not `dumb`. Each stream decides for itself, so a piped
/// stdout never colors the diagnostics and the other way around.
fn color_enabled(is_terminal: bool) -> bool {
    if !is_terminal {
        return false;
    }
    if std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
        return false;
    }
    !std::env::var("TERM").is_ok_and(|value| value.trim().eq_ignore_ascii_case("dumb"))
}

/// The usable terminal width, clamped to a readable range.
fn terminal_width() -> usize {
    let width = console::Term::stdout()
        .size_checked()
        .map(|(_, width)| usize::from(width))
        .unwrap_or(80);
    width.clamp(20, 200)
}

/// The human view of a parsed command. The match is exhaustive so every
/// command has a view.
pub fn view_for(command: &Command) -> HumanView {
    use crate::command::IssueCommand;
    use crate::command::StatusCommand;
    use crate::command::WorktreeCommand;
    match command {
        Command::Worktree(WorktreeCommand::Prune { .. }) => HumanView::WorktreePrune,
        Command::Worktree(WorktreeCommand::List { .. })
        | Command::Worktree(WorktreeCommand::Status { .. }) => HumanView::WorktreeLeaseList,
        Command::Worktree(WorktreeCommand::Probe { .. }) => HumanView::WorktreeProbe,
        Command::Worktree(WorktreeCommand::Acquire { .. }) => HumanView::WorktreeAcquire,
        Command::Issue(IssueCommand::Get { .. }) => HumanView::IssueDetail,
        Command::Issue(IssueCommand::Search { .. }) => HumanView::IssueSearch,
        Command::Issue(
            IssueCommand::Create { .. } | IssueCommand::Update { .. } | IssueCommand::Close { .. },
        ) => HumanView::IssueWrite,
        Command::Status(StatusCommand::Next { .. }) => HumanView::StatusNext,
        Command::Status(StatusCommand::Set { .. } | StatusCommand::Advance { .. }) => {
            HumanView::StatusTransition
        }
        Command::Status(StatusCommand::List) => HumanView::StatusList,
        Command::ConfigShow => HumanView::Config,
        Command::Doctor => HumanView::Doctor,
        Command::Help(_)
        | Command::Version
        | Command::AuthSetup { .. }
        | Command::ConfigSet { .. }
        | Command::ConfigClear { .. }
        | Command::ConfigProviderGet
        | Command::ConfigProviderSet { .. }
        | Command::ConfigProviderClear
        | Command::Issue(_)
        | Command::Comment(_)
        | Command::Record(_)
        | Command::Project(_)
        | Command::VersionCommand(_)
        | Command::Workflow(_)
        | Command::Hooks(_)
        | Command::Plugin(_)
        | Command::Relation(_)
        | Command::Timer(_)
        | Command::Worktree(_)
        | Command::Notify(_) => HumanView::Generic,
    }
}

/// A short human label of a parsed command.
pub fn label_for(command: &Command) -> &'static str {
    use crate::command::IssueCommand;
    use crate::command::StatusCommand;
    use crate::command::WorktreeCommand;
    match command {
        Command::Issue(IssueCommand::Get { .. })
        | Command::Issue(IssueCommand::GetBatch { .. }) => "issue get",
        Command::Issue(IssueCommand::Search { .. }) => "issue search",
        Command::Issue(IssueCommand::Create { .. }) => "issue create",
        Command::Issue(IssueCommand::Update { .. }) => "issue update",
        Command::Issue(IssueCommand::Close { .. }) => "issue close",
        Command::Issue(IssueCommand::UploadAttachment { .. }) => "issue upload-attachment",
        Command::Issue(IssueCommand::Sync { .. }) => "issue sync",
        Command::Issue(IssueCommand::Bind { .. }) => "issue bind",
        Command::Issue(IssueCommand::Unbind { .. }) => "issue unbind",
        Command::Issue(IssueCommand::StatusBranch) => "issue status branch",
        Command::Issue(IssueCommand::Branches { .. }) => "issue branches",
        Command::Comment(_) => "comment",
        Command::Record(_) => "record",
        Command::Project(_) => "project",
        Command::Status(StatusCommand::List) => "status list",
        Command::Status(StatusCommand::Next { .. }) => "status next",
        Command::Status(StatusCommand::Set { .. }) => "status set",
        Command::Status(StatusCommand::Advance { .. }) => "status advance",
        Command::VersionCommand(_) => "version list",
        Command::Workflow(_) => "workflow",
        Command::Hooks(_) => "hooks",
        Command::Plugin(_) => "plugin",
        Command::Relation(_) => "relation",
        Command::Timer(_) => "timer",
        Command::Worktree(WorktreeCommand::Acquire { .. }) => "worktree acquire",
        Command::Worktree(WorktreeCommand::Release { .. }) => "worktree release",
        Command::Worktree(WorktreeCommand::Status { .. }) => "worktree status",
        Command::Worktree(WorktreeCommand::List { .. }) => "worktree list",
        Command::Worktree(WorktreeCommand::Probe { .. }) => "worktree probe",
        Command::Worktree(WorktreeCommand::Prune { .. }) => "worktree prune",
        Command::Worktree(WorktreeCommand::Heartbeat { .. }) => "worktree heartbeat",
        Command::Help(_) => "help",
        Command::Version => "version",
        Command::AuthSetup { .. } => "admin auth setup",
        Command::ConfigShow => "config show",
        Command::ConfigSet { .. } => "config set",
        Command::ConfigClear { .. } => "config clear",
        Command::ConfigProviderGet => "config provider get",
        Command::ConfigProviderSet { .. } => "config provider set",
        Command::ConfigProviderClear => "config provider clear",
        Command::Doctor => "doctor",
        Command::Notify(_) => "notify",
    }
}

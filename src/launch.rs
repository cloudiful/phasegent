//! No-argument terminal-vs-desktop heuristic for the single binary.
//!
//! The binary has two explicit entry points:
//!
//! - `phasegent gui` opens the Tauri desktop shell (see [`crate::gui`]).
//! - Every other CLI invocation runs through [`crate::cli::run`] and
//!   never initializes the GUI.
//!
//! A bare launch with no arguments is ambiguous: a terminal user who
//! types `phasegent` expects CLI help, while an Explorer/Finder-style
//! double-click has no console and should open the GUI where that
//! context is detectable. The heuristic below keeps the terminal case
//! authoritative and stays conservative everywhere else:
//!
//! 1. When any of stdin/stdout/stderr is attached to a terminal, the
//!    launch is treated as a terminal invocation and shows CLI help.
//! 2. When none of stdio is a terminal, the launch *may* be a desktop
//!    double-click, but piped CI/test harnesses look identical. To
//!    avoid hijacking those, the GUI opens only when a desktop session
//!    indicator is also present (`DISPLAY`/`WAYLAND_DISPLAY`/
//!    `XDG_CURRENT_DESKTOP`/`DESKTOP_SESSION` on Linux,
//!    `SECURITYSESSIONID`/`__CFBundleIdentifier`/
//!    `Apple_PubSub_Socket_Render` on macOS, `SESSIONNAME` on Windows).
//! 3. When detection is uncertain, the decision stays on CLI help.
//!
//! `cli::run` itself is unchanged: `cli::run([])` still renders root
//! help. Only `main` consults this heuristic before dispatching, so
//! existing JSON/error contracts are preserved.

use std::io::IsTerminal;

/// Snapshot of the signals used by [`should_open_gui_on_no_args`].
/// The struct is intentionally plain data so unit tests can cover the
/// decision table without touching the real terminal or environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoArgContext {
    /// True when stdin is attached to a terminal.
    pub stdin_is_tty: bool,
    /// True when stdout is attached to a terminal.
    pub stdout_is_tty: bool,
    /// True when stderr is attached to a terminal.
    pub stderr_is_tty: bool,
    /// True when a desktop session indicator was observed in the
    /// environment (see [`has_desktop_indicator`]).
    pub desktop_indicator_present: bool,
}

/// Conservative decision for a no-argument launch.
///
/// Returns `true` only when no stdio handle is a terminal *and* a
/// desktop indicator is present. Any terminal attachment means CLI
/// help; a headless/piped environment without a desktop indicator
/// (CI, tests, scripts) also stays on CLI help.
pub fn should_open_gui_on_no_args(context: &NoArgContext) -> bool {
    if context.stdin_is_tty || context.stdout_is_tty || context.stderr_is_tty {
        return false;
    }
    context.desktop_indicator_present
}

/// Observe the current process for a desktop session indicator.
///
/// The check is deliberately narrow: it looks only for well-known
/// session variables and never inspects secrets or credentials. An
/// empty value counts as absent.
pub fn has_desktop_indicator() -> bool {
    const KEYS: &[&str] = &[
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "DESKTOP_SESSION",
        "SECURITYSESSIONID",
        "__CFBundleIdentifier",
        "Apple_PubSub_Socket_Render",
        "SESSIONNAME",
    ];
    KEYS.iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Capture the live no-argument context from the real stdio handles
/// and environment. Used only by `main`; tests construct
/// [`NoArgContext`] directly.
pub fn current_no_arg_context() -> NoArgContext {
    NoArgContext {
        stdin_is_tty: std::io::stdin().is_terminal(),
        stdout_is_tty: std::io::stdout().is_terminal(),
        stderr_is_tty: std::io::stderr().is_terminal(),
        desktop_indicator_present: has_desktop_indicator(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_context() -> NoArgContext {
        NoArgContext {
            stdin_is_tty: true,
            stdout_is_tty: true,
            stderr_is_tty: true,
            desktop_indicator_present: true,
        }
    }

    fn desktop_context() -> NoArgContext {
        NoArgContext {
            stdin_is_tty: false,
            stdout_is_tty: false,
            stderr_is_tty: false,
            desktop_indicator_present: true,
        }
    }

    fn piped_context() -> NoArgContext {
        NoArgContext {
            stdin_is_tty: false,
            stdout_is_tty: false,
            stderr_is_tty: false,
            desktop_indicator_present: false,
        }
    }

    #[test]
    fn terminal_launch_stays_on_cli_help_even_with_desktop_env() {
        assert!(!should_open_gui_on_no_args(&terminal_context()));
        for context in [
            NoArgContext {
                stdin_is_tty: true,
                ..desktop_context()
            },
            NoArgContext {
                stdout_is_tty: true,
                ..desktop_context()
            },
            NoArgContext {
                stderr_is_tty: true,
                ..desktop_context()
            },
        ] {
            assert!(
                !should_open_gui_on_no_args(&context),
                "any TTY must stay on CLI help: {context:?}"
            );
        }
    }

    #[test]
    fn desktop_double_click_opens_gui_where_detectable() {
        assert!(should_open_gui_on_no_args(&desktop_context()));
    }

    #[test]
    fn piped_ci_without_desktop_indicator_stays_on_cli_help() {
        // Piped test harnesses and CI have no TTY and no desktop env;
        // the conservative default must remain CLI help so existing
        // `cli::run([])` help contracts are never hijacked.
        assert!(!should_open_gui_on_no_args(&piped_context()));
    }

    #[test]
    fn explicit_gui_parses_without_role() {
        let invocation =
            crate::command::parse(&["gui".to_owned()]).expect("gui must parse without --role");
        assert!(invocation.role.is_none());
        assert!(matches!(invocation.command, crate::command::Command::Gui));
    }

    #[test]
    fn gui_rejects_surplus_arguments() {
        let error = crate::command::parse(&["gui".to_owned(), "extra".to_owned()])
            .expect_err("gui must reject surplus arguments");
        assert!(
            error.contains("gui takes no arguments"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn existing_cli_parse_contracts_are_unchanged() {
        // Empty args still map to root help through the shared parser;
        // the desktop heuristic lives in `main`, not in `cli::run`.
        let invocation = crate::command::parse(&[]).expect("empty args must parse");
        assert!(matches!(
            invocation.command,
            crate::command::Command::Help(crate::command::HelpTopic::Root)
        ));
        // A representative existing command still requires --role.
        let error = crate::command::parse(&["issue".to_owned(), "get".to_owned(), "1".to_owned()])
            .expect_err("issue get without --role must still fail");
        assert!(error.contains("--role"), "unexpected error: {error}");
    }
}

//! Deterministic launch semantics for the single binary.
//!
//! The Rust binary has two entry points:
//!
//! - [`crate::desktop_bridge`] — the hidden stdio mode the packaged Electron
//!   application spawns as its companion backend.
//! - Every other invocation runs through [`crate::cli::run`], which never
//!   initializes a desktop shell.
//!
//! A bare launch is unambiguous now that the desktop shell is a separate
//! Electron application: it always renders the root CLI help, identical in a
//! terminal, a piped script, and a packaged companion, instead of switching on
//! terminal or desktop-session detection. `cli::run([])` owns that output, so
//! this module only routes the arguments and documents the contract.

/// Run a CLI invocation and return its exit code.
///
/// A bare launch resolves to the root CLI help through
/// [`crate::cli::run`], which keeps the existing JSON/error contracts for
/// every explicit command.
pub fn run_cli(args: Vec<String>) -> i32 {
    crate::cli::run(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare launch is CLI space and succeeds with the root help: no context a
    /// packaged companion or script can present diverts it to a window.
    #[test]
    fn no_arguments_resolves_to_root_help() {
        let invocation = crate::command::parse(&[]).expect("empty args must parse");
        assert!(matches!(
            invocation.command,
            crate::command::Command::Help(crate::command::HelpTopic::Root)
        ));
        assert_eq!(run_cli(Vec::new()), 0);
    }

    /// No argument can open a Rust desktop shell: the shell is the Electron
    /// application, so the parser treats those names as unknown commands.
    #[test]
    fn no_argument_opens_a_rust_gui() {
        for name in ["gui", "desktop"] {
            let error =
                crate::command::parse(&[name.to_owned()]).expect_err("no Rust desktop entry");
            assert_eq!(error, format!("unknown command '{name}'"));
        }
        assert_eq!(run_cli(vec!["gui".to_owned()]), 2);
    }

    /// Explicit commands keep the role requirement; removing the desktop entry
    /// did not change the CLI contract.
    #[test]
    fn explicit_commands_keep_the_role_requirement() {
        let error = crate::command::parse_with_role_env(
            &["issue".to_owned(), "get".to_owned(), "1".to_owned()],
            None,
        )
        .expect_err("issue get without a role must still fail");
        assert!(
            error.contains("a role is required"),
            "unexpected error: {error}"
        );
    }
}

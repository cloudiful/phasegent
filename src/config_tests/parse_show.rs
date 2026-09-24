use super::*;

#[test]
fn config_show_command_parses_without_role() {
    let args = ["config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config show without a role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_show_command_parses_with_role() {
    let args = ["config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("executor"))
        .expect("config show with a role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_unknown_and_removed_subcommands_are_rejected() {
    // `config import-env` was removed; like any unknown subcommand it must be
    // rejected as an unknown config command, with and without a role.
    for literal in ["purge", "import-env"] {
        for with_role in [true, false] {
            let args = vec!["config".to_owned(), literal.to_owned()];
            let role = with_role.then_some("executor");
            let error = command::parse_with_role_env(&args, role)
                .expect_err("unknown config command must error");
            assert!(
                error.contains("unknown config command") && error.contains(literal),
                "got: {error}"
            );
        }
    }
}

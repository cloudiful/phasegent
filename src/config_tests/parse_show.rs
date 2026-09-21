use super::*;

#[test]
fn config_show_command_parses_without_role() {
    let args = ["config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config show without --role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_show_command_parses_with_role() {
    let args = ["--role", "executor", "config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config show with --role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_unknown_and_removed_subcommands_are_rejected() {
    // `config import-env` was removed; like any unknown subcommand it must be
    // rejected as an unknown config command, with and without --role.
    for literal in ["purge", "import-env"] {
        for with_role in [true, false] {
            let mut args = Vec::new();
            if with_role {
                args.push("--role".to_owned());
                args.push("admin".to_owned());
            }
            args.push("config".to_owned());
            args.push(literal.to_owned());
            let error = command::parse(&args).expect_err("unknown config command must error");
            assert!(
                error.contains("unknown config command") && error.contains(literal),
                "got: {error}"
            );
        }
    }
}

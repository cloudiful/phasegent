use super::*;

#[test]
fn native_hook_argument_table() {
    for (args, valid) in [
        (vec!["run", "prepare-commit-msg", "message"], true),
        (
            vec!["run", "prepare-commit-msg", "message", "message"],
            true,
        ),
        (
            vec!["run", "prepare-commit-msg", "message", "commit", "opaque"],
            true,
        ),
        (vec!["run", "prepare-commit-msg"], false),
        (
            vec!["run", "prepare-commit-msg", "message", "template", "opaque"],
            false,
        ),
        (
            vec![
                "run",
                "prepare-commit-msg",
                "message",
                "commit",
                "opaque",
                "extra",
            ],
            false,
        ),
        (vec!["run", "prepare-commit-msg", "-message"], false),
        (vec!["run", "commit-msg", "message"], true),
        (vec!["run", "commit-msg", "message", "extra"], false),
    ] {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_hooks_run(&owned).is_ok(), valid, "{args:?}");
        if valid {
            let Command::Hooks(HooksCommand::Run { source, .. }) = parse_hooks_run(&owned).unwrap()
            else {
                panic!("not a hook");
            };
            assert_eq!(source.as_deref(), args.get(3).copied());
        }
    }
}

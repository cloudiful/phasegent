//! Origin-aware help routing for the `config` group (issue 597 Phase 2).
//!
//! The same argv grammar serves the read-only top-level `config` group and the
//! human-operator `admin config` write group. These tests pin that a
//! command-form `--help` resolves to the topic of the surface that supplied it,
//! so an AI role can never reach an admin write page through the parser.

use super::super::{Command, HelpTopic};
use super::{parse_config, parse_config_admin};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn expect_help(parsed: Command) -> HelpTopic {
    match parsed {
        Command::Help(topic) => topic,
        other => panic!("expected Help, got {other:?}"),
    }
}

#[test]
fn top_level_config_read_only_forms_keep_read_only_topics() {
    assert!(matches!(
        expect_help(parse_config(&strings(&["--help"])).unwrap()),
        HelpTopic::Config
    ));
    assert!(matches!(
        expect_help(parse_config(&strings(&["show", "--help"])).unwrap()),
        HelpTopic::ConfigCommand(command) if command == "show"
    ));
    assert!(matches!(
        expect_help(parse_config(&strings(&["provider", "--help"])).unwrap()),
        HelpTopic::ConfigProvider
    ));
    assert!(matches!(
        expect_help(parse_config(&strings(&["provider", "get", "--help"])).unwrap()),
        HelpTopic::ConfigProviderCommand(command) if command == "get"
    ));
}

#[test]
fn top_level_config_write_forms_resolve_to_admin_topics() {
    for command in ["set", "clear"] {
        let topic = expect_help(parse_config(&strings(&[command, "--help"])).unwrap());
        assert!(
            matches!(&topic, HelpTopic::AdminConfigCommand(value) if value == command),
            "{command}: got {topic:?}"
        );
    }
    for command in ["set", "clear"] {
        let topic = expect_help(parse_config(&strings(&["provider", command, "--help"])).unwrap());
        assert!(
            matches!(&topic, HelpTopic::AdminConfigProviderCommand(value) if value == command),
            "provider {command}: got {topic:?}"
        );
    }
}

#[test]
fn admin_config_command_forms_resolve_to_admin_topics() {
    assert!(matches!(
        expect_help(parse_config_admin(&strings(&["--help"])).unwrap()),
        HelpTopic::AdminConfig
    ));
    for command in ["set", "clear"] {
        let topic = expect_help(parse_config_admin(&strings(&[command, "--help"])).unwrap());
        assert!(
            matches!(&topic, HelpTopic::AdminConfigCommand(value) if value == command),
            "admin config {command}: got {topic:?}"
        );
    }
    assert!(matches!(
        expect_help(parse_config_admin(&strings(&["provider", "--help"])).unwrap()),
        HelpTopic::AdminConfigProvider
    ));
    for command in ["set", "clear"] {
        let topic =
            expect_help(parse_config_admin(&strings(&["provider", command, "--help"])).unwrap());
        assert!(
            matches!(&topic, HelpTopic::AdminConfigProviderCommand(value) if value == command),
            "admin config provider {command}: got {topic:?}"
        );
    }
}

#[test]
fn non_help_invocations_keep_their_parse_and_error_behavior() {
    assert!(matches!(
        parse_config(&strings(&["show"])).unwrap(),
        Command::ConfigShow
    ));
    assert!(matches!(
        parse_config(&strings(&["provider", "set", "redmine"])).unwrap(),
        Command::ConfigProviderSet { .. }
    ));
    assert!(matches!(
        parse_config_admin(&strings(&["set", "api-base", "https://example.com"])).unwrap(),
        Command::ConfigSet { .. }
    ));
    assert!(matches!(
        parse_config_admin(&strings(&["clear", "api-base"])).unwrap(),
        Command::ConfigClear { .. }
    ));
    // The admin group still rejects read-only views with the top-level pointer.
    let show = parse_config_admin(&strings(&["show"])).unwrap_err();
    assert!(show.contains("phasegent config show"), "got: {show}");
    let get = parse_config_admin(&strings(&["provider", "get"])).unwrap_err();
    assert!(get.contains("phasegent config provider get"), "got: {get}");
    // Bare group invocations keep their distinct errors.
    assert!(parse_config(&strings(&[])).is_err());
    assert!(parse_config_admin(&strings(&[])).is_err());
}

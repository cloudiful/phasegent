//! Admin-group routing tests.
//!
//! Pins the provisioning boundary: `auth setup`, `config set/clear`,
//! `config provider set/clear`, and `workflow bootstrap` parse only under
//! `phasegent admin ...`; the legacy top-level paths fail with a moved
//! error that names the admin form. Read-only views (`config show`,
//! `config provider get`) stay top-level. The `Command` variants are
//! unchanged, so execution, role gating, and the capability matrix keep
//! working without modification.

use crate::command::{self, Command, HelpTopic};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn admin_auth_setup_parses_to_auth_setup() {
    let invocation = command::parse(&strings(&[
        "--role",
        "executor",
        "--provider",
        "redmine",
        "admin",
        "auth",
        "setup",
        "--stdin",
    ]))
    .expect("admin auth setup must parse");
    assert!(matches!(
        invocation.command,
        Command::AuthSetup {
            read_stdin: true,
            ..
        }
    ));
}

#[test]
fn top_level_auth_setup_is_rejected_with_moved_error() {
    let error = command::parse(&strings(&[
        "--role", "executor", "auth", "setup", "--stdin",
    ]))
    .expect_err("top-level auth setup must not parse");
    assert!(
        error.contains("admin auth setup"),
        "moved error must name the admin form: {error}"
    );
}

#[test]
fn admin_config_writes_parse_to_shared_variants() {
    let set = command::parse(&strings(&[
        "--role",
        "executor",
        "admin",
        "config",
        "set",
        "api-base",
        "https://example.com",
    ]))
    .expect("admin config set must parse");
    assert!(matches!(set.command, Command::ConfigSet { .. }));

    let clear = command::parse(&strings(&[
        "--role", "executor", "admin", "config", "clear", "api-base",
    ]))
    .expect("admin config clear must parse");
    assert!(matches!(clear.command, Command::ConfigClear { .. }));

    let provider_set = command::parse(&strings(&["admin", "config", "provider", "set", "redmine"]))
        .expect("admin config provider set must parse");
    assert!(matches!(
        provider_set.command,
        Command::ConfigProviderSet { .. }
    ));

    let provider_clear = command::parse(&strings(&["admin", "config", "provider", "clear"]))
        .expect("admin config provider clear must parse");
    assert!(matches!(
        provider_clear.command,
        Command::ConfigProviderClear
    ));
}

#[test]
fn top_level_config_writes_are_rejected_with_moved_error() {
    for argv in [
        strings(&["--role", "executor", "config", "set", "api-base", "x"]),
        strings(&["--role", "executor", "config", "clear", "api-base"]),
        strings(&["config", "provider", "set", "redmine"]),
        strings(&["config", "provider", "clear"]),
    ] {
        let error = command::parse(&argv).expect_err("top-level config write must not parse");
        assert!(
            error.contains("admin config"),
            "moved error must name the admin form: {error}"
        );
    }
}

#[test]
fn top_level_config_reads_still_parse() {
    let show = command::parse(&strings(&["config", "show"])).expect("config show must parse");
    assert!(matches!(show.command, Command::ConfigShow));
    let get = command::parse(&strings(&["config", "provider", "get"]))
        .expect("config provider get must parse");
    assert!(matches!(get.command, Command::ConfigProviderGet));
}

#[test]
fn admin_group_rejects_read_only_views() {
    let error = command::parse(&strings(&["admin", "config", "show"]))
        .expect_err("admin config show must not parse");
    assert!(
        error.contains("phasegent config show"),
        "rejection must point at the top-level form: {error}"
    );
    let error = command::parse(&strings(&["admin", "config", "provider", "get"]))
        .expect_err("admin config provider get must not parse");
    assert!(
        error.contains("phasegent config provider get"),
        "rejection must point at the top-level form: {error}"
    );
}

#[test]
fn admin_workflow_bootstrap_parses_and_top_level_is_rejected() {
    let invocation = command::parse(&strings(&[
        "--role",
        "admin",
        "--provider",
        "redmine",
        "admin",
        "workflow",
        "bootstrap",
        "--repository",
        "owner/repo",
    ]))
    .expect("admin workflow bootstrap must parse");
    assert!(matches!(invocation.command, Command::Workflow(_)));

    let error = command::parse(&strings(&[
        "--role",
        "admin",
        "--provider",
        "redmine",
        "workflow",
        "bootstrap",
    ]))
    .expect_err("top-level workflow bootstrap must not parse");
    assert!(
        error.contains("admin workflow bootstrap"),
        "moved error must name the admin form: {error}"
    );
}

#[test]
fn admin_help_and_unknown_group_behaviour() {
    let help = command::parse(&strings(&["admin", "--help"])).expect("admin --help must parse");
    assert!(matches!(help.command, Command::Help(HelpTopic::Admin)));

    let error =
        command::parse(&strings(&["admin", "bogus"])).expect_err("unknown admin group must error");
    assert!(
        error.contains("unknown admin command"),
        "unexpected error: {error}"
    );

    let error =
        command::parse(&strings(&["admin", "config"])).expect_err("bare admin config must error");
    assert!(
        error.contains("admin config requires a subcommand"),
        "unexpected error: {error}"
    );
}

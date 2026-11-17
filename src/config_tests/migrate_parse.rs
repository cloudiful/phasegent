//! Parsing tests for the human-operator `admin config migrate` surface.
//!
//! The migration names one absolute source database and an optional
//! absolute TOML overlay. The parser never touches the filesystem, so the
//! tests stay pure.

use crate::command::{self, Command};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn migrate_parses_the_source_database_and_the_optional_overlay() {
    let args = strings(&[
        "admin",
        "config",
        "migrate",
        "--from-sqlite",
        "/tmp/phasegent.sqlite3",
        "--from-toml",
        "/tmp/phasegent.toml",
    ]);
    let invocation = command::parse_with_role_env(&args, None)
        .expect("admin config migrate without a role must parse");
    match invocation.command {
        Command::ConfigMigrate {
            from_sqlite,
            from_toml,
        } => {
            assert_eq!(from_sqlite, "/tmp/phasegent.sqlite3");
            assert_eq!(from_toml.as_deref(), Some("/tmp/phasegent.toml"));
        }
        other => panic!("expected ConfigMigrate, got {other:?}"),
    }
}

#[test]
fn migrate_accepts_the_inline_option_form() {
    let args = strings(&[
        "admin",
        "config",
        "migrate",
        "--from-sqlite=/tmp/phasegent.sqlite3",
    ]);
    let invocation = command::parse_with_role_env(&args, None)
        .expect("the inline --from-sqlite form must parse");
    match invocation.command {
        Command::ConfigMigrate {
            from_sqlite,
            from_toml,
        } => {
            assert_eq!(from_sqlite, "/tmp/phasegent.sqlite3");
            assert!(from_toml.is_none());
        }
        other => panic!("expected ConfigMigrate, got {other:?}"),
    }
}

#[test]
fn migrate_requires_the_source_database_and_rejects_duplicates() {
    let error = command::parse_with_role_env(&strings(&["admin", "config", "migrate"]), None)
        .expect_err("a missing --from-sqlite must be rejected");
    assert!(error.contains("--from-sqlite"), "got: {error}");

    let error = command::parse_with_role_env(
        &strings(&[
            "admin",
            "config",
            "migrate",
            "--from-sqlite",
            "/tmp/a.sqlite3",
            "--from-sqlite",
            "/tmp/b.sqlite3",
        ]),
        None,
    )
    .expect_err("a duplicated --from-sqlite must be rejected");
    assert!(error.contains("duplicate --from-sqlite"), "got: {error}");

    let error = command::parse_with_role_env(
        &strings(&["admin", "config", "migrate", "--from-sqlite"]),
        None,
    )
    .expect_err("a missing --from-sqlite value must be rejected");
    assert!(error.contains("--from-sqlite"), "got: {error}");
}

#[test]
fn the_read_only_top_level_config_group_refuses_the_migration() {
    let error = command::parse_with_role_env(
        &strings(&["config", "migrate", "--from-sqlite", "/tmp/a.sqlite3"]),
        None,
    )
    .expect_err("the read-only config group must refuse the migration");
    assert!(error.contains("admin config migrate"), "got: {error}");
}

#[test]
fn an_ai_role_is_denied_the_migration() {
    let error = command::parse_with_role_env(
        &strings(&[
            "admin",
            "config",
            "migrate",
            "--from-sqlite",
            "/tmp/a.sqlite3",
        ]),
        Some("executor"),
    )
    .expect_err("an AI role must be denied the migration");
    assert!(
        error.contains("not allowed") || error.contains("permission"),
        "got: {error}"
    );
}

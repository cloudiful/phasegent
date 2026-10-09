//! `config set`/`config show`/`config clear` coverage for the canonical global
//! `PHASEGENT_REDMINE_API_BASE` setting and its set-time validation.

use super::super::support::*;
use super::super::*;
use super::*;

#[test]
fn config_set_redmine_api_base_is_global_and_round_trips() {
    with_isolated_storage("set-redmine-global", |_db_path, storage| {
        let outcome = config_write::set_setting_value(
            None,
            "PHASEGENT_REDMINE_API_BASE",
            "https://redmine.example",
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_REDMINE_API_BASE"));
        assert!(
            !text.contains("https://redmine.example"),
            "value must not be echoed: {text}"
        );
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .as_deref(),
            Some("https://redmine.example")
        );
        // The global setting is machine-wide: no role-scoped address row
        // is created for any role.
        for role in [
            Role::Admin,
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
        ] {
            assert!(
                storage
                    .load_redmine_config(role)
                    .unwrap()
                    .and_then(|config| config.api_base)
                    .is_none(),
                "role {} must not receive a scoped Redmine address",
                role.as_str()
            );
        }
        // Snapshot renders the canonical value in the sanitised URL slot.
        let snapshot = config::show(None, storage).unwrap();
        let entries = snapshot["global_settings"].as_array().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_REDMINE_API_BASE")
            .expect("snapshot must name the canonical global address");
        assert_eq!(entry["present"], Value::Bool(true));
        assert!(
            entry["sanitized_value"]
                .as_str()
                .unwrap()
                .contains("redmine.example"),
            "snapshot must expose the sanitised address: {entry:?}"
        );
        // Clear removes the global row.
        let clear =
            config_write::clear_setting(None, "PHASEGENT_REDMINE_API_BASE", storage).unwrap();
        assert!(
            serde_json::to_string(&clear)
                .unwrap()
                .contains("\"cleared\":true")
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn config_show_reports_the_address_once_as_a_global_setting() {
    // The snapshot is the operator-facing surface: the canonical address
    // belongs to `global_settings` and must never reappear as a per-role
    // value that reads as role-scoped.
    with_isolated_storage("show-redmine-global", |_db_path, storage| {
        // A legacy role row stays readable but inert; the snapshot must not
        // present it as the role's address.
        storage
            .save_redmine_config(
                Role::Executor,
                &redmine_config_with_base(Some("https://legacy.example")),
            )
            .unwrap();
        config_write::set_setting_value(
            None,
            "PHASEGENT_REDMINE_API_BASE",
            "https://redmine.example",
            storage,
        )
        .unwrap();

        let snapshot = config::show(None, storage).unwrap();
        for entry in snapshot["roles"].as_array().unwrap() {
            assert!(
                entry.get("redmine_api_base").is_none(),
                "role entry must not carry a Redmine address: {entry:?}"
            );
        }
        let global = snapshot["global_settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_REDMINE_API_BASE")
            .expect("snapshot must name the canonical global address");
        assert_eq!(
            global["sanitized_value"].as_str(),
            Some("https://redmine.example/")
        );
        // The role-scoped close-status id is unaffected by the move.
        let executor = snapshot["roles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["role"] == "executor")
            .expect("executor role entry");
        assert!(executor.get("redmine_close_status_id").is_some());
    });
}

#[test]
fn config_set_redmine_api_base_rejects_invalid_and_credential_urls() {
    with_isolated_storage("set-redmine-invalid", |_db_path, storage| {
        for bad in [
            "not a url",
            "ftp://redmine.example",
            "https://user:secret@redmine.example",
            "https://redmine.example?token=hush",
        ] {
            let error =
                config_write::set_setting_value(None, "PHASEGENT_REDMINE_API_BASE", bad, storage)
                    .unwrap_err();
            assert!(
                !error.contains("secret") && !error.contains("hush"),
                "validation error must not echo credential input: {error}"
            );
            assert!(
                storage
                    .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                    .unwrap()
                    .is_none(),
                "invalid value must not persist"
            );
        }
    });
}

//! `doctor` routing and report tests.
//!
//! The report must identify *which* credential is stored (fingerprint
//! plus store time) while never rendering a secret value, and must
//! mask the PostgreSQL URL down to host/database. Execution tests pin
//! a fresh `PHASEGENT_DB_PATH` so they never touch the operator's
//! real database.

use crate::command::{self, Command, HelpTopic};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn scratch_db(label: &str) -> (std::path::PathBuf, EnvGuard) {
    let dir = std::env::temp_dir().join(format!(
        "phasegent-doctor-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join(crate::infra::storage::DB_FILENAME);
    let guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    (dir, guard)
}

#[test]
fn doctor_parses_without_role_and_rejects_arguments() {
    let invocation =
        command::parse(&strings(&["doctor"])).expect("doctor without --role must parse");
    assert!(matches!(invocation.command, Command::Doctor));

    let error =
        command::parse(&strings(&["doctor", "extra"])).expect_err("doctor arguments must error");
    assert!(
        error.contains("takes no arguments"),
        "unexpected error: {error}"
    );

    let help = command::parse(&strings(&["--help", "doctor"])).expect("--help doctor must parse");
    assert!(matches!(help.command, Command::Help(HelpTopic::Doctor)));
}

#[test]
fn doctor_report_identifies_credentials_without_values() {
    let _lock = lock_workflow_tests();
    let (dir, _guard) = scratch_db("identity");
    let storage = crate::infra::storage::Storage::open().unwrap();
    storage
        .save_credential(crate::policy::Role::Executor, "redmine", "test-secret-KEY9")
        .unwrap();

    let report = crate::cli::doctor::build_report().unwrap();
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(
        !rendered.contains("test-secret-KEY9"),
        "report must never echo a secret value"
    );
    let executor = report
        .roles
        .iter()
        .find(|role| role.role == "executor")
        .expect("executor row must be present");
    assert!(executor.redmine_credential.present);
    assert_eq!(
        executor.redmine_credential.fingerprint.as_deref(),
        Some("KEY9")
    );
    assert!(
        executor.redmine_credential.updated_at.is_some(),
        "store time must be reported"
    );
    assert!(
        !executor.forgejo_credential.present,
        "unset credentials stay absent"
    );
    assert_eq!(report.index.backend, "sqlite");
    assert!(
        report.index.sqlite_path.is_some(),
        "sqlite branch must report the index path"
    );
    assert_eq!(report.index.pg_masked, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn doctor_report_masks_the_postgres_url() {
    let _lock = lock_workflow_tests();
    let (dir, _guard) = scratch_db("pg-mask");
    let _pg = EnvGuard::set(
        "PHASEGENT_INDEX_PG_URL",
        "postgres://bob:hunter2@db.internal:5432/app?sslmode=require",
    );

    let report = crate::cli::doctor::build_report().unwrap();
    assert_eq!(report.index.backend, "postgres");
    assert_eq!(report.index.pg_url_present, Some(true));
    let masked = report
        .index
        .pg_masked
        .clone()
        .expect("masked URL must be present");
    assert!(
        !masked.contains("hunter2") && !masked.contains("bob"),
        "userinfo must be stripped: {masked}"
    );
    assert!(
        !masked.contains("sslmode"),
        "query must be stripped: {masked}"
    );
    assert!(
        masked.contains("db.internal") && masked.contains("app"),
        "host/database stay visible for identification: {masked}"
    );
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(
        !rendered.contains("hunter2"),
        "report must never echo the PG password"
    );
    let _ = std::fs::remove_dir_all(dir);
}

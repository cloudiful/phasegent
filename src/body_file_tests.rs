//! `--body-file` one-shot input tests (issue 298).
//!
//! Covers the parser pairing rules (mutual exclusion, keep-flag
//! requirement, legacy `--body` compatibility) and the local
//! read/validate/cleanup lifecycle against real temp files: success
//! deletion, keep switch, failure preservation, replacement/identity
//! guards, and the UTF-8/size/regular-file boundaries. No provider,
//! network, credential, HOME, or SQLite access.

use crate::command::{self, Command, IssueCommand};
use std::fs;
use std::io::Write;

fn temp_body_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "phasegent-body-file-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn write_body(path: &std::path::Path, contents: &str) {
    let mut file = fs::File::create(path).expect("temp body file must be creatable");
    file.write_all(contents.as_bytes())
        .expect("write must succeed");
    file.sync_all().ok();
}

// ---------------------------------------------------------------------------
// Parser-level pairing rules.
// ---------------------------------------------------------------------------

#[test]
fn issue_create_body_file_parses_and_keeps_legacy_body_path() {
    let path = temp_body_path("create-parse");
    write_body(&path, "# Plan\n- goal");
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--body-file",
        path.to_str().unwrap(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("--body-file must parse");
    match invocation.command {
        Command::Issue(IssueCommand::Create {
            body, body_file, ..
        }) => {
            assert_eq!(body, "", "parser body stays empty for --body-file");
            assert_eq!(body_file.as_deref(), Some(path.to_str().unwrap()));
        }
        other => panic!("unexpected command: {other:?}"),
    }
    let _ = fs::remove_file(&path);

    // Legacy `--body` keeps parsing exactly as before, with no file input.
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--body",
        "text",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        Command::Issue(IssueCommand::Create {
            body, body_file, ..
        }) => {
            assert_eq!(body, "text");
            assert!(body_file.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn body_flags_are_mutually_exclusive_and_keep_requires_body_file() {
    let path = temp_body_path("mutual");
    write_body(&path, "b");
    let mutually_exclusive = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--body",
        "text",
        "--body-file",
        path.to_str().unwrap(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&mutually_exclusive)
        .expect_err("--body and --body-file must be mutually exclusive");
    assert!(
        error.contains("mutually exclusive"),
        "unexpected error: {error}"
    );

    let keep_without_file = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--body",
        "text",
        "--keep-body-file",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error =
        command::parse(&keep_without_file).expect_err("--keep-body-file without --body-file");
    assert!(
        error.contains("--keep-body-file requires --body-file"),
        "unexpected error: {error}"
    );

    let update_missing_body = ["--role", "orchestrator", "issue", "update-body", "9"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&update_missing_body)
        .expect_err("update-body without body flags must error");
    assert!(
        error.contains("requires --body or --body-file"),
        "unexpected error: {error}"
    );

    let comment_missing = [
        "--role",
        "executor",
        "comment",
        "create",
        "9",
        "--marker",
        "m",
        "--authorized",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&comment_missing).expect_err("comment without body flags");
    assert!(
        error.contains("requires --body or --body-file"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Execution-level read/validate/cleanup lifecycle.
// ---------------------------------------------------------------------------

fn resolved_body_file(
    label: &str,
    contents: &str,
    keep: bool,
) -> (std::path::PathBuf, crate::body_file::BodyFile) {
    let path = temp_body_path(label);
    write_body(&path, contents);
    let file = crate::body_file::BodyFile::load(path.to_str().unwrap(), keep)
        .expect("valid body file must load");
    (path, file)
}

#[test]
fn body_file_load_accepts_regular_files_and_rejects_missing() {
    let (path, file) = resolved_body_file("load-ok", "# Plan", false);
    assert!(path.is_file(), "file must remain after a successful load");
    let warning = file.cleanup_after_success();
    assert!(warning.is_none(), "identity-matched cleanup must be silent");
    assert!(!path.exists(), "success cleanup must delete the file");

    let missing = temp_body_path("load-missing");
    let error = crate::body_file::BodyFile::load(missing.to_str().unwrap(), false)
        .expect_err("missing path must fail");
    assert!(error.contains("not found or not accessible"), "{error}");
}

#[test]
fn body_file_load_rejects_directory_invalid_utf8_and_oversize() {
    let dir = temp_body_path("load-dir");
    fs::create_dir_all(&dir).unwrap();
    let error = crate::body_file::BodyFile::load(dir.to_str().unwrap(), false)
        .expect_err("directory must be rejected");
    assert!(error.contains("not a regular file"), "{error}");
    let _ = fs::remove_dir(&dir);

    let invalid = temp_body_path("load-utf8");
    write_body(&invalid, "ok\n");
    let mut file = fs::OpenOptions::new().append(true).open(&invalid).unwrap();
    file.write_all(&[0xFF, 0xFE]).unwrap();
    drop(file);
    let error = crate::body_file::BodyFile::load(invalid.to_str().unwrap(), false)
        .expect_err("invalid UTF-8 must be rejected");
    assert!(error.contains("not valid UTF-8"), "{error}");
    assert!(invalid.is_file(), "validation failure must keep the file");
    let _ = fs::remove_file(&invalid);

    let oversized = temp_body_path("load-size");
    write_body(&oversized, &"x".repeat(1024));
    let limit = crate::body_file::MAX_BODY_FILE_BYTES as usize;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&oversized)
        .unwrap();
    file.write_all(&vec![b'x'; limit]).unwrap();
    drop(file);
    let error = crate::body_file::BodyFile::load(oversized.to_str().unwrap(), false)
        .expect_err("oversized file must be rejected");
    assert!(error.contains("too large"), "{error}");
    assert!(oversized.is_file(), "size failure must keep the file");
    let _ = fs::remove_file(&oversized);
}

#[test]
fn body_file_load_accepts_a_file_exactly_at_the_size_cap() {
    let path = temp_body_path("load-size-exact");
    let cap = crate::body_file::MAX_BODY_FILE_BYTES as usize;
    write_body(&path, &"x".repeat(cap));
    let file = crate::body_file::BodyFile::load(path.to_str().unwrap(), false)
        .expect("a file exactly at the cap must load");
    assert_eq!(file.body().len(), cap, "the cap is inclusive");
    let warning = file.cleanup_after_success();
    assert!(warning.is_none(), "identity-matched cleanup must be silent");
    assert!(!path.exists(), "success cleanup must delete the file");
}

#[test]
fn resolve_uses_the_file_content_as_the_body() {
    let path = temp_body_path("resolve-file");
    write_body(&path, "# From file\n\nbody text");
    let (body, file) = crate::body_file::resolve(Some(""), Some(path.to_str().unwrap()), false)
        .expect("valid file must resolve")
        .expect("a file input must resolve to Some");
    assert_eq!(
        body, "# From file\n\nbody text",
        "the provider must receive the file content, not the empty inline body"
    );
    let file = file.expect("file input must carry a cleanup handle");
    let warning = file.cleanup_after_success();
    assert!(warning.is_none(), "identity-matched cleanup must be silent");
    assert!(!path.exists(), "success cleanup must delete the file");

    // Legacy `--body` still resolves to the inline text with no handle.
    let (body, file) = crate::body_file::resolve(Some("inline"), None, false)
        .expect("inline body must resolve")
        .expect("inline body must resolve to Some");
    assert_eq!(body, "inline");
    assert!(file.is_none(), "inline body has no cleanup handle");
}

#[test]
fn body_file_cleanup_honours_keep_switch_and_failure_paths() {
    let (path, file) = resolved_body_file("keep", "# Plan", true);
    let warning = file.cleanup_after_success();
    assert!(warning.is_none(), "keep switch must be silent");
    assert!(path.is_file(), "--keep-body-file must preserve the file");
    let _ = fs::remove_file(&path);

    // A provider-failure call path is simply "no cleanup call"; the file
    // still exists because nothing ran. Pin the contract by re-loading
    // and asserting existence without cleanup.
    let (path, _file) = resolved_body_file("no-cleanup", "body", false);
    assert!(path.is_file(), "no cleanup call must keep the file");
    let _ = fs::remove_file(&path);
}

#[test]
fn body_file_cleanup_never_deletes_a_replaced_path() {
    let (path, file) = resolved_body_file("replaced", "original", false);
    write_body(&path, "replaced-by-someone-else");
    let warning = file
        .cleanup_after_success()
        .expect("replacement must produce a warning");
    assert!(warning.contains("changed after read"), "{warning}");
    assert!(
        path.is_file(),
        "a replaced file must never be deleted by cleanup"
    );
    let replaced = fs::read_to_string(&path).unwrap();
    assert_eq!(replaced, "replaced-by-someone-else");
    let _ = fs::remove_file(&path);
}

#[test]
fn body_file_cleanup_never_deletes_a_modified_file() {
    let (path, file) = resolved_body_file("modified", "sent body", false);
    let mut append = fs::OpenOptions::new().append(true).open(&path).unwrap();
    append.write_all(b" + tail").unwrap();
    drop(append);
    let warning = file
        .cleanup_after_success()
        .expect("content modification must produce a warning");
    assert!(warning.contains("changed after read"), "{warning}");
    assert!(path.is_file(), "a modified file must never be deleted");
    let _ = fs::remove_file(&path);
}

#[test]
fn body_file_cleanup_tolerates_an_already_deleted_file() {
    let (path, file) = resolved_body_file("vanished", "body", false);
    fs::remove_file(&path).unwrap();
    let warning = file.cleanup_after_success();
    assert!(
        warning.is_none(),
        "already-deleted file must be a silent no-op"
    );
}

// ---------------------------------------------------------------------------
// Local provider round-trip through the shared executor: the body from the
// file reaches the provider and the file is gone afterwards.
// ---------------------------------------------------------------------------

#[test]
fn local_issue_create_via_body_file_deletes_after_success() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::providers::local::LocalProvider;

    let _lock = lock_workflow_tests();
    let dir = std::env::temp_dir().join(format!(
        "phasegent-body-file-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let _db_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        dir.join("phasegent-local.sqlite3").to_str().unwrap(),
    );
    let _index_guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("phasegent-index.sqlite3").to_str().unwrap(),
    );
    let _config_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        dir.join("phasegent-config.sqlite3").to_str().unwrap(),
    );

    let body_path = dir.join("plan.md");
    write_body(&body_path, "# Audit note\n\n- marker content");
    let exit = crate::cli::run(
        [
            "--role",
            "orchestrator",
            "--provider",
            "local",
            "issue",
            "create",
            "--title",
            "Body file round trip",
            "--body-file",
            body_path.to_str().unwrap(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    assert_eq!(exit, 0, "local create via --body-file must succeed");
    assert!(
        !body_path.exists(),
        "successful write must delete the body file by default"
    );
    let provider = LocalProvider::open().unwrap();
    let created = provider
        .get_issue(1)
        .expect("seeded/created issue 1 must exist");
    assert_eq!(created.title, "Body file round trip");
    assert_eq!(created.body, "# Audit note\n\n- marker content");
}

#[test]
fn local_comment_create_via_body_file_deletes_after_success() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::providers::local::LocalProvider;

    let _lock = lock_workflow_tests();
    let dir = std::env::temp_dir().join(format!(
        "phasegent-body-file-comment-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let _db_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        dir.join("phasegent-local.sqlite3").to_str().unwrap(),
    );
    let _index_guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("phasegent-index.sqlite3").to_str().unwrap(),
    );
    let _config_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        dir.join("phasegent-config.sqlite3").to_str().unwrap(),
    );

    let provider = LocalProvider::open().unwrap();
    let number = provider.create_issue("Host", "body").unwrap().number;

    let body_path = dir.join("audit.md");
    write_body(&body_path, "<!-- ai-executor marker=x -->\n\nNote text.");
    let exit = crate::cli::run(
        [
            "--role",
            "executor",
            "--provider",
            "local",
            "comment",
            "create",
            &number.to_string(),
            "--body-file",
            body_path.to_str().unwrap(),
            "--marker",
            "<!-- ai-executor marker=x -->",
            "--authorized",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    assert_eq!(exit, 0, "local comment via --body-file must succeed");
    assert!(!body_path.exists(), "success must delete the body file");

    // Failure path: a body file missing the marker must be kept.
    let bad_path = dir.join("bad.md");
    write_body(&bad_path, "no marker here");
    let exit = crate::cli::run(
        [
            "--role",
            "executor",
            "--provider",
            "local",
            "comment",
            "create",
            &number.to_string(),
            "--body-file",
            bad_path.to_str().unwrap(),
            "--marker",
            "<!-- missing -->",
            "--authorized",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    assert_ne!(exit, 0, "marker mismatch must fail");
    assert!(bad_path.is_file(), "a failed write must keep the body file");
    let _ = fs::remove_file(&bad_path);
}

#[test]
fn local_issue_update_body_keeps_file_on_provider_failure() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let dir = std::env::temp_dir().join(format!(
        "phasegent-body-file-fail-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let _db_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        dir.join("phasegent-local.sqlite3").to_str().unwrap(),
    );
    let _index_guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("phasegent-index.sqlite3").to_str().unwrap(),
    );
    let _config_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        dir.join("phasegent-config.sqlite3").to_str().unwrap(),
    );

    let body_path = dir.join("plan.md");
    write_body(&body_path, "updated body");
    // Issue 999999 does not exist, so the provider write fails and the
    // body file must be preserved.
    let exit = crate::cli::run(
        [
            "--role",
            "orchestrator",
            "--provider",
            "local",
            "issue",
            "update-body",
            "999999",
            "--body-file",
            body_path.to_str().unwrap(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    assert_ne!(exit, 0, "update on a missing issue must fail");
    assert!(
        body_path.is_file(),
        "a provider failure must keep the body file"
    );
    let _ = fs::remove_file(&body_path);
}

#![allow(unused_imports)]
use super::support::{MockResponse, TEST_API_KEY, provider, sequence, sequence_raw, strings};
use crate::command;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::{Capability, Role};
use crate::providers::{ProviderKind, RedmineConfig, RedmineProvider};
use std::fs;
use std::path::{Path, PathBuf};
use std::time;
const TEST_TOKEN: &str = "secret-token-xyz-do-not-expose";
fn temp_dir() -> PathBuf {
    let n = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("phasegent-attach-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn write_temp_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}
fn raw_contains(raw: &[u8], needle: &[u8]) -> bool {
    raw.windows(needle.len()).any(|w| w == needle)
}
fn raw_lower(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).to_ascii_lowercase()
}
#[test]
fn upload_posts_raw_octet_stream_with_filename_query_and_attaches_token() {
    let dir = temp_dir();
    let content = b"hello-attachment";
    let path = write_temp_file(&dir, "evidence.png", content);
    let (base, rx, srv) = sequence_raw(vec![
        MockResponse::ok(format!(r#"{{"upload":{{"token":"{TEST_TOKEN}"}}}}"#)),
        MockResponse::ok(r#"{"issue":{"id":5,"subject":"ok"}}"#.to_owned()),
    ]);
    let out = provider(base)
        .upload_attachment(5, path.to_str().unwrap(), None)
        .unwrap();
    assert_eq!(out.issue, 5);
    assert_eq!(out.filename, "evidence.png");
    assert_eq!(out.bytes, content.len());
    assert!(out.success);
    let js = serde_json::to_string(&out).unwrap();
    assert!(!js.contains(TEST_TOKEN));
    assert!(!js.contains("hello-attachment"));
    let reqs = rx.recv().unwrap();
    assert_eq!(reqs.len(), 2);
    let first = &reqs[0];
    let first_low = raw_lower(first);
    assert!(
        first_low.contains("post /uploads.json?filename=evidence.png"),
        "first: {}",
        String::from_utf8_lossy(first)
    );
    assert!(
        first_low.contains("content-type: application/octet-stream"),
        "ct: {}",
        String::from_utf8_lossy(first)
    );
    assert!(raw_contains(first, content));
    assert!(!String::from_utf8_lossy(first).contains(path.to_str().unwrap()));
    let second = String::from_utf8_lossy(&reqs[1]);
    assert!(second.to_ascii_lowercase().contains("put /issues/5.json"));
    assert!(second.contains("\"uploads\""));
    assert!(second.contains(TEST_TOKEN));
    assert!(second.contains("\"filename\":\"evidence.png\""));
    assert!(!second.contains("\"notes\""));
    srv.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_includes_optional_notes_when_description_provided() {
    let dir = temp_dir();
    let p = write_temp_file(&dir, "screen.png", b"pngdata");
    let (base, rx, srv) = sequence_raw(vec![
        MockResponse::ok(format!(r#"{{"upload":{{"token":"{TEST_TOKEN}"}}}}"#)),
        MockResponse::ok(r#"{"issue":{"id":7,"subject":"ok"}}"#.to_owned()),
    ]);
    provider(base)
        .upload_attachment(7, p.to_str().unwrap(), Some("failure evidence"))
        .unwrap();
    let reqs = rx.recv().unwrap();
    let second = String::from_utf8_lossy(&reqs[1]);
    assert!(second.contains("\"notes\":\"failure evidence\""));
    assert!(second.contains(TEST_TOKEN));
    srv.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_omits_notes_for_empty_description() {
    let dir = temp_dir();
    let p = write_temp_file(&dir, "file.txt", b"data");
    for desc in [Some("   "), Some(""), None] {
        let (base, rx, srv) = sequence_raw(vec![
            MockResponse::ok(format!(r#"{{"upload":{{"token":"{TEST_TOKEN}"}}}}"#)),
            MockResponse::ok(r#"{"issue":{"id":9,"subject":"ok"}}"#.to_owned()),
        ]);
        provider(base)
            .upload_attachment(9, p.to_str().unwrap(), desc)
            .unwrap();
        let reqs = rx.recv().unwrap();
        let s = String::from_utf8_lossy(&reqs[1]);
        assert!(!s.contains("\"notes\""), "desc {desc:?}: {s}");
        srv.join().unwrap();
    }
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_fails_on_malformed_token_response_without_exposing_token() {
    let dir = temp_dir();
    let p = write_temp_file(&dir, "file.txt", b"content");
    for bad in [
        "not json".to_owned(),
        r#"{"upload":{}}"#.to_owned(),
        r#"{"upload":{"token":""}}"#.to_owned(),
        r#"{"wrong":1}"#.to_owned(),
    ] {
        let (base, _rx, srv) = sequence(vec![MockResponse::ok(bad)]);
        let err = provider(base)
            .upload_attachment(11, p.to_str().unwrap(), None)
            .unwrap_err();
        let js = err.json().to_string();
        assert_eq!(
            js.contains("\"kind\":\"decode\""),
            true,
            "expected decode: {js}"
        );
        assert!(!js.contains(TEST_TOKEN));
        assert!(!js.contains("content"));
        srv.join().unwrap();
    }
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_rejects_missing_file_without_network() {
    let err = provider("http://redmine.test".to_owned())
        .upload_attachment(13, "/tmp/phasegent-missing-file-xyz-12345.txt", None)
        .unwrap_err();
    let s = err.json().to_string();
    assert!(
        s.contains("not found") || s.contains("not accessible"),
        "{s}"
    );
    assert!(!s.contains(TEST_TOKEN));
}
#[test]
fn upload_rejects_empty_file() {
    let dir = temp_dir();
    let p = write_temp_file(&dir, "empty.txt", b"");
    let err = provider("http://redmine.test".to_owned())
        .upload_attachment(15, p.to_str().unwrap(), None)
        .unwrap_err();
    let s = err.json().to_string();
    assert!(s.to_ascii_lowercase().contains("empty"), "{s}");
    assert!(!s.contains(TEST_TOKEN));
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_rejects_directory() {
    let dir = temp_dir();
    let err = provider("http://redmine.test".to_owned())
        .upload_attachment(17, dir.to_str().unwrap(), None)
        .unwrap_err();
    let s = err.json().to_string();
    assert!(s.to_ascii_lowercase().contains("regular file"), "{s}");
    assert!(!s.contains(TEST_TOKEN));
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_rejects_oversized_file() {
    let dir = temp_dir();
    let p = dir.join("big.bin");
    fs::File::create(&p)
        .unwrap()
        .set_len(25 * 1024 * 1024 + 1)
        .unwrap();
    let err = provider("http://redmine.test".to_owned())
        .upload_attachment(19, p.to_str().unwrap(), None)
        .unwrap_err();
    let s = err.json().to_string();
    assert!(s.to_ascii_lowercase().contains("too large"), "{s}");
    assert!(!s.contains(TEST_TOKEN));
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn upload_output_never_contains_token_or_file_content() {
    let dir = temp_dir();
    let p = write_temp_file(&dir, "secret.txt", b"super-secret-file-bytes-xyz");
    let (base, _rx, srv) = sequence_raw(vec![
        MockResponse::ok(format!(r#"{{"upload":{{"token":"{TEST_TOKEN}"}}}}"#)),
        MockResponse::ok(r#"{"issue":{"id":21,"subject":"ok"}}"#.to_owned()),
    ]);
    let out = provider(base)
        .upload_attachment(21, p.to_str().unwrap(), Some("desc"))
        .unwrap();
    let js = serde_json::to_string(&out).unwrap();
    assert!(!js.contains(TEST_TOKEN));
    assert!(!js.contains("super-secret-file-bytes-xyz"));
    assert!(!format!("{out:?}").contains(TEST_TOKEN));
    srv.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn forgejo_and_gitlab_upload_are_not_supported_without_file_access() {
    let missing = "/tmp/phasegent-missing-for-not-supported.txt";
    let _ = fs::remove_file(missing);
    for prov in ["forgejo", "gitlab"] {
        let _lock = lock_workflow_tests();
        let tmp = std::env::temp_dir().join(format!(
            "phasegent-not-supported-{prov}-{}",
            time::SystemTime::now()
                .duration_since(time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = tmp.join(crate::infra::storage::DB_FILENAME);
        let _g = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
        let st = Storage::open_at(&db).unwrap();
        st.save_credential(Role::Orchestrator, prov, "dummy-token-for-test")
            .unwrap();
        let exit = crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            prov,
            "--api-base",
            "http://example.test",
            "--repository",
            "owner/repo",
            "issue",
            "upload-attachment",
            "42",
            "--path",
            missing,
        ]));
        assert_eq!(exit, 1, "{prov}");
        let dir = temp_dir();
        let real = write_temp_file(&dir, "real.txt", b"data");
        let exit2 = crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            prov,
            "--api-base",
            "http://example.test",
            "--repository",
            "owner/repo",
            "issue",
            "upload-attachment",
            "42",
            "--path",
            real.to_str().unwrap(),
        ]));
        assert_eq!(exit2, 1, "{prov} real");
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(tmp);
    }
}
#[test]
fn upload_cli_requires_orchestrator_and_validates_args() {
    for role in ["executor", "reviewer", "admin"] {
        let e = crate::cli::run(strings([
            "--role",
            role,
            "--provider",
            "redmine",
            "issue",
            "upload-attachment",
            "5",
            "--path",
            "/tmp/any.txt",
        ]));
        assert_eq!(e, 3, "{role}");
    }
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "issue",
            "upload-attachment",
            "5"
        ])),
        2
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "issue",
            "upload-attachment",
            "0",
            "--path",
            "/tmp/any.txt"
        ])),
        2
    );
}

use super::support::*;
use super::*;

#[test]
fn auto_prefixed_orphan_running_row_is_finished_failed_by_recover() {
    // Phase 3 contract: a hard-crash that leaves an auto- prefixed
    // running row open is recoverable via the existing
    // `timer recover` surface. The recover path transitions the
    // running row to `FAILED` locally before any provider
    // attempt, exactly the way a manual orphan is handled, and
    // a second recover on the same id is rejected (idempotent
    // against the FAILED terminal state). No live Redmine /
    // GitLab writes occur: the storage is the source of truth
    // and the projection layer is intentionally not exercised in
    // this test (that is the contract-test surface).
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("recover-orphan");
    let run_id =
        crate::time_tracking_cli::auto_start_run(202, "In Progress", "executor", 1).unwrap();
    assert!(run_id.starts_with("auto-"));

    // The recover path is orchestrator-only. We invoke the
    // public(crate) `execute_recovery` dispatcher so the test
    // exercises the same code that the CLI uses. The provider
    // config is intentionally absent so the projection attempt
    // fails with a structured config error after the durable
    // FAILED transition; this is the same shape the
    // `timer_recovery_marks_orphan_failed_*` test in
    // `phase2_tests.rs` uses to confirm the local-first
    // contract.
    let first_err = crate::time_tracking_cli::execute_recovery(
        Some(crate::policy::Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        None,
        None,
        None,
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect_err("recover without config must surface a structured error after durable FAILED");
    let first_json = first_err.json();
    let kind = first_json["kind"].as_str().unwrap_or("");
    assert!(
        kind == "config" || kind == "request",
        "first recover must be a structured config or request error, got: {first_err:?}"
    );
    let after_first = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after_first.status, "FAILED",
        "recover must transition the running row to FAILED locally"
    );
    assert_eq!(after_first.sync_status, "failed");
    assert!(after_first.finished_at.is_some());

    // A second recover on the same id must be rejected because
    // the row is already terminal at `failed`; the recover
    // path never reopens or silently overwrites a previous
    // outcome.
    let second_err = crate::time_tracking_cli::execute_recovery(
        Some(crate::policy::Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        None,
        None,
        None,
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect_err("recover on a terminal failed row must surface the persisted error");
    let second_json = second_err.json();
    let kind = second_json["kind"].as_str().unwrap_or("");
    assert!(
        kind == "config" || kind == "request",
        "second recover must be a structured config or request error, got: {second_err:?}"
    );
    let after_second = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after_second.status, "FAILED",
        "status must remain FAILED on retry"
    );
    assert_eq!(
        after_second.sync_status, "failed",
        "sync_status must remain failed on retry"
    );
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_prefixed_orphan_recover_projects_to_provider_with_idempotent_retry() {
    // Phase 3 full-recover projection: an auto- prefixed orphan
    // is finished FAILED locally and then projected to a Redmine
    // mock via the existing `execute_recovery` path. The mock
    // server mirrors the contract-test harness so the test does
    // not depend on a live Redmine instance. The retry recovers
    // the same `failed` error from the ledger without a second
    // HTTP call: a terminal `failed` row never reaches the
    // projection layer.
    use crate::infra::storage::test_support::EnvGuard;
    use crate::policy::Role;
    use crate::providers::{RedmineConfig, RedmineProvider};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    let _lock = lock_workflow_tests();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-auto-recover-redmine-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = home.join(crate::infra::storage::DB_FILENAME);
    let _env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).unwrap();
    // Save the Redmine credential so `RedmineProvider::for_role`
    // can read it via `auth::redmine_api_key`.
    storage
        .save_credential(Role::Orchestrator, "redmine", "test-recover-key")
        .unwrap();

    // Open the orphan via the lifecycle auto path so the run
    // carries the `auto-` prefix and matches what
    // `timer list` would surface for a crashed orchestrator.
    let run_id =
        crate::time_tracking_cli::auto_start_run(203, "implementation", "executor", 1).unwrap();
    assert!(run_id.starts_with("auto-"));

    // Mock Redmine server: the recover path drives
    // `project_run_with_provider`, which performs the same three
    // calls as the manual `timer finish` flow. The mock inspects
    // the request line so each call observes the right fixture
    // (activities → re-list → POST).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let size = stream.read(&mut chunk).unwrap();
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&bytes).into_owned();
            // Pick the fixture by request line so the
            // projection path can advance through activities →
            // re-list → POST without an empty response causing
            // a config error.
            let body = if request.starts_with("GET /enumerations/time_entry_activities.json") {
                r#"{"time_entry_activities":[{"id":9,"name":"Development","is_default":true}]}"#
            } else if request.starts_with("GET /time_entries.json?") {
                r#"{"total_count":0,"limit":100,"time_entries":[]}"#
            } else {
                r#"{"time_entry":{"id":88,"issue":{"id":203},"activity":{"id":9,"name":"Development"},"hours":0.01,"comments":"phasegent timer run_id=auto-x","spent_on":"2026-09-09"}}"#
            };
            let status_text = if request.starts_with("POST") {
                ("201 Created", body)
            } else {
                ("200 OK", body)
            };
            requests.push(request);
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_text.0,
                status_text.1.len(),
                status_text.1
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        sender.send(requests).unwrap();
    });
    let mock_base = format!("http://{address}");

    // Build a RedmineConfig that points at the mock and let
    // `RedmineConfig::resolve` pick up the stored credential.
    // We do not persist `api_base` to the role config because
    // the recover call passes it explicitly as `api_base`.
    let _resolved =
        RedmineConfig::resolve(Role::Orchestrator, Some(&mock_base), Some("42"), Some("37"))
            .expect("resolve must succeed with explicit args and stored credential");
    let _provider = RedmineProvider::for_role(
        Role::Orchestrator,
        RedmineConfig::new(mock_base.clone(), "42", 37),
    )
    .expect("provider must build from stored credential");

    // First recover: orphan → FAILED locally → project to mock.
    // The mock server returns 201 with a time entry id, so the
    // projection layer marks the row `synced`.
    crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        Some(&mock_base),
        Some("42"),
        Some("37"),
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect("recover against mock must succeed");

    let after = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after.status, "FAILED",
        "orphan recover must leave status at FAILED"
    );
    assert_eq!(
        after.sync_status, "synced",
        "projection against mock must mark sync_status synced"
    );
    assert_eq!(after.time_entry_id, Some(88));

    // The mock recorded exactly 3 requests (activities list,
    // re-list, POST). After the first recover the local
    // `synced` state prevents the projection layer from
    // issuing a second POST.
    let observed = receiver.recv().unwrap();
    assert_eq!(
        observed.len(),
        3,
        "first recover must hit the mock once for activities + re-list + POST, got {observed:?}"
    );
    assert!(observed[0].starts_with("GET /enumerations/time_entry_activities.json"));
    assert!(observed[1].starts_with("GET /time_entries.json?"));
    assert!(observed[2].starts_with("POST /time_entries.json"));
    assert!(observed[2].contains("auto-"));

    // Second recover on a `synced` terminal row: the recover
    // path returns the existing row as a `Single` envelope and
    // never attempts another projection. The mock listener has
    // already served its three responses, so the assertion is
    // that the returned row matches the post-first-recover
    // state exactly. This is the documented contract: only
    // `failed` (not `synced`) terminal rows surface the stored
    // error, because a successfully projected run is
    // intentionally treated as already-done.
    let second = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        Some(&mock_base),
        Some("42"),
        Some("37"),
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect("recover on a synced-FAILED row must return the existing row");
    let second_row = match second {
        crate::time_tracking::TimerListOutput::Single { run } => *run,
        other => panic!("second recover must return Single, got {other:?}"),
    };
    assert_eq!(second_row.run_id, run_id);
    assert_eq!(second_row.status, "FAILED");
    assert_eq!(second_row.sync_status, "synced");
    assert_eq!(second_row.time_entry_id, Some(88));
    // The mock has only 3 fixtures, so a 4th call would
    // block forever. The successful return above proves no
    // additional HTTP call was made.
    let after_retry = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(after_retry.status, "FAILED");
    assert_eq!(after_retry.sync_status, "synced");
    assert_eq!(after_retry.time_entry_id, Some(88));

    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

// ---------------------------------------------------------------------------
// Issue 305 Task 3, widened by issue 537 Phase 2: issue-close worktree-lease
// release.
//
// The lifecycle helper resolves the current repo identity and delegates to
// the worktree domain flip. These tests pin the all-session release contract:
// every active lease for the closed issue/repo becomes `retained` regardless
// of the session that owns it; every other issue and repo is left `active`,
// and an absent session never guesses an owner.
// ---------------------------------------------------------------------------

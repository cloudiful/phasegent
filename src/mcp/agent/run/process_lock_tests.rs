//! Regression coverage for the per-database ledger lock (issue 685 P2
//! checkpoint blocker).
//!
//! The blocker was a *second process*: `RunManager::new` ran recovery before
//! anything established who owned the ledger, so a second `phasegent mcp
//! serve` over the same database read the first process's live `running` row,
//! found no local flight for it, and marked it `interrupted` — after which the
//! row was resumable and would spawn a second ACP process for a live session.
//! The in-process shared manager never covered that, because it is process-wide.
//!
//! So these tests are about the two directions of the same property. A
//! competing claim is refused *without touching a row*, and the refusal ends
//! the instant its owner process exits, after which the successor recovers
//! exactly what the dead owner left behind. The second direction needs a real
//! second process; the child half runs in the test binary itself, selected by
//! an environment variable, so no helper binary or script is involved and the
//! test works wherever the crate builds.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::infra::storage::Storage;
use crate::mcp::agent::RunManager;
use crate::mcp::agent::store::RUN_RUNNING;
use crate::mcp::agent::store_tests::temp_db_path;

use super::process_lock::{LOCK_FILE_NAME, ProcessLock, ProcessLockError, lock_path_for};

/// How long the parent waits for a child handshake before failing.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(30);
/// How long the child holds the ledger before giving up on its parent.
const CHILD_HOLD_BUDGET: Duration = Duration::from_secs(30);
/// Poll interval for the file handshakes.
const POLL: Duration = Duration::from_millis(10);

/// Environment variables of the child half of the cross-process test.
const CHILD_DB: &str = "PHASEGENT_P2_LOCK_CHILD_DB";
const CHILD_READY: &str = "PHASEGENT_P2_LOCK_CHILD_READY";
const CHILD_EXIT: &str = "PHASEGENT_P2_LOCK_CHILD_EXIT";

/// The child half. In a normal test run none of the variables are set, so this
/// returns immediately; the parent half spawns this same test binary with the
/// variables set and `--exact` naming this test.
///
/// The child claims the ledger, announces it, leaves a live `running` row
/// behind, and holds the claim until its parent asks it to stop. Returning
/// drops the manager, which closes the lock file, and the process then exits —
/// the two ways a kernel-held lock is released, both real here.
#[test]
fn child_process_claims_the_ledger_and_holds_it_until_signalled() {
    let Some(db) = std::env::var_os(CHILD_DB) else {
        return;
    };
    let db = PathBuf::from(db);
    let ready = PathBuf::from(std::env::var(CHILD_READY).expect("child ready path"));
    let exit = PathBuf::from(std::env::var(CHILD_EXIT).expect("child exit path"));

    let manager = RunManager::new(Storage::open_at(&db).expect("child storage"))
        .expect("child claims the ledger");
    manager
        .connection()
        .create_work_run("child-live", "/tmp/wt-child", "prompt")
        .expect("child seeds a run");
    manager
        .connection()
        .update_work_run(
            "child-live",
            &crate::mcp::agent::store::WorkRunUpdate {
                status: Some(RUN_RUNNING.to_owned()),
                ..crate::mcp::agent::store::WorkRunUpdate::default()
            },
        )
        .expect("child marks the run live");
    announce(&ready);

    let deadline = Instant::now() + CHILD_HOLD_BUDGET;
    while !exit.exists() && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    assert!(
        exit.exists(),
        "the parent never signalled the child to stop"
    );
    // `manager` drops here: the lock is released before the process exits, so
    // the parent's post-exit claim does not depend on process teardown alone.
    drop(manager);
}

// ---------------------------------------------------------------------------
// One process, two claims
// ---------------------------------------------------------------------------

/// A second claim over the same database in this process is refused, and a
/// refusal never rewrites the live row the holder owns.
#[test]
fn a_second_claim_in_one_process_is_refused_without_recovering_rows() {
    let path = temp_db_path("lock-same-process");
    let holder =
        RunManager::new(Storage::open_at(&path).expect("holder storage")).expect("first claim");
    seed_live_run(&holder, "live-1");

    let error = expect_refused(Storage::open_at(&path).expect("second storage"));
    assert!(error.contains("another phasegent process"), "{error}");
    // The refusal is the entire point: recovery must not have run, so the
    // holder's live row is still `running` rather than `interrupted`.
    assert_eq!(read_status(&path, "live-1"), RUN_RUNNING);
    drop(holder);
}

/// Dropping the holder releases the lock inside the same process, which is the
/// mechanism the cross-process case relies on at process exit.
#[test]
fn dropping_the_holder_releases_the_claim_for_the_next_manager() {
    let path = temp_db_path("lock-drop-release");
    let holder =
        RunManager::new(Storage::open_at(&path).expect("holder storage")).expect("first claim");
    seed_live_run(&holder, "live-1");
    assert!(RunManager::new(Storage::open_at(&path).expect("blocked storage")).is_err());
    drop(holder);

    let successor = RunManager::new(Storage::open_at(&path).expect("successor storage"))
        .expect("the released claim is claimable");
    // The successor owns the ledger now, so it recovers what the predecessor
    // left active — which is what makes an interrupted run resumable.
    assert_eq!(read_status(&path, "live-1"), "interrupted");
    assert!(successor.ledger_lock_path().exists());
}

/// Cloning a manager shares its held lock instead of taking a second one, so a
/// long-lived clone can never be a competing claim.
#[test]
fn a_cloned_manager_shares_one_held_lock() {
    let path = temp_db_path("lock-clone");
    let manager = RunManager::new(Storage::open_at(&path).expect("storage")).expect("claim");
    let clone = manager.clone();
    assert!(manager.shares_ledger_lock_with(&clone));
    assert_eq!(manager.ledger_lock_path(), clone.ledger_lock_path());
    assert_eq!(
        manager
            .ledger_lock_path()
            .file_name()
            .and_then(|name| name.to_str()),
        Some(LOCK_FILE_NAME)
    );
    assert_eq!(
        lock_path_for(&path),
        manager.ledger_lock_path(),
        "the lock lives beside the database it guards"
    );
    assert!(
        manager.ledger_lock_path().parent() == path.parent(),
        "the lock file stays in the database's own (owner-only) directory"
    );
    // The shared claim is not a second claim: the clone does not block it.
    drop(clone);
    assert!(RunManager::new(Storage::open_at(&path).expect("blocked storage")).is_err());
    drop(manager);
}

/// The lock is a real advisory lock even without a database: two handles on one
/// lock file contend, and the loser gets the `Held` classification rather than
/// an unexplained I/O failure.
#[test]
fn two_raw_lock_handles_contend_and_the_loser_is_classified_as_held() {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-process-lock-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    fs::create_dir_all(&directory).expect("scratch dir");
    let database = directory.join("phasegent.sqlite3");
    let held = ProcessLock::acquire(&database).expect("first raw claim");
    match ProcessLock::acquire(&database) {
        Err(ProcessLockError::Held) => {}
        Err(other) => panic!("expected Held, got {other:?}"),
        Ok(lock) => panic!("expected Held, got {}", lock.path().display()),
    }
    drop(held);
    ProcessLock::acquire(&database).expect("the released lock is claimable");
    fs::remove_dir_all(&directory).ok();
}

/// An unusable lock path is a refusal too, never a silent pass. The lock file
/// is opened beside the database, and this module deliberately does not create
/// that directory — `Storage::open_at` owns it, owner-only, and a claim that
/// cannot create its own rendezvous must refuse rather than pretend the ledger
/// is unowned.
#[test]
fn an_unusable_lock_path_refuses_rather_than_passing() {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-process-lock-bad-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    // The database's parent directory does not exist, so the lock file cannot
    // be created there.
    let error = match ProcessLock::acquire(
        &directory
            .join("missing-directory")
            .join("phasegent.sqlite3"),
    ) {
        Err(error) => error,
        Ok(lock) => panic!(
            "an unopenable lock file must refuse, got {}",
            lock.path().display()
        ),
    };
    match error {
        ProcessLockError::Unavailable(detail) => assert!(!detail.is_empty()),
        ProcessLockError::Held => panic!("an open failure is not contention"),
    }
    fs::remove_dir_all(&directory).ok();
}

// ---------------------------------------------------------------------------
// Two processes
// ---------------------------------------------------------------------------

/// The blocker's own scenario, end to end across processes.
#[test]
fn a_competing_process_is_refused_and_its_exit_hands_the_ledger_over() {
    let path = temp_db_path("lock-cross-process");
    let ready = sibling_path(&path, "ready");
    let exit = sibling_path(&path, "exit");
    // Create the database and its directory before the child opens it.
    drop(Storage::open_at(&path).expect("parent storage"));

    let child = spawn_lock_child(&path, &ready, &exit);
    await_file(&ready, HANDSHAKE_BUDGET, "the child to claim the ledger");

    // The child holds the ledger and has left a live `running` row. This claim
    // must be refused, and the child's row must survive it untouched — the
    // exact failure the checkpoint review reproduced.
    let error = expect_refused(Storage::open_at(&path).expect("parent storage"));
    assert!(error.contains("another phasegent process"), "{error}");
    assert_eq!(
        read_status(&path, "child-live"),
        RUN_RUNNING,
        "the child's live run must not be marked interrupted"
    );
    // The refusal cannot spawn either: it yields no manager, so `start_run` is
    // unreachable and the ledger still holds exactly the child's one row.
    assert_eq!(
        row_count(&path),
        1,
        "a refused claim must not create a run row"
    );

    // Release the child, whose exit is what the kernel acts on.
    announce(&exit);
    let output = child.wait_with_output().expect("child exit");
    assert!(
        output.status.success(),
        "the lock child failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // The successor claims the freed ledger and recovers what the child left
    // active, which is what makes the row resumable in this process.
    let successor = RunManager::new(Storage::open_at(&path).expect("successor storage"))
        .expect("the ledger is free once its owner exits");
    assert_eq!(
        read_status(&path, "child-live"),
        "interrupted",
        "the successor recovers the dead owner's active row"
    );
    drop(successor);
}

/// Spawn the test binary, running only the child half of the handshake.
fn spawn_lock_child(db: &Path, ready: &Path, exit: &Path) -> Child {
    let binary = std::env::current_exe().expect("test binary path");
    Command::new(binary)
        .args([
            "--exact",
            "mcp::agent::run::process_lock_tests::child_process_claims_the_ledger_and_holds_it_until_signalled",
            "--nocapture",
        ])
        .env(CHILD_DB, db)
        .env(CHILD_READY, ready)
        .env(CHILD_EXIT, exit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the lock child")
}

fn await_file(path: &Path, budget: Duration, what: &str) {
    let deadline = Instant::now() + budget;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} at {}",
            path.display()
        );
        std::thread::sleep(POLL);
    }
}

fn announce(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut file = fs::File::create(path).expect("handshake file");
    file.write_all(b"ok").expect("handshake write");
}

/// Claim the ledger, expecting a refusal, and return the refusal's message.
/// `RunManager` deliberately has no `Debug` (it holds a live lock and a storage
/// handle), so `expect_err` is not available for it.
fn expect_refused(storage: Storage) -> String {
    match RunManager::new(storage) {
        Err(error) => error,
        Ok(_) => panic!("the ledger claim must be refused while another process holds it"),
    }
}

/// A handshake file beside the scratch database.
fn sibling_path(db: &Path, suffix: &str) -> PathBuf {
    let parent = db.parent().expect("scratch parent");
    parent.join(format!("lock-handshake-{suffix}"))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Seed one live `running` row through the holder's own connection, so the
/// manager has no in-flight entry for it (a row it did not start).
fn seed_live_run(manager: &RunManager, run_id: &str) {
    manager
        .connection()
        .create_work_run(run_id, "/tmp/wt-live", "prompt")
        .expect("seed run");
    manager
        .connection()
        .update_work_run(
            run_id,
            &crate::mcp::agent::store::WorkRunUpdate {
                status: Some(RUN_RUNNING.to_owned()),
                ..crate::mcp::agent::store::WorkRunUpdate::default()
            },
        )
        .expect("mark the run live");
}

/// Read one row's status on a fresh connection: plain reads take no lock, so a
/// competing process can still observe what it must not touch.
fn read_status(db: &Path, run_id: &str) -> String {
    Storage::open_at(db)
        .expect("reader storage")
        .load_work_run(run_id)
        .expect("read run")
        .expect("run row")
        .status
}

/// Every run row in the ledger, on a fresh read-only connection. Used to prove
/// a refusal neither rewrote nor added one.
fn row_count(db: &Path) -> usize {
    Storage::open_at(db)
        .expect("reader storage")
        .list_work_runs(false, 1_000)
        .expect("list runs")
        .len()
}

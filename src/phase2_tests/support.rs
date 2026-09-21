use super::*;

pub(crate) fn mock_server_with_headers(
    body: &str,
    response_headers: &[&str],
) -> (String, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let body = body.to_owned();
    let response_headers = response_headers
        .iter()
        .map(|header| (*header).to_owned())
        .collect::<Vec<_>>();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8192];
        let size = stream.read(&mut request).unwrap();
        sender
            .send(String::from_utf8_lossy(&request[..size]).into_owned())
            .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
            body.len(),
            response_headers
                .iter()
                .map(|header| format!("{header}\r\n"))
                .collect::<String>(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (format!("http://{address}/api/v1"), receiver, server)
}

pub(crate) fn close_cli_root(label: &str) -> std::path::PathBuf {
    let root = crate::test_scratch::root().join(format!(
        "phasegent-close-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

pub(crate) fn close_cli_init_repo(repo: &std::path::Path) {
    fs::create_dir_all(repo).unwrap();
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    runner
        .run(&["init", "-q", "-b", "main"], repo)
        .expect("git init");
    let _ = runner.run(
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        repo,
    );
}

pub(crate) fn close_cli_lease_state(lease_id: &str) -> (String, Option<String>) {
    let storage = Storage::open().unwrap();
    let row = crate::worktree::leases::load_lease(&storage, lease_id)
        .unwrap()
        .unwrap();
    (row.status, row.release_reason)
}

pub(crate) fn close_cli_seed_issue(title: &str, status: &str) -> u64 {
    let provider = crate::providers::local::LocalProvider::open().unwrap();
    let issue = provider.create_issue(title, "body").unwrap();
    if status != "New" {
        provider
            .with_conn("seed issue status", |conn| {
                conn.execute(
                    "UPDATE local_issues SET status = ?1 WHERE id = ?2",
                    rusqlite::params![status, issue.number as i64],
                )?;
                Ok(())
            })
            .unwrap();
    }
    issue.number
}

pub(crate) fn close_cli_add_worktree(
    repo: &std::path::Path,
    label: &str,
    branch: &str,
) -> std::path::PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-close-wt-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let output = runner
        .run(
            &[
                "worktree",
                "add",
                dir.to_str().expect("utf8 worktree path"),
                "-b",
                branch,
                "HEAD",
            ],
            repo,
        )
        .expect("git worktree add runs");
    assert_eq!(output.status, 0, "git worktree add must succeed");
    dir
}

pub(crate) fn close_cli_seed_lease_at(
    identity: &str,
    issue: u64,
    session: &str,
    status: &str,
    worktree_path: &std::path::Path,
) -> String {
    let storage = Storage::open().unwrap();
    crate::worktree::ensure_schema(&storage).unwrap();
    let lease_id = format!(
        "close-cli-cleanup-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-close-cli-checkout",
            worktree_path: worktree_path.to_str().expect("utf8 worktree path"),
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .unwrap();
    lease_id
}

pub(crate) fn close_cli_branch_exists(repo: &std::path::Path, branch: &str) -> bool {
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let reference = format!("refs/heads/{branch}");
    runner
        .run(&["show-ref", "--verify", "--quiet", &reference], repo)
        .expect("git show-ref runs")
        .status
        == 0
}

pub(crate) fn sync_cli_local_dispatcher() -> crate::providers::ProviderDispatcher {
    crate::providers::ProviderDispatcher::local(
        crate::providers::local::LocalProvider::open().expect("local provider"),
    )
}

/// Run one reconciliation pass against the temp local provider and return
/// its report.
pub(crate) fn sync_cli_pass(
    repo: &std::path::Path,
    all: bool,
    mode: crate::cli::issue::sync::SyncMode,
) -> crate::cli::issue::sync::SyncReport {
    let provider = sync_cli_local_dispatcher();
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let storage = Storage::open().expect("storage");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    crate::cli::issue::sync::run_sync(
        &provider,
        &runner,
        &storage,
        crate::cli::issue::sync::SyncRequest {
            all,
            mode,
            cwd: repo,
        },
    )
    .expect("reconciliation pass")
}

/// Seed one local issue directly in the provider's closed state, the way a
/// web close leaves it (`is_closed` status without a local close chain).
pub(crate) fn sync_cli_seed_closed_issue(title: &str) -> u64 {
    close_cli_seed_issue(title, "Closed")
}

/// Run `issue sync` through the CLI executor from `cwd`.
pub(crate) fn sync_cli_run_cli(
    provider: ProviderKind,
    all: bool,
    no_clean: bool,
    cwd: &std::path::Path,
) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(provider),
        None,
        None,
        None,
        None,
        command::IssueCommand::Sync { all, no_clean },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

/// Run one `worktree` subcommand through the CLI executor from `cwd`.
pub(crate) fn sync_cli_run_worktree(
    command: crate::command::WorktreeCommand,
    cwd: &std::path::Path,
) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::worktree::execute_worktree(Some(Role::Orchestrator), command);
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

/// A bound-then-released loopback port, so the connection is refused fast
/// without depending on a well-known port being free.
pub(crate) fn sync_cli_dead_api_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/api/v1")
}

pub(crate) fn sync_cli_identity(repo: &std::path::Path) -> String {
    crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), repo)
        .expect("repository identity")
}

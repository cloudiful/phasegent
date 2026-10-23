//! Terminal acceptance tests (issue #1054 P4).
//!
//! These scenarios run the compiled binary on a real POSIX pseudo-terminal, so
//! the assertions cover what a person actually sees: which format `auto`
//! resolves to on a terminal, whether the spinner animates, and whether a
//! result or a diagnostic ever collides with it.
//!
//! Every run gets an isolated home, XDG roots, database, and config path; the
//! only provider is a local TCP fake, so no test touches real operator storage
//! or the network. The pipe-side channel separation is covered by
//! `tests/cli_output_modes.rs`; a pty merges stdout and stderr, so these
//! scenarios assert on the merged terminal transcript and the exit code.

#[cfg(unix)]
mod posix {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A per-test scratch directory that removes itself when the test ends.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "phasegent-output-pty-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create scratch root");
            Self { root }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// The frame glyphs the spinner uses on a Unicode terminal. A captured pty
    /// transcript keeps them even after the line is cleared, so their presence
    /// proves the progress ran.
    const SPINNER_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    /// The transcript without the color/style sequences, so an assertion
    /// reads the text a person reads rather than the escape bytes.
    fn plain(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '\u{1b}' {
                out.push(character);
                continue;
            }
            match chars.peek() {
                // A CSI sequence runs from `[` to its final byte in `@..=~`.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // Any other escape sequence drops its single introducer.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        }
        out
    }

    /// The transcript without the pty's newline translation, so a remaining
    /// carriage return is a real redraw rather than a line ending.
    fn without_newline_translation(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    /// Whether the transcript carries a cursor redraw.
    fn has_redraw(text: &str) -> bool {
        without_newline_translation(text).contains('\r')
    }

    /// A one-shot HTTP fake that pauses before it answers, so a provider
    /// request is long enough to animate deterministically. It binds the
    /// loopback interface only and never reaches the network.
    struct SlowProvider {
        base: String,
    }

    impl SlowProvider {
        fn start(delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback provider");
            let base = format!(
                "http://{}",
                listener.local_addr().expect("loopback address")
            );
            // The listener and its thread die with the test process; a request
            // that never arrives must not block the suite, so the handle is
            // detached rather than joined.
            std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                std::thread::sleep(delay);
                let body = "{\"error\":\"provider unavailable\"}";
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
            Self { base }
        }
    }

    /// One pty run: the merged terminal transcript and the exit code.
    struct PtyRun {
        code: i32,
        transcript: String,
    }

    /// The Python driver that puts the child on a pseudo-terminal. `python3` is
    /// a required part of the POSIX test environment, so a missing interpreter
    /// fails the test instead of silently skipping it.
    const PTY_DRIVER: &str = r#"
import os, pty, select, subprocess, sys
master, slave = pty.openpty()
proc = subprocess.Popen(sys.argv[1:], stdin=subprocess.DEVNULL, stdout=slave, stderr=slave, close_fds=True)
os.close(slave)
data = b""
while True:
    ready, _, _ = select.select([master], [], [], 30.0)
    if not ready:
        proc.kill()
        break
    try:
        chunk = os.read(master, 65536)
    except OSError:
        break
    if not chunk:
        break
    data += chunk
code = proc.wait()
sys.stdout.buffer.write(data)
sys.stdout.buffer.flush()
sys.exit(code)
"#;

    /// Run the binary on a pseudo-terminal with an isolated environment.
    ///
    /// `role` models `PHASEGENT_ROLE` as a test input; `None` leaves the caller
    /// as the ordinary Operator. The managed markers are cleared so the caller
    /// context is explicit, and `TERM`/`LANG` are pinned so terminal detection
    /// and the spinner glyph set are deterministic.
    fn run_pty(
        scratch: &Scratch,
        role: Option<&str>,
        extra_env: &[(&str, &str)],
        args: &[&str],
    ) -> PtyRun {
        let driver = scratch.path("pty-driver.py");
        if !driver.exists() {
            fs::write(&driver, PTY_DRIVER).expect("write pty driver");
        }
        let mut command = Command::new("python3");
        command
            .arg(&driver)
            .arg(env!("CARGO_BIN_EXE_phasegent"))
            .args(args)
            .env("HOME", scratch.path("home"))
            .env("XDG_CONFIG_HOME", scratch.path("config"))
            .env("XDG_DATA_HOME", scratch.path("data"))
            .env("XDG_CACHE_HOME", scratch.path("cache"))
            .env("PHASEGENT_DB_PATH", scratch.path("phasegent.sqlite3"))
            .env("PHASEGENT_CONFIG_PATH", scratch.path("missing.toml"))
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env_remove("LC_ALL")
            .env_remove("LC_CTYPE")
            .env_remove("NO_COLOR")
            .env_remove("CI")
            .env_remove("CONTINUOUS_INTEGRATION")
            .env_remove("PHASEGENT_PROVIDER")
            .env_remove("PHASEGENT_DEFAULT_PROVIDER")
            .env_remove("PHASEGENT_API_BASE")
            .env_remove("PHASEGENT_REDMINE_API_BASE")
            .env_remove("PHASEGENT_SEARCH_ENDPOINT");
        match role {
            Some(role) => {
                command.env("PHASEGENT_ROLE", role);
            }
            None => {
                command.env_remove("PHASEGENT_ROLE");
            }
        }
        command.env_remove("PHASEGENT_SESSION_ID");
        command.env_remove("OPENCODE_SESSION_ID");
        for (name, value) in extra_env {
            command.env(name, value);
        }
        let output = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn the pty driver");
        PtyRun {
            code: output.status.code().unwrap_or(-1),
            transcript: String::from_utf8_lossy(&output.stdout).into_owned(),
        }
    }

    /// Run one non-pty CLI step with the isolated environment of a scenario.
    fn run_iso(scratch: &Scratch, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_phasegent"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", scratch.path("home"))
            .env("XDG_CONFIG_HOME", scratch.path("config"))
            .env("XDG_DATA_HOME", scratch.path("data"))
            .env("XDG_CACHE_HOME", scratch.path("cache"))
            .env("PHASEGENT_DB_PATH", scratch.path("phasegent.sqlite3"))
            .env("PHASEGENT_CONFIG_PATH", scratch.path("missing.toml"))
            .env_remove("PHASEGENT_ROLE")
            .env_remove("PHASEGENT_SESSION_ID")
            .env_remove("OPENCODE_SESSION_ID")
            .env_remove("CI")
            .env_remove("CONTINUOUS_INTEGRATION")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn phasegent")
    }

    /// A task-owned Git repository plus one retained lease, so a prune pass has
    /// a real candidate. Everything lives inside the scratch root; no operator
    /// worktree is ever read, pruned, or moved.
    struct PruneFixture {
        repo: PathBuf,
    }

    impl PruneFixture {
        fn new(scratch: &Scratch) -> Self {
            let repo = scratch.path("repo");
            fs::create_dir_all(&repo).expect("create fixture repo");
            for args in [
                &["init", "-q", "."][..],
                &["config", "user.email", "pty@example.invalid"][..],
                &["config", "user.name", "pty fixture"][..],
            ] {
                let status = Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .expect("run git");
                assert!(status.success(), "git {args:?} failed");
            }
            fs::write(repo.join("a.txt"), "fixture\n").expect("write fixture file");
            for args in [&["add", "a.txt"][..], &["commit", "-qm", "init"][..]] {
                let status = Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .expect("run git");
                assert!(status.success(), "git {args:?} failed");
            }
            let acquired = run_iso(
                scratch,
                &repo,
                &[
                    "worktree",
                    "acquire",
                    "--issue",
                    "7",
                    "--isolate",
                    "--session",
                    "ses_ptyfixture",
                ],
            );
            assert!(
                acquired.status.success(),
                "fixture acquire failed: {}",
                String::from_utf8_lossy(&acquired.stderr)
            );
            let value: serde_json::Value =
                serde_json::from_slice(&acquired.stdout).expect("acquire prints one document");
            let lease = value
                .get("lease_id")
                .and_then(serde_json::Value::as_str)
                .expect("acquire names the lease")
                .to_owned();
            // A retained lease keeps the directory a real prune candidate while
            // no active lease protects it.
            let released = run_iso(scratch, &repo, &["worktree", "release", "--lease", &lease]);
            assert!(
                released.status.success(),
                "fixture release failed: {}",
                String::from_utf8_lossy(&released.stderr)
            );
            Self { repo }
        }
    }

    /// A fake `opencode` client that answers the running-session query after a
    /// pause, so a prune pass is long enough to animate deterministically. It
    /// is a task-owned shell script inside the scratch root and talks to no
    /// service.
    fn slow_opencode(scratch: &Scratch, delay: Duration) -> PathBuf {
        let bin = scratch.path("bin");
        fs::create_dir_all(&bin).expect("create fake bin");
        let script = bin.join("opencode");
        let seconds = delay.as_millis() as f64 / 1000.0;
        fs::write(
            &script,
            format!("#!/bin/sh\nsleep {seconds}\nprintf '{{\"data\":{{}}}}'\n"),
        )
        .expect("write fake opencode");
        let mut permissions = fs::metadata(&script).expect("fake metadata").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&script, permissions).expect("make fake executable");
        bin
    }

    /// The PATH for one scenario, with the fake client first.
    fn path_with(bin: &Path) -> String {
        let ambient = std::env::var("PATH").unwrap_or_default();
        format!("{}:{ambient}", bin.display())
    }

    /// Store a Redmine credential and the fake provider address through the
    /// product path, so a slow request never depends on hand-written storage.
    fn seed_provider(scratch: &Scratch, provider: &SlowProvider) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_phasegent"));
        command
            .args([
                "admin",
                "auth",
                "setup",
                "--stdin",
                "--provider",
                "redmine",
                "--api-base",
                provider.base.as_str(),
            ])
            .env("HOME", scratch.path("home"))
            .env("XDG_CONFIG_HOME", scratch.path("config"))
            .env("XDG_DATA_HOME", scratch.path("data"))
            .env("XDG_CACHE_HOME", scratch.path("cache"))
            .env("PHASEGENT_DB_PATH", scratch.path("phasegent.sqlite3"))
            .env("PHASEGENT_CONFIG_PATH", scratch.path("missing.toml"))
            .env_remove("PHASEGENT_ROLE")
            .env_remove("PHASEGENT_SESSION_ID")
            .env_remove("OPENCODE_SESSION_ID")
            .env_remove("CI")
            .env_remove("CONTINUOUS_INTEGRATION")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn auth setup");
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(b"pty-fixture-key\n")
            .expect("write fixture credential");
        let output = child.wait_with_output().expect("auth setup result");
        assert!(
            output.status.success(),
            "auth setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A human operator on a terminal reads text, and a slow provider request
    /// animates the progress line before the diagnostic replaces it.
    #[test]
    fn a_human_terminal_renders_text_and_animates_a_slow_request() {
        let scratch = Scratch::new("human-progress");
        let provider = SlowProvider::start(Duration::from_millis(600));
        seed_provider(&scratch, &provider);

        let run = run_pty(&scratch, None, &[], &["issue", "get", "1"]);

        assert_eq!(run.code, 1, "transcript:\n{}", run.transcript);
        assert!(
            !run.transcript.trim_start().starts_with('{'),
            "a human terminal resolves `auto` to text, not JSON:\n{}",
            run.transcript,
        );
        assert!(
            SPINNER_TICKS
                .iter()
                .any(|tick| run.transcript.contains(tick)),
            "the slow request must animate; transcript:\n{}",
            run.transcript,
        );
        assert!(
            plain(&run.transcript).contains("issue get"),
            "the progress names the running operation; transcript:\n{}",
            run.transcript,
        );
        assert!(
            plain(&run.transcript).contains("error:"),
            "the diagnostic still reaches the terminal; transcript:\n{}",
            run.transcript,
        );
        // Keep a marker for the erase-line sequence while dropping the color
        // styling, so the order of "erase the progress" and "print the error"
        // is visible in one normalized string.
        let visible = plain(&run.transcript.replace("\u{1b}[2K", "<CLEAR>"));
        let clear_at = visible
            .rfind("<CLEAR>")
            .expect("the progress line is cleared");
        let error_at = visible.rfind("error:").expect("the diagnostic");
        assert!(
            clear_at < error_at,
            "the progress line is cleared before the diagnostic; transcript:\n{}",
            run.transcript,
        );
        assert!(
            !visible[clear_at..error_at].contains("issue get"),
            "the progress text is erased before the diagnostic; transcript:\n{}",
            run.transcript,
        );
    }

    /// An explicit machine mode stays one clean document on a terminal.
    #[test]
    fn a_machine_output_on_a_terminal_stays_one_clean_document() {
        let scratch = Scratch::new("machine-document");
        let run = run_pty(&scratch, None, &[], &["--output", "json", "doctor"]);

        assert_eq!(run.code, 0, "transcript:\n{}", run.transcript);
        assert!(
            !run.transcript.contains('\u{1b}') && !has_redraw(&run.transcript),
            "the machine mode writes no control sequence or redraw:\n{:?}",
            run.transcript,
        );
        let value: serde_json::Value =
            serde_json::from_str(without_newline_translation(&run.transcript).trim())
                .expect("stdout is exactly one JSON document");
        assert!(value.get("database_path").is_some(), "got: {value}");
    }

    /// An agent role reads machine JSON even on a terminal, and no progress
    /// line ever appears for it.
    #[test]
    fn an_agent_role_on_a_terminal_reads_machine_json_without_progress() {
        let scratch = Scratch::new("agent-terminal");
        let provider = SlowProvider::start(Duration::from_millis(400));
        seed_provider(&scratch, &provider);

        let run = run_pty(&scratch, Some("executor"), &[], &["issue", "get", "1"]);

        assert_eq!(run.code, 1, "transcript:\n{}", run.transcript);
        assert!(
            !SPINNER_TICKS
                .iter()
                .any(|tick| run.transcript.contains(tick)),
            "an agent role never animates; transcript:\n{}",
            run.transcript,
        );
        assert!(
            !has_redraw(&run.transcript),
            "an agent role gets no redraw; transcript:\n{}",
            run.transcript,
        );
        let value: serde_json::Value =
            serde_json::from_str(without_newline_translation(&run.transcript).trim())
                .expect("an agent role reads one JSON document even on a terminal");
        assert!(value.get("error").is_some(), "got: {value}");
    }

    /// A slow successful `worktree prune` on a terminal animates, then clears
    /// the ticker before the text summary reaches stdout.
    #[test]
    fn a_slow_successful_prune_clears_the_progress_before_the_result() {
        let scratch = Scratch::new("prune-progress");
        let fixture = PruneFixture::new(&scratch);
        let bin = slow_opencode(&scratch, Duration::from_millis(600));
        let path = path_with(&bin);

        let run = run_pty(
            &scratch,
            None,
            &[("PATH", path.as_str())],
            &[
                "worktree",
                "prune",
                "--repo",
                fixture.repo.to_str().expect("fixture path"),
            ],
        );

        assert_eq!(run.code, 0, "transcript:\n{}", run.transcript);
        assert!(
            SPINNER_TICKS
                .iter()
                .any(|tick| run.transcript.contains(tick)),
            "the slow prune must animate; transcript:\n{}",
            run.transcript,
        );
        let visible = plain(&run.transcript.replace("\u{1b}[2K", "<CLEAR>"));
        let clear_at = visible
            .rfind("<CLEAR>")
            .expect("the progress line is cleared");
        let heading_at = visible
            .rfind("worktree prune (removal)")
            .expect("the text summary reaches stdout");
        assert!(
            clear_at < heading_at,
            "the ticker is cleared before the summary; transcript:\n{}",
            run.transcript,
        );
        assert!(
            !visible[clear_at..heading_at].contains("worktree prune:"),
            "no live stage overlaps the summary; transcript:\n{}",
            run.transcript,
        );
        assert!(
            visible.contains("kept "),
            "the kept directory keeps its row; transcript:\n{}",
            run.transcript,
        );
        assert!(
            !visible.trim_start().starts_with('{'),
            "the default on a terminal is text, not JSON; transcript:\n{}",
            run.transcript,
        );
    }

    /// The two legacy prune JSON aliases never animate on a human terminal,
    /// even though `auto` resolves to text there.
    #[test]
    fn a_legacy_json_prune_never_animates_on_a_human_terminal() {
        let scratch = Scratch::new("prune-legacy-json");
        let fixture = PruneFixture::new(&scratch);
        let bin = slow_opencode(&scratch, Duration::from_millis(600));
        let path = path_with(&bin);
        let repo = fixture.repo.to_str().expect("fixture path");

        for args in [
            &["worktree", "prune", "--repo", repo, "--json"][..],
            &["worktree", "prune", "--repo", repo, "--format", "json"][..],
        ] {
            let run = run_pty(&scratch, None, &[("PATH", path.as_str())], args);
            assert_eq!(run.code, 0, "{args:?} transcript:\n{}", run.transcript);
            assert!(
                !SPINNER_TICKS
                    .iter()
                    .any(|tick| run.transcript.contains(tick)),
                "{args:?} must not animate; transcript:\n{}",
                run.transcript,
            );
            assert!(
                !has_redraw(&run.transcript),
                "{args:?} must not redraw a terminal; transcript:\n{}",
                run.transcript,
            );
            let value: serde_json::Value =
                serde_json::from_str(without_newline_translation(&run.transcript).trim())
                    .expect("one JSON document on the terminal");
            assert_eq!(
                value
                    .get("totals")
                    .and_then(|t| t.get("kept_issue"))
                    .and_then(serde_json::Value::as_u64),
                Some(1),
                "{args:?} keeps the machine document: {value}",
            );
        }
    }

    /// The progress switches and a non-interactive terminal keep the line
    /// quiet without changing the result or the exit code.
    #[test]
    fn the_progress_gates_keep_the_terminal_quiet() {
        let scratch = Scratch::new("progress-gates");
        let provider = SlowProvider::start(Duration::from_millis(400));
        seed_provider(&scratch, &provider);

        for (label, role, extra_env, args) in [
            (
                "--no-progress",
                None,
                &[][..],
                &["--output", "text", "--no-progress", "issue", "get", "1"][..],
            ),
            (
                "CI",
                None,
                &[("CI", "1")][..],
                &["--output", "text", "issue", "get", "1"][..],
            ),
            (
                "TERM=dumb",
                None,
                &[("TERM", "dumb")][..],
                &["--output", "text", "issue", "get", "1"][..],
            ),
            (
                "agent role with explicit text",
                Some("orchestrator"),
                &[][..],
                &["--output", "text", "issue", "get", "1"][..],
            ),
        ] {
            let run = run_pty(&scratch, role, extra_env, args);
            assert_eq!(run.code, 1, "{label}: transcript:\n{}", run.transcript);
            assert!(
                !SPINNER_TICKS
                    .iter()
                    .any(|tick| run.transcript.contains(tick)),
                "{label} must not animate; transcript:\n{}",
                run.transcript,
            );
            assert!(
                plain(&run.transcript).contains("error:"),
                "{label} keeps the diagnostic; transcript:\n{}",
                run.transcript,
            );
        }
    }

    /// A successful human command on a terminal renders text and leaves the
    /// terminal without a control sequence once the run ends.
    #[test]
    fn a_successful_human_run_leaves_no_progress_residue() {
        let scratch = Scratch::new("clean-exit");
        let run = run_pty(&scratch, None, &[], &["doctor"]);

        assert_eq!(run.code, 0, "transcript:\n{}", run.transcript);
        assert!(
            plain(&run.transcript).contains("database:"),
            "a human terminal reads the curated text view:\n{}",
            run.transcript,
        );
        assert!(
            !has_redraw(&run.transcript),
            "a fast command leaves no redraw behind; transcript:\n{}",
            run.transcript,
        );
        assert!(
            plain(&run.transcript).contains("phasegent-output-pty-clean-exit"),
            "the run stays inside its scratch root; transcript:\n{}",
            plain(&run.transcript),
        );
        assert!(
            !SPINNER_TICKS
                .iter()
                .any(|tick| run.transcript.contains(tick)),
            "a fast command draws no progress frame;\ntranscript:\n{}",
            run.transcript,
        );
    }
}

#[cfg(not(unix))]
mod posix {
    /// The pty acceptance surface needs a POSIX pseudo-terminal. Other
    /// platforms keep the pipe-based coverage in `tests/cli_output_modes.rs`
    /// and report the limitation loudly instead of passing silently.
    #[test]
    fn the_posix_pty_acceptance_surface_is_unavailable() {
        eprintln!(
            "skipping the pty acceptance surface: this platform has no POSIX pseudo-terminal; \
             the pipe-based output coverage still runs in tests/cli_output_modes.rs"
        );
    }
}

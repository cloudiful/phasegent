//! Bounded execution of the installed `opencode api` client.
//!
//! The cleanup runs inside a post-close hook, so it must never hang the
//! close and must never buffer an unbounded response. `std` provides no
//! wait-with-timeout, so the child is spawned with piped stdout, drained
//! by a reader thread under a byte ceiling, and polled to exit within
//! [`COMMAND_TIMEOUT`]. Exceeding either bound kills the child and returns
//! an error, which the cleanup treats as uncertainty.
//!
//! stderr is redirected to null: the client writes a short `HTTP <code>`
//! line there and nothing this module needs, and an undrained pipe would
//! be a deadlock hazard for no benefit.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Wall-clock ceiling for one `opencode api` invocation. Long enough for
/// a cold service handshake, short enough that a wedged client cannot
/// stall an issue close.
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Ceiling on captured stdout. A full page of the largest documented
/// session listing fits comfortably; anything larger is treated as a
/// runaway response rather than buffered.
pub(super) const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;

/// Poll interval while waiting for the child to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) struct CommandOutput {
    pub(super) stdout: String,
    /// Child exit status. `opencode api` exits non-zero for every
    /// non-2xx response, so this is the only reliable HTTP signal on a
    /// stream that carries only the body.
    pub(super) status: i32,
}

/// Run `program` with `args` and return its stdout, bounded by
/// [`COMMAND_TIMEOUT`] and [`MAX_STDOUT_BYTES`]. The arguments are passed
/// as an argv array, so no value is ever interpreted by a shell.
pub(super) fn run_bounded(program: &Path, args: &[String]) -> Result<CommandOutput, String> {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", program.display()))?;
    let mut child = child;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout missing".to_owned())?;
    let overflowed = Arc::new(AtomicBool::new(false));
    let reader = {
        let overflowed = Arc::clone(&overflowed);
        thread::spawn(move || read_bounded(stdout, overflowed))
    };
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if overflowed.load(Ordering::Relaxed) {
            terminate(&mut child);
            return Err(format!(
                "{} produced more than {MAX_STDOUT_BYTES} bytes",
                program.display()
            ));
        }
        match child.try_wait() {
            Ok(Some(exit)) => break exit.code().unwrap_or(-1),
            Ok(None) => {}
            Err(error) => return Err(format!("could not wait for {}: {error}", program.display())),
        }
        if Instant::now() >= deadline {
            terminate(&mut child);
            return Err(format!(
                "{} did not finish within {}s",
                program.display(),
                COMMAND_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };
    let bytes = reader
        .join()
        .map_err(|_| "stdout reader panicked".to_owned())?
        .map_err(|error| format!("could not read {}: {error}", program.display()))?;
    let stdout = String::from_utf8(bytes)
        .map_err(|_| format!("{} produced non-UTF-8 output", program.display()))?;
    Ok(CommandOutput { stdout, status })
}

/// Read at most `MAX_STDOUT_BYTES` and raise the overflow flag instead of
/// growing further; the wait loop turns that flag into a kill.
fn read_bounded(mut stdout: impl Read, overflowed: Arc<AtomicBool>) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stdout.read(&mut chunk)?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes.len() + read > MAX_STDOUT_BYTES {
            overflowed.store(true, Ordering::Relaxed);
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn terminate(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

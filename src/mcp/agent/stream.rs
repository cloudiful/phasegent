//! Bounded framing for the ACP child's byte streams.
//!
//! Two problems the raw transport has and this module closes: a single
//! JSON-RPC line is unbounded, so a runaway agent could grow the
//! reader's buffer without limit; and a line that is not valid UTF-8
//! or not valid JSON is a frame the adapter must skip, not the end of
//! the stream. Closing a healthy session on one bad frame loses the
//! run silently.
//!
//! The child's stderr is drained for the same reason it is captured:
//! a handshake failure is only diagnosable with the agent's own
//! output, and an unread stderr pipe blocks the child once it fills.
//! It is captured, not trusted: the tail is redacted at this source, and
//! again at the sink in [`super::error::AgentError`], so agent output
//! can never carry a secret into a persisted or model-visible error.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};
use tokio::process::ChildStderr;

use super::redact;
use super::wire::IncomingMessage;

/// Hard cap on one framed message. ACP tool-call payloads are the
/// largest members; anything past this is refused rather than buffered.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Characters of child stderr retained for a failure message.
pub const MAX_STDERR_CHARS: usize = 2_000;

/// One framed line, decoded. `Ok(None)` means the stream ended.
pub enum Frame {
    Message(Box<IncomingMessage>),
    /// A line past [`MAX_LINE_BYTES`], or one that did not decode.
    Skipped,
    Eof,
}

/// Read one newline-delimited line, bounded, and decode it. Never
/// reports a decode failure as end-of-stream.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Frame> {
    let mut buffer = Vec::new();
    let read = {
        let mut limited = (&mut *reader).take((MAX_LINE_BYTES + 1) as u64);
        limited.read_until(b'\n', &mut buffer).await?
    };
    if read == 0 {
        return Ok(Frame::Eof);
    }
    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
        buffer.pop();
    }
    if buffer.len() > MAX_LINE_BYTES {
        return Ok(Frame::Skipped);
    }
    Ok(match serde_json::from_slice::<IncomingMessage>(&buffer) {
        Ok(message) => Frame::Message(Box::new(message)),
        Err(_) => Frame::Skipped,
    })
}

/// Bounded tail of the child's stderr, attached to spawn and handshake
/// failures so an agent-side crash is diagnosable.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<Tail>>);

#[derive(Default)]
struct Tail {
    text: String,
    truncated: bool,
}

impl StderrTail {
    /// The retained tail with secret-like spans replaced, or `None` when
    /// the child said nothing. This is the only way out of the tail, so
    /// the redaction cannot be bypassed by a new call site.
    pub fn text(&self) -> Option<String> {
        let guard = self.0.lock().expect("stderr tail lock");
        if guard.text.trim().is_empty() {
            return None;
        }
        let redacted = redact::redact(&guard.text);
        Some(if guard.truncated {
            format!("{redacted}...")
        } else {
            redacted
        })
    }
}

fn push_stderr(tail: &StderrTail, chunk: &[u8]) {
    let text = String::from_utf8_lossy(chunk);
    let mut guard = tail.0.lock().expect("stderr tail lock");
    if guard.truncated {
        return;
    }
    guard.text.push_str(&text);
    while guard.text.chars().count() > MAX_STDERR_CHARS {
        guard.text = guard.text.chars().skip(1_024).collect();
        guard.truncated = true;
    }
}

/// Spawn the task that keeps the child's stderr pipe drained and
/// retains its bounded tail.
pub fn drain_stderr(stderr: ChildStderr) -> (tokio::task::JoinHandle<()>, StderrTail) {
    let tail = StderrTail::default();
    let writer = tail.clone();
    let handle = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buffer = [0_u8; 4_096];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => push_stderr(&writer, &buffer[..read]),
            }
        }
    });
    (handle, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    fn notification(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": "s", "tag": tag}
        })
    }

    #[tokio::test]
    async fn frames_decode_and_strip_line_endings() {
        let input = format!("{}\n{}\r\n", notification("one"), notification("two"));
        let mut reader = BufReader::new(input.as_bytes());
        let Frame::Message(first) = read_frame(&mut reader).await.expect("read one") else {
            panic!("first frame must decode");
        };
        assert_eq!(first.params.as_ref().expect("params")["tag"], "one");
        let Frame::Message(second) = read_frame(&mut reader).await.expect("read two") else {
            panic!("second frame must decode");
        };
        assert_eq!(second.params.as_ref().expect("params")["tag"], "two");
        assert!(matches!(
            read_frame(&mut reader).await.expect("read eof"),
            Frame::Eof
        ));
    }

    #[tokio::test]
    async fn malformed_frames_are_skipped_not_treated_as_eof() {
        let mut input = Vec::new();
        input.extend_from_slice(b"{not json}\n");
        input.extend_from_slice(&[0xff, 0xfe, b'\n']);
        input.extend_from_slice(format!("{}\n", notification("alive")).as_bytes());
        let mut reader = BufReader::new(input.as_slice());

        assert!(matches!(
            read_frame(&mut reader).await.expect("read junk"),
            Frame::Skipped
        ));
        assert!(matches!(
            read_frame(&mut reader).await.expect("read non-utf8"),
            Frame::Skipped
        ));
        let Frame::Message(third) = read_frame(&mut reader).await.expect("read third") else {
            panic!("a bad frame must not end the session");
        };
        assert_eq!(third.params.as_ref().expect("params")["tag"], "alive");
    }

    #[test]
    fn stderr_tail_is_bounded_and_optional() {
        let tail = StderrTail::default();
        assert_eq!(tail.text(), None, "a silent child reports nothing");
        push_stderr(&tail, b"first line\n");
        assert_eq!(tail.text().as_deref(), Some("first line\n"));
        push_stderr(&tail, &vec![b'x'; MAX_STDERR_CHARS * 2]);
        let bounded = tail.text().expect("tail");
        assert!(bounded.ends_with("..."));
        assert!(bounded.chars().count() <= MAX_STDERR_CHARS + 3);
    }

    #[test]
    fn a_secret_split_across_reads_never_leaves_the_tail() {
        // A credential straddling two pipe reads is the case a
        // per-chunk scrub would miss, because the tail is redacted once,
        // as a whole.
        let tail = StderrTail::default();
        push_stderr(&tail, b"dial failed: Authorization: Bearer sk-mini");
        push_stderr(&tail, b"map-0123456789abcdef\n");
        let text = tail.text().expect("tail");
        assert!(!text.contains("sk-minimap-0123456789abcdef"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
        assert!(text.contains("dial failed"), "{text}");
    }
}

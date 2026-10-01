//! Newline-delimited JSON-RPC framing over the ACP child process.
//!
//! The [`Connection`] owns the child's stdin (via a writer task fed by
//! an mpsc channel) and dispatches the child's stdout line by line:
//! responses resolve pending request slots, agent requests are answered
//! through the shared outbound channel, and notifications are
//! forwarded. Nothing here interprets ACP semantics; the session layer
//! does.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{Mutex, mpsc, oneshot};

use super::error::{AgentError, AgentResult, sanitize};
use super::stream::{Frame, read_frame};
use super::wire::{
    IncomingMessage, error_response_json, notification_json, request_json, response_json,
};

/// JSON-RPC error code for a method this client does not implement.
const METHOD_NOT_FOUND: i64 = -32601;

/// Handles an agent-initiated request and produces the response result
/// payload; `Err` becomes a JSON-RPC error response. Must never panic.
pub type AgentRequestCallback = Arc<
    dyn Fn(&str, Option<&serde_json::Value>) -> Result<serde_json::Value, (i64, String)>
        + Send
        + Sync,
>;

/// Handles one agent notification.
pub type NotificationCallback = Arc<dyn Fn(&IncomingMessage) + Send + Sync>;

struct PendingSlot {
    sender: oneshot::Sender<Result<serde_json::Value, AgentError>>,
}

#[derive(Default)]
struct PendingMap {
    slots: HashMap<i64, PendingSlot>,
    /// Set once the reader loop ends; `request` checks it after
    /// inserting so a slot can never outlive the reader.
    closed: bool,
}

#[derive(Clone)]
pub(crate) struct Connection {
    outbound: mpsc::Sender<String>,
    pending: Arc<Mutex<PendingMap>>,
    /// Aborting ends the writer task, closing the write half even
    /// though the reader task holds a channel clone.
    writer_abort: tokio::task::AbortHandle,
}

/// Everything the reader loop and the request path share.
#[derive(Clone)]
struct Loop {
    outbound: mpsc::Sender<String>,
    pending: Arc<Mutex<PendingMap>>,
}

impl Connection {
    /// Wrap a spawned child: start the stdin writer task and return
    /// the connection plus the reader task that must be awaited after
    /// the child exits.
    pub fn start(
        child: &mut Child,
        callbacks: (AgentRequestCallback, NotificationCallback),
    ) -> Result<(Self, tokio::task::JoinHandle<()>), AgentError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::spawn("mcode acp has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::spawn("mcode acp has no stdout"))?;
        Ok(Self::start_over(stdout, stdin, callbacks))
    }

    /// Wrap arbitrary halves (child stdio or an in-process duplex in
    /// tests). The halves move into the writer/reader tasks.
    pub(crate) fn start_over(
        reader: impl tokio::io::AsyncRead + Send + Unpin + 'static,
        writer: impl tokio::io::AsyncWrite + Send + Unpin + 'static,
        callbacks: (AgentRequestCallback, NotificationCallback),
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (outbound, outbound_rx) = mpsc::channel::<String>(64);
        let writer_abort = tokio::spawn(writer_task(outbound_rx, Box::new(writer))).abort_handle();
        let pending = Arc::new(Mutex::new(PendingMap::default()));
        let loop_state = Loop {
            outbound: outbound.clone(),
            pending: pending.clone(),
        };
        let reader_task = tokio::spawn(reader_task(
            loop_state,
            BufReader::new(Box::new(reader) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            callbacks.0,
            callbacks.1,
        ));
        (
            Self {
                outbound,
                pending,
                writer_abort,
            },
            reader_task,
        )
    }

    /// End the writer task, close the transport's write half, and fail
    /// every outstanding request. The far side then sees EOF and
    /// exits. Failing the pending slots here is what makes teardown
    /// prompt instead of leaving a caller waiting for a reply that can
    /// no longer arrive.
    pub(crate) async fn close(&self) {
        self.writer_abort.abort();
        let mut slots = self.pending.lock().await;
        slots.closed = true;
        for (_, slot) in slots.slots.drain() {
            let _ = slot.sender.send(Err(AgentError::closed()));
        }
    }

    /// Send one request and await its response.
    pub async fn request(
        &self,
        next_id: impl Fn() -> i64,
        method: &str,
        params: serde_json::Value,
    ) -> AgentResult<serde_json::Value> {
        let id = next_id();
        let (sender, receiver) = oneshot::channel();
        {
            let mut slots = self.pending.lock().await;
            if slots.closed {
                return Err(AgentError::closed());
            }
            if slots.slots.len() >= MAX_PENDING_REQUESTS {
                return Err(AgentError::protocol("too many outstanding acp requests"));
            }
            slots.slots.insert(id, PendingSlot { sender });
        }
        if self
            .outbound
            .send(request_json(id, method, params))
            .await
            .is_err()
        {
            self.pending.lock().await.slots.remove(&id);
            return Err(AgentError::closed());
        }
        match receiver.await {
            Ok(outcome) => outcome,
            // Reader gone (process died): every pending request fails.
            Err(_) => Err(AgentError::closed()),
        }
    }

    /// Send one notification (no response expected).
    pub async fn notify(&self, method: &str, params: serde_json::Value) -> AgentResult<()> {
        self.outbound
            .send(notification_json(method, params))
            .await
            .map_err(|_| AgentError::closed())
    }
}

/// Bound on simultaneously outstanding requests. The adapter drives
/// one prompt at a time per process, so this only guards protocol
/// anomalies.
pub(crate) const MAX_PENDING_REQUESTS: usize = 16;

async fn writer_task(
    mut rx: mpsc::Receiver<String>,
    mut stdin: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
) {
    while let Some(line) = rx.recv().await {
        let ok = stdin.write_all(line.as_bytes()).await.is_ok()
            && stdin.write_all(b"\n").await.is_ok()
            && stdin.flush().await.is_ok();
        if !ok {
            break;
        }
    }
}

async fn reader_task(
    state: Loop,
    mut reader: BufReader<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
    agent_requests: AgentRequestCallback,
    notifications: NotificationCallback,
) {
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            // A read error ends the stream; there is nothing to retry.
            Err(_) => break,
        };
        // A malformed or oversized line is skipped, not treated as the
        // end of the session: one bad frame must not silently discard a
        // live run.
        let message = match frame {
            Frame::Message(message) => message,
            Frame::Skipped => continue,
            Frame::Eof => break,
        };
        match (&message.method, &message.id) {
            (None, Some(_)) => resolve_response(&state.pending, &message).await,
            (Some(method), Some(id)) => {
                let outcome = agent_requests(method, message.params.as_ref());
                let payload = match outcome {
                    Ok(result) => response_json(id, result),
                    Err((code, text)) => error_response_json(id, code, &sanitize(&text)),
                };
                if state.outbound.send(payload).await.is_err() {
                    break;
                }
            }
            (Some(_), None) => notifications(&message),
            (None, None) => {}
        }
    }
    let mut slots = state.pending.lock().await;
    slots.closed = true;
    for (_, slot) in slots.slots.drain() {
        let _ = slot.sender.send(Err(AgentError::closed()));
    }
}

async fn resolve_response(pending: &Arc<Mutex<PendingMap>>, message: &IncomingMessage) {
    let Some(id) = message.id.as_ref().and_then(serde_json::Value::as_i64) else {
        return;
    };
    let slot = pending.lock().await.slots.remove(&id);
    if let Some(slot) = slot {
        let outcome = match &message.error {
            // Agent-authored text: the child legitimately holds the
            // model's credential, so `AgentError::new` redacts this span
            // the same way it redacts the stderr tail.
            Some(error) => Err(AgentError::protocol(format!(
                "acp error {}: {}",
                error.code, error.message
            ))),
            None => Ok(message.result.clone().unwrap_or(serde_json::Value::Null)),
        };
        let _ = slot.sender.send(outcome);
    }
}

/// Unknown agent-initiated methods are refused without pretending to
/// implement them.
pub fn method_not_found_error(method: &str) -> (i64, String) {
    (
        METHOD_NOT_FOUND,
        format!("client does not implement {method}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_after_close_fails_with_closed() {
        // A connection whose writer channel is already dropped.
        let (outbound, receiver) = mpsc::channel::<String>(1);
        drop(receiver);
        let connection = Connection {
            outbound,
            pending: Arc::new(Mutex::new(PendingMap {
                slots: Default::default(),
                closed: false,
            })),
            writer_abort: tokio::spawn(async {}).abort_handle(),
        };
        let error = connection
            .request(|| 1, "session/prompt", serde_json::json!({}))
            .await
            .expect_err("a closed transport must fail the request");
        assert_eq!(error.kind.as_str(), "closed");
    }

    #[test]
    fn unknown_agent_methods_report_method_not_found() {
        let (code, message) = method_not_found_error("fs/write_text_file");
        assert_eq!(code, -32601);
        assert!(message.contains("fs/write_text_file"));
    }
}

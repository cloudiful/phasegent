//! A loopback identity broker for the CLI integration tests (issue #22 P1).
//!
//! The CLI verifies a managed session against the host broker before it
//! trusts any agent role. The tests need a real broker on the loopback
//! interface: this one speaks the same versioned newline-delimited JSON
//! protocol, holds one token bound to one session, and answers the stored
//! role for that token only.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// The environment variable that carries the broker endpoint.
pub const BROKER_ADDR_ENV: &str = "PHASEGENT_BROKER_ADDR";
/// The environment variable that carries the opaque broker token.
pub const BROKER_TOKEN_ENV: &str = "PHASEGENT_BROKER_TOKEN";

/// A running test broker. It answers every verification request for its
/// own token and session and refuses everything else.
pub struct TestBroker {
    addr: String,
    token: String,
    session: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestBroker {
    /// Start a broker that reports `role` for the given session.
    pub fn start(role: &str, session: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the test broker");
        let addr = listener.local_addr().expect("the test broker address");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let token = format!("test-token-{role}-{session}");
        let thread_token = token.clone();
        let thread_session = session.to_owned();
        let thread_role = role.to_owned();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { break };
                serve(stream, &thread_token, &thread_session, &thread_role);
            }
        });
        Self {
            addr: format!("127.0.0.1:{}", addr.port()),
            token,
            session: session.to_owned(),
            stop,
            handle: Some(handle),
        }
    }

    /// The `host:port` endpoint of the broker.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// The opaque token of the broker.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The session the broker answers for.
    pub fn session(&self) -> &str {
        &self.session
    }
}

impl Drop for TestBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop with one throwaway connection.
        if let Ok(stream) = TcpStream::connect(&self.addr) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Answer one verification request. The request never carries a role: the
/// broker replies with the role it stored for the presented token, and only
/// when the session matches too.
fn serve(mut stream: TcpStream, token: &str, session: &str, role: &str) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let request = serde_json::from_str::<serde_json::Value>(line.trim()).unwrap_or_default();
    let presented_token = request
        .get("token")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let presented_session = request
        .get("session")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let reply = if presented_token == token && presented_session == session {
        serde_json::json!({"v":1,"ok":true,"role":role,"session":session})
    } else {
        serde_json::json!({"v":1,"ok":false,"error":"unknown token"})
    };
    let _ = writeln!(stream, "{reply}");
    let _ = stream.flush();
}

//! Inter-project message bus over Unix domain sockets.
//!
//! Why: Multiple trusty-agents projects running concurrently need a lightweight
//! IPC channel to share signals (e.g., "agent A finished", "memory updated")
//! without going through a central server. Unix domain sockets give us
//! low-latency, in-process-group communication with no port conflicts.
//! What: `MessageBus` owns a `UnixListener` at `~/.trusty-agents/sockets/<id>.sock`
//! and a `broadcast::Sender<BusEnvelope>`. An `accept_loop` task reads
//! NDJSON lines from each incoming connection and re-broadcasts them to all
//! subscribers in the same process. `send_to` connects to a peer's socket
//! and writes one NDJSON line through `trusty_common::uds`'s shared framing
//! entry point (#5180) — one way, no reply.
//! Test: Start a bus, connect a raw UnixStream, write a JSON line, subscribe
//! and assert the envelope is received.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

/// Capacity of the broadcast channel (number of in-flight envelopes buffered).
///
/// Why: Large enough that slow subscribers don't cause the channel to drop
/// messages under normal loads, but bounded to avoid unbounded memory growth.
const CHANNEL_CAPACITY: usize = 256;

/// NDJSON envelope wrapping an inner IPC payload with routing metadata.
///
/// Why: Adds source/target routing on top of the existing inner NDJSON
/// payload format without requiring changes to existing message types.
/// What: `source_project` identifies the sender; `target_project = None`
/// means broadcast to all listeners. `message` is the raw JSON payload
/// (typically an existing `IpcMessage` serialized).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BusEnvelope {
    /// Basename of the project directory that sent this message.
    pub source_project: String,
    /// Target project id, or `None` for broadcast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_project: Option<String>,
    /// Inner NDJSON payload (arbitrary JSON value).
    #[serde(flatten)]
    pub message: serde_json::Value,
}

/// Per-project Unix-domain-socket message bus.
///
/// Why: Each project gets its own socket so peer discovery is just listing
/// `~/.trusty-agents/sockets/*.sock` and connection is a simple path connect.
/// What: Holds the socket path, project id, and a broadcast sender.
/// The internal `accept_loop` runs as a detached tokio task.
pub struct MessageBus {
    socket_path: PathBuf,
    project_id: String,
    tx: broadcast::Sender<BusEnvelope>,
}

impl MessageBus {
    /// Start the bus: create the socket directory, bind the listener, and
    /// spawn the accept loop as a background task.
    ///
    /// Why: A single `start` call encapsulates socket setup and background
    /// task spawning so callers don't have to manage the lifecycle themselves.
    /// What: Returns an `Arc<Self>` so the bus can be shared across tokio tasks.
    /// Removes any stale socket file at the target path before binding.
    /// Test: Call `start`, connect a raw `UnixStream`, write a line, subscribe
    /// and assert the envelope arrives.
    pub async fn start(project_id: &str) -> Result<Arc<Self>> {
        let socket_path = Self::socket_path_for(project_id)?;
        // Remove stale socket from a previous run.
        if socket_path.exists() {
            fs::remove_file(&socket_path).await.ok();
        }

        // #5099: was `create_dir_all` + a bare bind, so both the sockets
        // directory and the socket landed at the process umask.
        let listener = trusty_common::uds::bind_hardened(&socket_path)?;
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);

        let bus = Arc::new(Self {
            socket_path,
            project_id: project_id.to_string(),
            tx: tx.clone(),
        });

        tokio::spawn(accept_loop(listener, tx));
        tracing::info!(project_id = %project_id, "message bus started");
        Ok(bus)
    }

    /// Remove the socket file on graceful shutdown.
    ///
    /// Why: Leaving stale socket files causes `list_running` to report dead
    /// projects as alive until the OS cleans them up.
    /// What: Best-effort `fs::remove_file`; logs a warning on failure but
    /// does not propagate the error.
    /// Test: Call `start` then `shutdown`; assert the socket file is gone.
    pub async fn shutdown(&self) -> Result<()> {
        if let Err(e) = fs::remove_file(&self.socket_path).await {
            tracing::warn!(path = %self.socket_path.display(), error = %e, "bus: failed to remove socket");
        }
        tracing::info!(project_id = %self.project_id, "message bus shutdown");
        Ok(())
    }

    /// Send a message to another project by connecting to its socket.
    ///
    /// Why: `send_to` abstracts the connect-write-close pattern so callers
    /// don't duplicate socket path resolution and NDJSON framing.
    /// What: Resolves the target's socket path, dials it through
    /// [`trusty_common::uds::connect_hardened`], writes one NDJSON-framed
    /// `BusEnvelope` line, and flushes. #5089: a target socket that is not
    /// `0600`-and-self-owned in a `0700` directory is refused, not written to.
    /// Test: `send_envelope_to_delivers_over_a_hardened_socket`,
    /// `send_envelope_to_refuses_a_world_readable_socket`.
    pub async fn send_to(&self, target_project: &str, payload: serde_json::Value) -> Result<()> {
        let target_path = Self::socket_path_for(target_project)?;
        let envelope = BusEnvelope {
            source_project: self.project_id.clone(),
            target_project: Some(target_project.to_string()),
            message: payload,
        };
        send_envelope_to(&target_path, &envelope).await
    }

    /// Subscribe to all incoming messages on this bus.
    ///
    /// Why: Multiple consumers (workflow engine, CTRL-CLI, status poller) need
    /// independent subscriptions without blocking each other.
    /// What: Returns a `broadcast::Receiver` that receives every envelope
    /// delivered to this bus, including messages from peer projects.
    /// Test: Subscribe, send a message via `send_to`, assert receiver gets it.
    pub fn subscribe(&self) -> broadcast::Receiver<BusEnvelope> {
        self.tx.subscribe()
    }

    /// Return the canonical socket path for `project_id`.
    ///
    /// Why: Centralizes path construction so `send_to` and `list_running`
    /// agree on the naming convention.
    /// What: Returns `~/.trusty-agents/sockets/<project_id>.sock`.
    /// Test: Assert the returned path ends with `<id>.sock`.
    pub fn socket_path_for(project_id: &str) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        Ok(home
            .join(".trusty-agents")
            .join("sockets")
            .join(format!("{project_id}.sock")))
    }

    /// List project ids of currently running buses by scanning the socket dir.
    ///
    /// Why: Lets callers discover peers without a registry lookup; a live
    /// socket file is the most reliable signal of a running process.
    /// What: Reads `~/.trusty-agents/sockets/`, returns the stem of every
    /// `*.sock` file that is connectable. Stale sockets are skipped, and since
    /// #5089 so are sockets that fail the permission check — `send_to` would
    /// refuse them, so they are not reachable peers.
    /// Test: `list_running_in_reports_a_hardened_socket`,
    /// `list_running_in_skips_a_world_readable_socket`.
    pub async fn list_running() -> Result<Vec<String>> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        list_running_in(&home.join(".trusty-agents").join("sockets")).await
    }

    /// Return the socket path for this bus instance.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

/// Wall-clock ceiling for one bus send.
///
/// Why (#5180): before the migration this path had no bound — a peer whose
/// socket receive buffer was full (an accept loop that stopped draining, a
/// process stopped under a debugger) blocked the sender's `write_all`
/// indefinitely, and `send_to` is called from request-handling paths. The
/// payload is one small envelope to a local socket, so anything that has not
/// completed in 10 s is not going to.
/// Test: `send_envelope_to_writes_exactly_one_newline_terminated_frame`.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Dial `target_path` and write one NDJSON-framed envelope to it.
///
/// Why: split out of [`MessageBus::send_to`] so the dial half is reachable from
/// a test without writing into the real `~/.trusty-agents/sockets/`, which
/// `socket_path_for` hardcodes.
/// What: #5180 — routes through
/// [`trusty_common::uds::send_framed_notification`], the shared framing entry
/// point, which dials (verifying the socket per #5089), writes one
/// newline-terminated frame, flushes and half-closes. This is a NOTIFICATION,
/// not a request: [`accept_loop`] re-broadcasts the envelope to in-process
/// subscribers and never writes a reply, so `send_framed_request` would block
/// until its timeout and then report a failure for a delivery that succeeded.
/// Test: `send_envelope_to_delivers_over_a_hardened_socket`,
/// `send_envelope_to_writes_exactly_one_newline_terminated_frame`,
/// `send_envelope_to_refuses_a_world_readable_socket`.
async fn send_envelope_to(target_path: &Path, envelope: &BusEnvelope) -> Result<()> {
    trusty_common::uds::send_framed_notification(target_path, envelope, SEND_TIMEOUT).await?;
    Ok(())
}

/// Stems of every connectable `*.sock` in `sockets_dir`.
///
/// Why: split out of [`MessageBus::list_running`] for the same reason as
/// [`send_envelope_to`] — the public entry point is pinned to the home
/// directory, so a test that exercised it would see (and be perturbed by) the
/// developer's live buses.
/// What: returns an empty vec when the directory is absent; otherwise probes
/// each `*.sock` entry and keeps the stems that answer.
/// Test: `list_running_in_reports_a_hardened_socket`,
/// `list_running_in_skips_a_world_readable_socket`.
async fn list_running_in(sockets_dir: &Path) -> Result<Vec<String>> {
    if !sockets_dir.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut entries = fs::read_dir(sockets_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        // Probe liveness with a quick connect attempt. #5089: a socket that
        // fails verification is one `send_to` would refuse anyway, so it is
        // not a running peer as far as this list is concerned.
        if trusty_common::uds::connect_hardened(&path).await.is_ok()
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            result.push(stem.to_string());
        }
    }
    Ok(result)
}

/// Test helper: bind a one-off listener at `socket_path`, accept the next
/// inbound connection, and drain NDJSON `BusEnvelope` lines for up to
/// `timeout` before returning what we got.
///
/// Why: Tests for PM-to-PM relay need to assert "the envelope arrived"
/// without juggling broadcast subscriptions and tokio runtime lifetimes.
/// What: Removes any stale socket file at `socket_path`, binds a fresh
/// `UnixListener`, spawns a single-connection reader, and returns up to
/// `timeout` worth of envelopes. Returns whatever was drained when the
/// deadline elapses, the connection closes, or a parse error occurs.
/// Test: Used by integration tests; not exercised by `bus` unit tests.
#[cfg(test)]
#[allow(dead_code)]
pub async fn subscribe_and_drain(
    socket_path: &Path,
    timeout: std::time::Duration,
) -> Vec<BusEnvelope> {
    if socket_path.exists() {
        let _ = fs::remove_file(socket_path).await;
    }
    // #5089: bind the way the real bus does. A bare `create_dir_all` + bind
    // leaves the stub at the process umask, which `send_to` now refuses to
    // dial — the stub daemon would be unreachable from the code under test.
    let listener = match trusty_common::uds::bind_hardened(socket_path) {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };
    let drained = tokio::time::timeout(timeout, async move {
        let mut out: Vec<BusEnvelope> = Vec::new();
        if let Ok((stream, _)) = listener.accept().await {
            let reader = BufReader::new(stream);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<BusEnvelope>(&line) {
                    Ok(env) => out.push(env),
                    Err(_) => break,
                }
            }
        }
        out
    })
    .await;
    drained.unwrap_or_default()
}

/// Background task: accept connections, read NDJSON lines, broadcast envelopes.
///
/// Why: Runs independently of the main task so accepting new connections does
/// not block message delivery or application logic.
/// What: Loops forever accepting `UnixStream` connections; each connection is
/// handled in its own spawned task that reads lines until EOF, deserializes
/// each as `BusEnvelope`, and sends it on the broadcast channel. Lagged
/// receivers are silently ignored.
/// Test: Implicit via `start` integration test.
async fn accept_loop(listener: UnixListener, tx: broadcast::Sender<BusEnvelope>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // #5099: a foreign-uid peer is dropped without being served.
                if let Err(e) = trusty_common::uds::ensure_peer_is_self(&stream) {
                    tracing::warn!(error = %e, "bus accept_loop: rejected connection");
                    continue;
                }
                let tx_clone = tx.clone();
                tokio::spawn(handle_connection(stream, tx_clone));
            }
            Err(e) => {
                tracing::warn!(error = %e, "bus accept_loop: listener error");
                // Short back-off to avoid tight error loops.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Read NDJSON lines from `stream` and broadcast each as a `BusEnvelope`.
///
/// Why: Each connection is isolated so a malformed message from one peer
/// doesn't kill the whole accept loop.
/// What: Reads lines until EOF; parses each as JSON into `BusEnvelope`.
/// Lines that fail to parse are logged and skipped.
async fn handle_connection(stream: UnixStream, tx: broadcast::Sender<BusEnvelope>) {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BusEnvelope>(&line) {
            Ok(envelope) => {
                // Ignore send errors (no subscribers is fine).
                let _ = tx.send(envelope);
            }
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "bus: malformed envelope; skipping");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn envelope() -> BusEnvelope {
        BusEnvelope {
            source_project: "sender".to_string(),
            target_project: Some("receiver".to_string()),
            message: serde_json::json!({"type": "ping"}),
        }
    }

    /// Widen a bound socket to `0666`, the shape a pre-#5099 daemon left behind.
    fn widen(path: &Path) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
            .expect("widen socket");
    }

    /// The normal path still works: a socket bound through `bind_hardened`
    /// accepts the envelope and the bytes arrive intact.
    #[tokio::test]
    async fn send_envelope_to_delivers_over_a_hardened_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("receiver.sock");
        let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");

        let reader = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut lines = BufReader::new(stream).lines();
            lines.next_line().await.expect("read").expect("one line")
        });

        send_envelope_to(&sock, &envelope()).await.expect("send");

        let line = reader.await.expect("join");
        let got: BusEnvelope = serde_json::from_str(&line).expect("parse");
        assert_eq!(got.source_project, "sender");
    }

    /// #5180 wire-shape guard: the bus is NDJSON, so a peer's `lines()` reader
    /// desynchronises for the rest of the connection if a send ever emits zero
    /// or two terminators, or prepends a length prefix. Reading `lines()` (as
    /// the test above does) cannot see that — it normalises the framing away.
    /// This one asserts the raw bytes.
    #[tokio::test]
    async fn send_envelope_to_writes_exactly_one_newline_terminated_frame() {
        use tokio::io::AsyncReadExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("receiver.sock");
        let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");

        let reader = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.expect("read to eof");
            raw
        });

        send_envelope_to(&sock, &envelope()).await.expect("send");

        let raw = reader.await.expect("join");
        let text = String::from_utf8(raw).expect("utf8");
        assert!(
            text.ends_with('\n'),
            "the frame must be newline-terminated: {text:?}"
        );
        assert_eq!(
            text.matches('\n').count(),
            1,
            "exactly one terminator per envelope: {text:?}"
        );
        let expected = serde_json::to_string(&envelope()).expect("serialise");
        assert_eq!(
            text.trim_end_matches('\n'),
            expected,
            "the frame body must be the bare serialised envelope, no prefix"
        );
    }

    /// #5089 step 1a: `send_to` writes a payload, so it must refuse a socket
    /// that fails the permission check rather than trusting whatever answers.
    /// Against the pre-migration bare `UnixStream::connect` this passes the
    /// write straight through to a `0666` socket.
    #[tokio::test]
    async fn send_envelope_to_refuses_a_world_readable_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("receiver.sock");
        // Keep the listener alive, so a bare connect would succeed.
        let _listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
        widen(&sock);

        let err = send_envelope_to(&sock, &envelope())
            .await
            .expect_err("must refuse a socket that is not 0600");
        assert!(
            err.to_string().contains("refusing to connect"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn list_running_in_reports_a_hardened_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("sockets");
        let sock = dir.join("alive.sock");
        let _listener = trusty_common::uds::bind_hardened(&sock).expect("bind");

        let running = list_running_in(&dir).await.expect("list");
        assert_eq!(running, vec!["alive".to_string()]);
    }

    /// #5089 step 1a: a socket that fails verification is not a peer we would
    /// ever send to, so it must not be reported as running either.
    #[tokio::test]
    async fn list_running_in_skips_a_world_readable_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("sockets");
        let sock = dir.join("wide.sock");
        let _listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
        widen(&sock);

        let running = list_running_in(&dir).await.expect("list");
        assert!(
            running.is_empty(),
            "a 0666 socket must not be reported running: {running:?}"
        );
    }
}

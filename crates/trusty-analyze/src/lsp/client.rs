//! JSON-RPC over an LSP byte pipe: request/response correlation, per-request
//! timeouts, server-initiated requests and published diagnostics (#9078).
//!
//! Why: a language server answers out of order, sends its own requests, and
//! pushes diagnostics whenever it likes. One reader task must route every
//! inbound message, or a slow answer blocks a fast one.
//! What: [`LspClient`] owns the write half and a reader task over the read
//! half. Each request registers a waiter keyed by a fresh integer id; the
//! reader completes it. A timed-out waiter is removed and the request is
//! cancelled, so a late answer is dropped by id and cannot reach the next call.
//! When the stream closes or loses its framing, every waiter and every later
//! call fails with the same typed reason.
//! Test: `client_correlates_responses_that_arrive_out_of_order`,
//! `late_response_after_timeout_does_not_desync`,
//! `peer_closing_mid_request_is_an_error_not_a_hang`,
//! `write_to_a_dead_peer_is_an_error`, `server_initiated_requests_are_answered`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::documents::DocumentSet;
use super::framing::{encode, FrameDecoder};
use super::{lock, LspError};

type Writer = tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>;
type Waiter = oneshot::Sender<Result<Value, LspError>>;

/// The latest `textDocument/publishDiagnostics` for one document.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedDiagnostics {
    /// The document version the server computed against, when it said.
    pub version: Option<i64>,
    /// The raw LSP `Diagnostic` objects.
    pub diagnostics: Vec<Value>,
}

#[derive(Default)]
struct Table {
    next_id: i64,
    waiters: HashMap<i64, Waiter>,
    closed: Option<LspError>,
}

struct Shared {
    table: Mutex<Table>,
    diagnostics: Mutex<HashMap<String, PublishedDiagnostics>>,
    /// Bumped on every publication and on close, to wake diagnostic waiters.
    changes: watch::Sender<u64>,
    consecutive_timeouts: AtomicU32,
}

impl Shared {
    /// Fail every waiter and every later call with `reason` (first reason wins).
    fn close(&self, reason: LspError) {
        let (waiters, reason) = {
            let mut t = lock(&self.table);
            let reason = t.closed.get_or_insert(reason).clone();
            (std::mem::take(&mut t.waiters), reason)
        };
        for (_, w) in waiters {
            let _ = w.send(Err(reason.clone()));
        }
        self.changes.send_modify(|n| *n += 1);
    }

    fn closed_reason(&self) -> Option<LspError> {
        lock(&self.table).closed.clone()
    }
}

/// A JSON-RPC client over one LSP byte pipe.
///
/// Why/What: see the module doc. Dropping the client stops its reader task.
/// Test: `client_correlates_responses_that_arrive_out_of_order`.
pub struct LspClient {
    shared: Arc<Shared>,
    writer: Arc<Writer>,
    reader: JoinHandle<()>,
    pub(super) documents: DocumentSet,
}

impl LspClient {
    /// Start a client over `reader` (server stdout) and `writer` (server stdin).
    ///
    /// Must run inside a tokio runtime: it spawns the reader task.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (changes, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            table: Mutex::new(Table {
                next_id: 1,
                ..Table::default()
            }),
            diagnostics: Mutex::new(HashMap::new()),
            changes,
            consecutive_timeouts: AtomicU32::new(0),
        });
        let writer: Arc<Writer> = Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
        let reader = tokio::spawn(read_loop(reader, Arc::clone(&shared), Arc::clone(&writer)));
        Self {
            shared,
            writer,
            reader,
            documents: DocumentSet::default(),
        }
    }

    /// Send a request and wait at most `timeout` for its answer.
    ///
    /// # Errors
    /// [`LspError::Timeout`] (the request is then cancelled with
    /// `$/cancelRequest`), [`LspError::ServerError`], [`LspError::Write`], or the
    /// close reason ([`LspError::ServerExited`] / [`LspError::Protocol`]).
    ///
    /// Test: `late_response_after_timeout_does_not_desync`,
    /// `server_error_response_is_a_typed_error`.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut t = lock(&self.shared.table);
            if let Some(reason) = &t.closed {
                return Err(reason.clone());
            }
            let id = t.next_id;
            t.next_id += 1;
            t.waiters.insert(id, tx);
            id
        };
        let mut msg = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if !params.is_null() {
            msg["params"] = params;
        }
        if let Err(e) = write_frame(&self.writer, &msg).await {
            lock(&self.shared.table).waiters.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(self.closed_reason().unwrap_or(LspError::ServerExited {
                detail: "the reader stopped".into(),
            })),
            Err(_) => {
                // #9078: drop the waiter so a late answer is discarded by id.
                lock(&self.shared.table).waiters.remove(&id);
                self.shared
                    .consecutive_timeouts
                    .fetch_add(1, Ordering::SeqCst);
                let _ = self.notify("$/cancelRequest", json!({"id": id})).await;
                Err(LspError::Timeout {
                    method: method.to_string(),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                })
            }
        }
    }

    /// Send a notification (no answer expected).
    ///
    /// # Errors
    /// The close reason, or [`LspError::Write`].
    ///
    /// Test: `write_to_a_dead_peer_is_an_error`.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let mut msg = json!({"jsonrpc": "2.0", "method": method});
        if !params.is_null() {
            msg["params"] = params;
        }
        write_frame(&self.writer, &msg).await
    }

    /// Why the stream closed, if it has.
    pub fn closed_reason(&self) -> Option<LspError> {
        self.shared.closed_reason()
    }

    /// Whether the stream has closed; every call then fails at once.
    pub fn is_closed(&self) -> bool {
        self.closed_reason().is_some()
    }

    /// Requests in a row that timed out; any answer resets it.
    pub fn consecutive_timeouts(&self) -> u32 {
        self.shared.consecutive_timeouts.load(Ordering::SeqCst)
    }

    /// The latest diagnostics published for `uri`.
    pub fn diagnostics(&self, uri: &str) -> Option<PublishedDiagnostics> {
        lock(&self.shared.diagnostics).get(uri).cloned()
    }

    /// Wait until the server publishes diagnostics for `uri` at `min_version`
    /// or later.
    ///
    /// An unversioned publication never satisfies the wait: it cannot prove
    /// which document state it describes (DOC-72 §3).
    ///
    /// # Errors
    /// [`LspError::Timeout`] past `timeout`, or the close reason.
    ///
    /// Test: `published_diagnostics_are_collected_by_version`.
    pub async fn wait_for_diagnostics(
        &self,
        uri: &str,
        min_version: i64,
        timeout: Duration,
    ) -> Result<PublishedDiagnostics, LspError> {
        let mut changes = self.shared.changes.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(d) = self.diagnostics(uri) {
                if d.version.is_some_and(|v| v >= min_version) {
                    return Ok(d);
                }
            }
            if let Some(reason) = self.closed_reason() {
                return Err(reason);
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    return Err(LspError::Timeout {
                        method: "textDocument/publishDiagnostics".into(),
                        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                    })
                }
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn write_frame(writer: &Writer, msg: &Value) -> Result<(), LspError> {
    let bytes = encode(msg)?;
    let mut w = writer.lock().await;
    w.write_all(&bytes)
        .await
        .map_err(|e| LspError::Write(e.to_string()))?;
    w.flush().await.map_err(|e| LspError::Write(e.to_string()))
}

async fn read_loop<R: AsyncRead + Unpin>(mut reader: R, shared: Arc<Shared>, writer: Arc<Writer>) {
    let mut decoder = FrameDecoder::default();
    let mut chunk = vec![0u8; 16 * 1024];
    let reason = loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) if decoder.buffered() == 0 => {
                break exited("the server closed its stdout".into());
            }
            Ok(0) => {
                break exited(format!(
                    "the server closed its stdout mid-frame ({} bytes buffered)",
                    decoder.buffered()
                ));
            }
            Ok(n) => n,
            Err(e) => break exited(format!("read from the server failed: {e}")),
        };
        decoder.extend(&chunk[..n]);
        let failed = loop {
            match decoder.next_message() {
                Ok(Some(msg)) => route(msg, &shared, &writer),
                Ok(None) => break None,
                Err(e) => break Some(e),
            }
        };
        if let Some(e) = failed {
            break e;
        }
    };
    tracing::debug!(error = %reason, "lsp reader stopped");
    shared.close(reason);
}

fn exited(detail: String) -> LspError {
    LspError::ServerExited { detail }
}

fn route(msg: Value, shared: &Shared, writer: &Arc<Writer>) {
    let method = msg.get("method").and_then(Value::as_str);
    match (method, msg.get("id")) {
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                tracing::warn!(%id, "lsp response with a non-integer id; dropped");
                return;
            };
            shared.consecutive_timeouts.store(0, Ordering::SeqCst);
            let waiter = lock(&shared.table).waiters.remove(&id);
            let answer = match msg.get("error") {
                Some(err) => Err(LspError::ServerError {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            match waiter {
                Some(w) => {
                    let _ = w.send(answer);
                }
                None => tracing::debug!(id, "late lsp response after timeout; dropped"),
            }
        }
        (Some(method), Some(id)) => answer_server_request(method, id.clone(), &msg, writer),
        (Some("textDocument/publishDiagnostics"), None) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                tracing::warn!("publishDiagnostics without a uri; dropped");
                return;
            };
            let published = PublishedDiagnostics {
                version: params.get("version").and_then(Value::as_i64),
                diagnostics: params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            };
            lock(&shared.diagnostics).insert(uri.to_string(), published);
            shared.changes.send_modify(|n| *n += 1);
        }
        (Some(method), None) => tracing::trace!(method, "lsp notification ignored"),
        (None, None) => tracing::warn!("lsp message with neither method nor id; dropped"),
    }
}

/// Answer a request the server sent us. The three a server sends during normal
/// work get a neutral success; anything else gets `MethodNotFound`. The reply is
/// written from its own task so a full stdin pipe never stalls the reader.
fn answer_server_request(method: &str, id: Value, msg: &Value, writer: &Arc<Writer>) {
    let reply = match method {
        "workspace/configuration" => {
            let n = msg["params"]["items"].as_array().map_or(0, Vec::len);
            json!({"jsonrpc": "2.0", "id": id, "result": vec![Value::Null; n]})
        }
        "window/workDoneProgress/create" | "client/registerCapability" => {
            json!({"jsonrpc": "2.0", "id": id, "result": null})
        }
        _ => json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": format!("unsupported: {method}")}}),
    };
    let writer = Arc::clone(writer);
    tokio::spawn(async move {
        if let Err(e) = write_frame(&writer, &reply).await {
            tracing::debug!(error = %e, "could not answer a server request");
        }
    });
}

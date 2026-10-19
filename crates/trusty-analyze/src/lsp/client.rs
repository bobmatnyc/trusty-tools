//! JSON-RPC over an LSP byte pipe: request/response correlation, per-request
//! timeouts, server-initiated requests and published diagnostics (#9078).
//!
//! Why: a language server answers out of order, sends its own requests, and
//! pushes diagnostics whenever it likes. One reader task must route every
//! inbound message, or a slow answer blocks a fast one.
//! What: [`LspClient`] runs a reader task over the read half and a single
//! writer task that owns the write half. Every outgoing frame is handed to the
//! writer whole, so a caller that gives up mid-write can never leave half a
//! frame on the wire. Each write is bounded: a request's deadline covers its
//! write and its answer, and the writer abandons any one write after the
//! client's write timeout. A write that cannot finish closes the client,
//! because the server has stopped reading (#9078). Each request registers a
//! waiter keyed by a fresh integer id; the reader completes it. A timed-out
//! waiter is removed and the request is cancelled, so a late answer is dropped
//! by id and cannot reach the next call. When the stream closes or loses its
//! framing, every waiter and every later call fails with the same typed reason.
//! Test: `client_correlates_responses_that_arrive_out_of_order`,
//! `late_response_after_timeout_does_not_desync`,
//! `peer_closing_mid_request_is_an_error_not_a_hang`,
//! `write_to_a_dead_peer_is_an_error`, `server_initiated_requests_are_answered`,
//! `a_peer_that_never_reads_cannot_wedge_a_request`,
//! `a_cancelled_request_never_leaves_a_partial_frame`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::documents::DocumentSet;
use super::framing::{encode, FrameDecoder};
use super::{lock, LspError};

type Waiter = oneshot::Sender<Result<Value, LspError>>;
type Outbox = mpsc::UnboundedSender<WriteJob>;

/// The bound on one write when [`LspClient::new`] picks it.
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// One whole frame for the writer task, and who to tell when it is written.
struct WriteJob {
    bytes: Vec<u8>,
    done: Option<oneshot::Sender<Result<(), LspError>>>,
}

/// Why a send did not complete.
enum SendFailure {
    /// The caller's deadline passed with the frame still unwritten.
    Deadline,
    Failed(LspError),
}

/// The latest `textDocument/publishDiagnostics` for one document.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedDiagnostics {
    /// The document version the server computed against, when it said.
    pub version: Option<i64>,
    /// The raw LSP `Diagnostic` objects.
    pub diagnostics: Vec<Value>,
    /// The client's publication counter after this one arrived; a later
    /// publication always carries a larger number (#9078, L3).
    pub seq: u64,
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
    outbox: Outbox,
    write_timeout: Duration,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    pub(super) documents: DocumentSet,
}

impl LspClient {
    /// Start a client over `reader` (server stdout) and `writer` (server
    /// stdin), with [`DEFAULT_WRITE_TIMEOUT`].
    ///
    /// Must run inside a tokio runtime: it spawns the reader and writer tasks.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::with_write_timeout(reader, writer, DEFAULT_WRITE_TIMEOUT)
    }

    /// [`LspClient::new`], abandoning any single write after `write_timeout`.
    ///
    /// Test: `a_peer_that_never_reads_cannot_wedge_a_request`.
    pub fn with_write_timeout<R, W>(reader: R, writer: W, write_timeout: Duration) -> Self
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
        let (outbox, inbox) = mpsc::unbounded_channel();
        let writer = tokio::spawn(write_loop(
            writer,
            inbox,
            Arc::clone(&shared),
            write_timeout,
        ));
        let reader = tokio::spawn(read_loop(reader, Arc::clone(&shared), outbox.clone()));
        Self {
            shared,
            outbox,
            write_timeout,
            reader,
            writer,
            documents: DocumentSet::default(),
        }
    }

    /// Hand `msg` to the writer and wait until it is written or `deadline`.
    async fn send(&self, msg: &Value, deadline: Instant) -> Result<(), SendFailure> {
        let bytes = encode(msg).map_err(SendFailure::Failed)?;
        let (done, written) = oneshot::channel();
        let job = WriteJob {
            bytes,
            done: Some(done),
        };
        if self.outbox.send(job).is_err() {
            return Err(SendFailure::Failed(self.stopped("the writer stopped")));
        }
        match tokio::time::timeout_at(deadline, written).await {
            Ok(Ok(res)) => res.map_err(SendFailure::Failed),
            Ok(Err(_)) => Err(SendFailure::Failed(self.stopped("the writer stopped"))),
            Err(_) => {
                // #9078: the server is not reading; the stream is unusable.
                self.shared.close(LspError::Write(
                    "a write did not finish in time; the server is not reading its stdin".into(),
                ));
                Err(SendFailure::Deadline)
            }
        }
    }

    /// Queue a frame without waiting for it (replies, cancellations).
    fn post(&self, msg: &Value) {
        post(&self.outbox, msg);
    }

    fn stopped(&self, detail: &str) -> LspError {
        self.closed_reason().unwrap_or(LspError::ServerExited {
            detail: detail.into(),
        })
    }

    fn count_timeout(&self, method: &str, timeout: Duration) -> LspError {
        self.shared
            .consecutive_timeouts
            .fetch_add(1, Ordering::SeqCst);
        LspError::Timeout {
            method: method.to_string(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Send a request; `timeout` bounds its write and its answer together.
    ///
    /// # Errors
    /// [`LspError::Timeout`] (the request is then cancelled with
    /// `$/cancelRequest`; a write still unfinished at the deadline also closes
    /// the client), [`LspError::ServerError`], [`LspError::Write`], or the close
    /// reason ([`LspError::ServerExited`] / [`LspError::Protocol`]).
    ///
    /// Test: `late_response_after_timeout_does_not_desync`,
    /// `server_error_response_is_a_typed_error`.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let deadline = Instant::now() + timeout;
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
        if let Err(failure) = self.send(&msg, deadline).await {
            lock(&self.shared.table).waiters.remove(&id);
            return Err(match failure {
                SendFailure::Deadline => self.count_timeout(method, timeout),
                SendFailure::Failed(e) => e,
            });
        }
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(self.stopped("the reader stopped")),
            Err(_) => {
                // #9078: drop the waiter so a late answer is discarded by id.
                lock(&self.shared.table).waiters.remove(&id);
                self.post(&json!({"jsonrpc": "2.0", "method": "$/cancelRequest",
                                  "params": {"id": id}}));
                Err(self.count_timeout(method, timeout))
            }
        }
    }

    /// Send a notification (no answer expected), bounded by the write timeout.
    ///
    /// # Errors
    /// The close reason, or [`LspError::Write`] (a write that did not finish in
    /// time also closes the client).
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
        let deadline = Instant::now() + self.write_timeout;
        self.send(&msg, deadline).await.map_err(|f| match f {
            SendFailure::Deadline => self.stopped("a write did not finish in time"),
            SendFailure::Failed(e) => e,
        })
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
        self.wait_for_publication(uri, timeout, |d| {
            d.version.is_some_and(|v| v >= min_version)
        })
        .await
    }

    /// Wait until the latest publication for `uri` satisfies `accept`.
    async fn wait_for_publication(
        &self,
        uri: &str,
        timeout: Duration,
        accept: impl Fn(&PublishedDiagnostics) -> bool,
    ) -> Result<PublishedDiagnostics, LspError> {
        let mut changes = self.shared.changes.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(d) = self.diagnostics(uri).filter(|d| accept(d)) {
                return Ok(d);
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

impl LspClient {
    /// The publication counter now; every later publication carries a larger
    /// [`PublishedDiagnostics::seq`].
    pub fn publication_seq(&self) -> u64 {
        *self.shared.changes.borrow()
    }

    /// Every document's latest diagnostics, keyed by URI.
    pub fn all_diagnostics(&self) -> Vec<(String, PublishedDiagnostics)> {
        lock(&self.shared.diagnostics)
            .iter()
            .map(|(uri, d)| (uri.clone(), d.clone()))
            .collect()
    }

    /// Wait for diagnostics that describe `uri` as last synced: published at
    /// `version` or later, or published with no version after `synced_at`.
    ///
    /// Why: rust-analyzer's cargo-check pass publishes without a version
    /// (#9078, architect default (b)). Such a publication is accepted, but only
    /// if it arrived after the text it could describe was sent.
    ///
    /// # Errors
    /// [`LspError::Timeout`] past `timeout`, or the close reason.
    ///
    /// Test: `unversioned_diagnostics_are_accepted_and_never_stale`,
    /// `lsp_diagnostics_for_a_file_are_versioned_and_stamped`.
    pub async fn wait_for_fresh_diagnostics(
        &self,
        uri: &str,
        version: i64,
        synced_at: u64,
        timeout: Duration,
    ) -> Result<PublishedDiagnostics, LspError> {
        self.wait_for_publication(uri, timeout, |d| match d.version {
            Some(v) => v >= version,
            None => d.seq > synced_at,
        })
        .await
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

fn post(outbox: &Outbox, msg: &Value) {
    if let Ok(bytes) = encode(msg) {
        let _ = outbox.send(WriteJob { bytes, done: None });
    }
}

/// The only owner of the server's stdin. Each frame is written whole or the
/// client is closed: an I/O error or a write past `limit` leaves the stream
/// unusable, so every queued and later frame fails with that reason.
async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut inbox: mpsc::UnboundedReceiver<WriteJob>,
    shared: Arc<Shared>,
    limit: Duration,
) {
    while let Some(job) = inbox.recv().await {
        let res = match shared.closed_reason() {
            Some(reason) => Err(reason),
            None => {
                let write = async {
                    writer.write_all(&job.bytes).await?;
                    writer.flush().await
                };
                let res = match tokio::time::timeout(limit, write).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(LspError::Write(e.to_string())),
                    Err(_) => Err(LspError::Write(format!(
                        "a write did not finish within {} ms; the server is not reading its stdin",
                        limit.as_millis()
                    ))),
                };
                if let Err(e) = &res {
                    shared.close(e.clone());
                }
                res
            }
        };
        if let Some(done) = job.done {
            let _ = done.send(res);
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(mut reader: R, shared: Arc<Shared>, outbox: Outbox) {
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
                Ok(Some(msg)) => route(msg, &shared, &outbox),
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

fn route(msg: Value, shared: &Shared, outbox: &Outbox) {
    let method = msg.get("method").and_then(Value::as_str);
    match (method, msg.get("id")) {
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                tracing::warn!(%id, "lsp response with a non-integer id; dropped");
                return;
            };
            shared.consecutive_timeouts.store(0, Ordering::SeqCst);
            let waiter = lock(&shared.table).waiters.remove(&id);
            let answer = match (msg.get("result"), msg.get("error")) {
                (_, Some(err)) => Err(LspError::ServerError {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                (Some(result), None) => Ok(result.clone()),
                // #9078 Fail-Open Check: never a null success.
                (None, None) => Err(LspError::Protocol(format!(
                    "response {id} carries neither result nor error"
                ))),
            };
            match waiter {
                Some(w) => {
                    let _ = w.send(answer);
                }
                None => tracing::debug!(id, "late lsp response after timeout; dropped"),
            }
        }
        (Some(method), Some(id)) => answer_server_request(method, id.clone(), &msg, outbox),
        (Some("textDocument/publishDiagnostics"), None) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                tracing::warn!("publishDiagnostics without a uri; dropped");
                return;
            };
            let mut seq = 0;
            shared.changes.send_modify(|n| {
                *n += 1;
                seq = *n;
            });
            let published = PublishedDiagnostics {
                version: params.get("version").and_then(Value::as_i64),
                diagnostics: params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                seq,
            };
            lock(&shared.diagnostics).insert(uri.to_string(), published);
            // #9078: wake waiters only once the entry is readable.
            shared.changes.send_modify(|n| *n += 1);
        }
        (Some(method), None) => tracing::trace!(method, "lsp notification ignored"),
        (None, None) => tracing::warn!("lsp message with neither method nor id; dropped"),
    }
}

/// Answer a request the server sent us. The three a server sends during normal
/// work get a neutral success; anything else gets `MethodNotFound`. The reply is
/// queued for the writer task, so a full stdin pipe never stalls the reader.
fn answer_server_request(method: &str, id: Value, msg: &Value, outbox: &Outbox) {
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
    post(outbox, &reply);
}

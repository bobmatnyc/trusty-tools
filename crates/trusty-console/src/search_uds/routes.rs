//! `/api/search/{*path}` — the HTTP face of trusty-search's socket (#6285).
//!
//! Why: the console-served dashboard at `/tools/search/` (#6155, PR #6384) is
//! plain browser JavaScript. It speaks HTTP and Server-Sent Events and nothing
//! else, while trusty-search now speaks framed JSON-RPC over a Unix socket. This
//! module is the whole of the translation, so the SPA needs no fork and
//! trusty-search needs no HTTP.
//!
//! What: one handler. It maps the request through [`super::map::map_request`],
//! dials the socket, and answers either a JSON body or an open
//! `text/event-stream`.
//!
//! ## The SSE bridge, frame for frame
//!
//! `trusty_search::service::rpc::streams` states the contract this side has to
//! honour: one stream ITEM is exactly the JSON document one SSE `data:` line
//! carried, parsed rather than prefixed. So the bridge re-prefixes it and adds
//! nothing — the `{"type":"connected"}` opener, every `DaemonEvent`, every
//! reindex progress event and the `{"type":"lag","skipped":N}` frame reach the
//! browser byte-identical to what the daemon's own SSE route wrote.
//!
//! Two things the daemon's SSE route emitted that its RPC stream deliberately
//! does not, and what happens to them here:
//!
//! - the `: heartbeat\n\n` comment every 20 s. It exists so an idle TCP body is
//!   not torn down, and the browser hop is still TCP — so this module emits it,
//!   on the same interval, rather than changing what the browser receives.
//! - the terminal `data:` framing of a failure. A mid-stream failure becomes one
//!   `{"type":"error","message":…}` event before the body closes, because the
//!   SPA reads a closed reindex stream as a COMPLETED reindex
//!   (`crates/trusty-search/ui/src/lib/views/Indexes.svelte`) and a silent close
//!   would report a broken reindex as a finished one.
//!
//! A refusal that arrives BEFORE the first item — the reindex stream's "no
//! progress record for this index" — becomes an HTTP status, not a `200`
//! carrying an error event, which is what `GET /indexes/{id}/reindex/stream`
//! answered over HTTP. That is why the first frame is read before the response
//! head is built.
//!
//! Test: `tests` below, plus `tests/search_uds_bridge.rs`, which drives the
//! whole router against a stub daemon socket.

use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use trusty_common::uds::UdsRpcError;
use trusty_common::uds::stream_client::FramedStream;

use super::map::{Call, map_request};
use super::{
    CALL_TIMEOUT, MAX_FRAME_BYTES, STREAM_OPEN_TIMEOUT, SearchRpcError, call, json_response,
    open_stream,
};
use crate::server::AppState;

/// How often an open stream emits an SSE keep-alive comment.
///
/// The same 20 s `trusty_search::service::server::reindex_handlers` used, so an
/// idle browser connection sees the byte sequence it saw before the migration.
const SSE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// How many stream items may buffer between the socket reader and the browser.
///
/// Matches the daemon-side producer buffer (`streams::STREAM_BUFFER`), so
/// neither side is the first to accumulate behind a slow reader.
const SSE_BUFFER: usize = 64;

/// `ANY /api/search/{*path}` — reach trusty-search over its socket.
///
/// Why a dedicated handler rather than a row in `proxy::routes::full_id`: that
/// proxy forwards bytes to a base URL resolved from an `http_addr` discovery
/// file, and trusty-search stops writing one (#6285, ADR-0032). #6286 and #6287
/// deleted the memory and analyze rows for the same reason; the search row would
/// have been deleted too, taking the dashboard with it.
///
/// What: map the request, resolve the socket, then either one unary exchange
/// rendered as JSON or one stream bridged to Server-Sent Events. An unmapped
/// path is `501` naming itself; an unresolvable or unreachable socket is `502`;
/// a refusal is the status the same refusal carried over HTTP.
///
/// Test: `an_unmapped_path_is_not_implemented_and_names_itself` and
/// `a_dead_socket_is_a_bad_gateway_not_an_empty_success`, plus every other
/// case in `tests/search_uds_bridge.rs`.
pub async fn search_api_handler(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let query = parts.uri.query().map(str::to_owned);

    // #6285: the body becomes one request FRAME, so the frame budget is the body
    // limit — one constant, not a second literal that can drift from it.
    let body_limit = usize::try_from(MAX_FRAME_BYTES).unwrap_or(usize::MAX);
    let body_bytes: Bytes = match axum::body::to_bytes(body, body_limit).await {
        Ok(b) => b,
        Err(e) => {
            warn!("search_uds: could not read the request body: {e}");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body exceeds the {MAX_FRAME_BYTES}-byte frame budget"),
            )
                .into_response();
        }
    };

    let mapped = match map_request(&parts.method, &path, query.as_deref(), &body_bytes) {
        Ok(call) => call,
        Err(reason) => {
            warn!(
                "search_uds: {} /{path} is not mapped: {reason}",
                parts.method
            );
            return (
                StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({ "error": reason, "service": super::SEARCH_SERVICE })),
            )
                .into_response();
        }
    };

    let socket = match state.search_socket_path() {
        Ok(p) => p,
        Err(reason) => return SearchRpcError::Unresolved(reason).into_response(),
    };

    match mapped {
        Call::Unary { method, params } => {
            debug!("search_uds: {} /{path} → {method}", parts.method);
            match call(&socket, method, params, CALL_TIMEOUT).await {
                Ok(result) => json_response(&result),
                Err(e) => e.into_response(),
            }
        }
        Call::Stream { method, params } => {
            debug!("search_uds: {} /{path} → {method} (stream)", parts.method);
            stream_response(&socket, method, params, STREAM_OPEN_TIMEOUT).await
        }
    }
}

/// `ANY /proxy/search/{*path}` — the deprecated alias, kept working (#1849).
///
/// Why kept: the trusty-search SPA's own `base.js` documents `/proxy/search/` as
/// a supported mount, so callers exist that predate the `/api/` rename. They
/// would otherwise get `400 unknown daemon` once the search row leaves
/// `proxy::routes::full_id`.
/// Test: `deprecated_alias_reaches_the_same_handler` in
/// `tests/search_uds_bridge.rs`.
pub async fn deprecated_search_api_handler(
    state: State<AppState>,
    path: AxumPath<String>,
    req: Request,
) -> Response {
    tracing::trace!("search_uds: DEPRECATED /proxy/search/… — use /api/search/… instead (#1849)");
    search_api_handler(state, path, req).await
}

/// Open one stream and answer it as Server-Sent Events.
///
/// Why the first frame is read before the head is built: a streaming method can
/// refuse — `search.index.reindex.stream` answers `404` for an index with no
/// progress record — and that refusal arrives as the stream's FIRST frame, after
/// the dial has already succeeded. Committing to `200 text/event-stream` before
/// reading it would turn every such refusal into an empty stream, which the SPA
/// reads as a finished reindex.
/// What: peek, then either the refusal's own status or a `200` whose body starts
/// with the peeked item.
///
/// `open_timeout` bounds everything up to that peek — the dial, the request
/// write, and the first frame read, which share ONE deadline computed here. A
/// listener whose backlog is full accepts the connection and then reads nothing,
/// and without this bound the browser waits out
/// [`super::STREAM_FRAME_TIMEOUT`]'s day for a response head. Abandoning the read
/// is safe: the whole `FramedStream` is dropped on the timeout path, so no
/// half-read line is left for anyone to resume. The parameter exists so a test
/// need not wait the production minute.
/// Test: `tests/search_uds_bridge.rs`'s
/// `a_stream_refusal_before_the_first_item_is_an_http_status`, plus
/// `a_socket_that_never_answers_is_a_prompt_bad_gateway` and
/// `a_slow_open_and_a_silent_first_frame_share_one_budget` below.
async fn stream_response(
    socket: &Path,
    method: &'static str,
    params: Value,
    open_timeout: std::time::Duration,
) -> Response {
    // #6285: one deadline, taken before the dial and reused for the first-frame
    // read. Giving each step its own `open_timeout` let the two together run to
    // twice the figure the constant names.
    let deadline = tokio::time::Instant::now() + open_timeout;

    let mut stream = match open_stream(socket, method, params, open_timeout).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    let peeked = match tokio::time::timeout_at(deadline, stream.next_frame()).await {
        Ok(frame) => frame,
        Err(_) => {
            return SearchRpcError::Unreachable(format!(
                "{} did not answer {method} within {}s",
                super::SEARCH_SERVICE,
                open_timeout.as_secs_f32()
            ))
            .into_response();
        }
    };

    let first = match peeked {
        Some(Ok(item)) => Some(item),
        Some(Err(e)) => return stream_error(method, e).into_response(),
        // A stream that ended with no items at all is still a well-formed empty
        // answer; the browser gets an immediately-closed event stream, which is
        // what the HTTP route did for a completed reindex with an empty replay.
        None => None,
    };

    let head = futures_util::stream::iter(
        first
            .into_iter()
            .map(|item| Ok::<Bytes, std::convert::Infallible>(sse_data(&item))),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        // The same header the daemon's SSE routes set, so a reverse proxy in
        // front of the console does not buffer the stream into uselessness.
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(head.chain(sse_tail(stream, method))))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The rest of an open stream, as SSE frames plus keep-alive comments.
///
/// Why the reader runs in its own task rather than inside the `select!`:
/// `FramedStream::next_frame` reads a line off a `BufReader`, and cancelling
/// that mid-line — which a heartbeat tick would do — discards the bytes already
/// read. Moving the read behind an `mpsc` makes both arms of the select
/// cancel-safe, since `Receiver::recv` and `Interval::tick` both are.
///
/// The task also carries the disconnect signal: when the browser goes, axum
/// drops this body and the receiver drops. The read itself selects on
/// `Sender::closed()` so the task notices immediately rather than at the next
/// frame — a status stream can be silent for minutes, and waiting for a frame
/// that will never come would hold the socket, and the daemon's producer behind
/// it, open for exactly that long. The same shape the daemon's own producer uses
/// (`trusty_search::service::rpc::streams`). Either way the task returns,
/// dropping the `FramedStream` and closing the socket, which is what ends the
/// producer (`streams`'s "a dropped client stops the producer").
///
/// Test: `tests/search_uds_bridge.rs`'s `a_stream_reaches_the_browser_frame_for_frame`,
/// `a_mid_stream_failure_becomes_an_error_event`, and
/// `a_browser_disconnect_releases_the_daemon_socket` for the disconnect arm.
fn sse_tail(
    mut stream: FramedStream<Value>,
    method: &'static str,
) -> impl futures_util::Stream<Item = Result<Bytes, std::convert::Infallible>> {
    let (tx, rx) = mpsc::channel::<Result<Value, UdsRpcError>>(SSE_BUFFER);
    tokio::spawn(async move {
        loop {
            // Cancelling `next_frame` mid-line discards the bytes already read,
            // which only matters to a reader that resumes. This arm never
            // resumes: it returns, and the `FramedStream` is dropped with it.
            let item = tokio::select! {
                biased;
                () = tx.closed() => return,
                item = stream.next_frame() => item,
            };
            let Some(item) = item else { return };
            let terminal = item.is_err();
            if tx.send(item).await.is_err() || terminal {
                return;
            }
        }
    });

    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + SSE_HEARTBEAT_INTERVAL,
        SSE_HEARTBEAT_INTERVAL,
    );

    futures_util::stream::unfold(Some((rx, heartbeat)), move |state| async move {
        let (mut rx, mut heartbeat) = state?;
        tokio::select! {
            biased;
            item = rx.recv() => match item {
                Some(Ok(value)) => Some((Ok(sse_data(&value)), Some((rx, heartbeat)))),
                Some(Err(e)) => {
                    // #6285: never a silent close. See the module docs.
                    warn!("search_uds: {method} failed mid-stream: {e}");
                    let event = json!({ "type": "error", "message": e.to_string() });
                    Some((Ok(sse_data(&event)), None))
                }
                None => None,
            },
            _ = heartbeat.tick() => Some((
                Ok(Bytes::from_static(b": heartbeat\n\n")),
                Some((rx, heartbeat)),
            )),
        }
    })
}

/// Encode one stream item as an SSE `data:` frame.
///
/// Why `to_string` on a `Value` rather than passing the raw line through: the
/// stream carries parsed JSON, and re-serialising is what puts it back on one
/// line — an embedded newline would split one event into two.
/// Test: `sse_data_is_one_line_per_event`.
fn sse_data(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

/// Turn a stream failure into the console's verdict about it.
///
/// Why: `UdsRpcError::Stream` is the daemon's own terminal error frame and
/// carries its code, so it maps to the HTTP status that refusal had. Every other
/// variant is a transport problem the console observed, which is a `502`.
/// Test: `stream_error_carries_the_daemon_code`.
fn stream_error(method: &str, e: UdsRpcError) -> SearchRpcError {
    match e {
        UdsRpcError::Stream { error, .. } => SearchRpcError::Refused {
            code: error.code,
            message: error.message,
        },
        other => SearchRpcError::Unreachable(format!(
            "{} did not stream {method}: {other}",
            super::SEARCH_SERVICE
        )),
    }
}

/// Resolve trusty-search's socket for the routes and the connector alike.
///
/// Why on `AppState` rather than a free call: the integration tests bind their
/// own stub socket and need the router to dial it, and the alternative —
/// `TRUSTY_DATA_DIR_OVERRIDE` — is process-global in a test binary that runs
/// six connectors in parallel. The same argument `detect::AnalyzeConnector`
/// records for taking a socket override.
/// Test: `tests/search_uds_bridge.rs` drives every case through it.
impl AppState {
    /// The socket this console dials for trusty-search.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or created.
    pub(crate) fn search_socket_path(&self) -> Result<PathBuf, String> {
        match &self.search_socket {
            Some(p) => Ok(p.as_ref().clone()),
            None => super::socket_path(),
        }
    }

    /// Dial `socket` for trusty-search instead of the resolved path.
    ///
    /// Why: see the `AppState::search_socket` field doc — the alternative is a
    /// process-global env var this crate's test binary cannot use safely.
    /// Test: `tests/search_uds_bridge.rs` sets it on every case.
    #[must_use]
    pub fn with_search_socket(mut self, socket: PathBuf) -> Self {
        self.search_socket = Some(std::sync::Arc::new(socket));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::uds::server::RpcError;

    /// Why: an event carrying a newline inside a string would split into two SSE
    /// events and the SPA would parse neither.
    /// Test: this is the test.
    #[test]
    fn sse_data_is_one_line_per_event() {
        let framed = sse_data(&json!({ "message": "a\nb" }));
        let text = String::from_utf8(framed.to_vec()).expect("utf-8");
        assert_eq!(text, "data: {\"message\":\"a\\nb\"}\n\n");
        assert_eq!(text.matches("\n\n").count(), 1);
    }

    /// Why: the daemon's terminal error frame is a refusal with a code, and it
    /// must reach the browser as the status that code stands for — not as a
    /// generic gateway failure that hides which index was missing.
    /// Test: this is the test.
    #[test]
    fn stream_error_carries_the_daemon_code() {
        let err = stream_error(
            "search.index.reindex.stream",
            UdsRpcError::Stream {
                path: PathBuf::from("/tmp/x.sock"),
                error: RpcError::new(-32004, "no reindex in progress for 'ghost'"),
            },
        );
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert!(err.message().contains("ghost"), "{err:?}");
    }

    /// Why: a listener that accepts and then never answers is the case the
    /// per-frame budget cannot cover — that budget is a day, deliberately, so a
    /// stalled reindex is not cut off. Without a separate bound on the OPEN, the
    /// browser waits a day for a response head it will never get.
    /// What: binds a socket that accepts, reads the request, and writes nothing,
    /// then asks for a stream with a 300 ms open budget. The production budget
    /// is [`STREAM_OPEN_TIMEOUT`]; the parameter is what lets this assert in
    /// milliseconds.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_socket_that_never_answers_is_a_prompt_bad_gateway() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("silent.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let _accepting = tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            // Read the request and answer nothing — the wedged-listener case.
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut conn, &mut sink).await;
            std::future::pending::<()>().await;
        });

        let started = std::time::Instant::now();
        let response = stream_response(
            &socket,
            super::super::METHOD_STATUS_STREAM,
            json!({}),
            std::time::Duration::from_millis(300),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the open must be bounded, not left to the per-frame budget: {:?}",
            started.elapsed()
        );
    }

    /// Bind a listener that accepts, stalls `drain_after`, then drains the
    /// request and answers nothing at all.
    ///
    /// The stall is what makes the OPEN slow rather than instant: the request
    /// frame is larger than any plausible UNIX-socket send buffer, so the
    /// client's `write_all` cannot finish until this end starts reading.
    fn stalls_then_drains(socket: PathBuf, drain_after: std::time::Duration) -> PathBuf {
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(drain_after).await;
            let mut sink = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut conn, &mut sink).await;
            std::future::pending::<()>().await;
        });
        socket
    }

    /// A request frame past any plausible socket send buffer.
    fn bulky_params() -> Value {
        json!({ "blob": "x".repeat(512 * 1024) })
    }

    /// Why: the open has two waiting steps — the dial-and-write inside
    /// [`open_stream`] and the first frame read here — and giving each its own
    /// full `open_timeout` made the real ceiling twice the figure
    /// [`STREAM_OPEN_TIMEOUT`], its doc comment and the changelog all name. A
    /// browser waiting two minutes for a bound documented as one is the defect.
    /// What: two phases against the same stub shape, because the ceiling alone
    /// would pass vacuously if the open happened to be fast. Phase one proves
    /// the open both SUCCEEDS and takes about `DRAIN_AFTER`. Phase two runs the
    /// same shape through `stream_response`, whose first-frame read then waits
    /// for a frame that never comes: one shared deadline returns at about
    /// `BUDGET`, two separate ones would run to `DRAIN_AFTER + BUDGET`.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_open_and_a_silent_first_frame_share_one_budget() {
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(1000);
        const DRAIN_AFTER: std::time::Duration = std::time::Duration::from_millis(600);
        // Above `BUDGET`, so one shared deadline clears it; below
        // `DRAIN_AFTER + BUDGET`, so two separate deadlines cannot.
        const CEILING: std::time::Duration = std::time::Duration::from_millis(1300);

        let tmp = tempfile::TempDir::new().expect("tempdir");

        let slow = stalls_then_drains(tmp.path().join("slow-open.sock"), DRAIN_AFTER);
        let started = std::time::Instant::now();
        let opened = open_stream(
            &slow,
            super::super::METHOD_STATUS_STREAM,
            bulky_params(),
            BUDGET,
        )
        .await;
        let open_took = started.elapsed();
        assert!(opened.is_ok(), "the open must succeed, slowly: {opened:?}");
        assert!(
            open_took >= DRAIN_AFTER,
            "the write must park until the peer drains, or this test proves nothing: {open_took:?}"
        );
        drop(opened);

        let silent = stalls_then_drains(tmp.path().join("silent-frame.sock"), DRAIN_AFTER);
        let started = std::time::Instant::now();
        let response = stream_response(
            &silent,
            super::super::METHOD_STATUS_STREAM,
            bulky_params(),
            BUDGET,
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            elapsed >= BUDGET,
            "a shared deadline still spends the whole budget: {elapsed:?}"
        );
        assert!(
            elapsed < CEILING,
            "the dial, the write and the first frame read must share ONE {BUDGET:?} deadline, \
             not take one each: {elapsed:?}"
        );
    }

    /// Why: everything that is not the daemon's own refusal is the console
    /// failing to reach it, and must not be dressed up as a daemon verdict.
    /// Test: this is the test.
    #[test]
    fn stream_error_reports_a_transport_failure_as_unreachable() {
        let err = stream_error(
            "search.status.stream",
            UdsRpcError::NoResponse {
                path: PathBuf::from("/tmp/x.sock"),
            },
        );
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }
}

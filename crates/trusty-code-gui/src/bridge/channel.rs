//! The two event streams, carried over a Tauri IPC `Channel` (#10166).
//!
//! Why: a custom-scheme response is one whole body ([`crate::bridge::scheme`]),
//! and `/sessions/{id}/events` and `/workstreams/{id}/events` are tails that
//! never end. A Tauri `Channel` is the in-process transport that streams, so the
//! stream command opens the route through the same router and forwards each SSE
//! event into the channel. The router, the ticket check and the SSE encoding
//! (`trusty_common::uds::sse`) are unchanged; only the last hop moved.
//!
//! What: [`open`] runs `GET {path}?ticket=` through the router and keeps the
//! body only when it is a `200 text/event-stream`; [`pump`] decodes that body
//! into [`StreamEvent`]s; [`StreamRegistry`] owns the pumping tasks so the
//! webview can close one by id. Every way a stream stops ends with exactly one
//! [`StreamEvent::End`] — the webview reads that as a drop to reconnect
//! through, never as a quiet success.
//!
//! Test: `tests` below, and the stream cases in `tests/bridge_uds.rs`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, HttpBody as _};
use axum::http::{self, StatusCode, header};
use serde::Serialize;
use tokio::task::AbortHandle;
use tower::ServiceExt as _;

/// Largest SSE event the decoder holds before giving up on the stream.
///
/// Why: one event is one daemon frame re-encoded, so a buffer past twice the
/// frame budget without a terminator is a broken stream, not a big event.
const MAX_EVENT_BYTES: usize = 2 * trusty_common::uds::MAX_FRAME_BYTES as usize;

/// How much of a refusal body [`open`] quotes back to the webview.
const MAX_REFUSAL_BYTES: usize = 4096;

/// One thing a stream tells the webview.
///
/// Serialised as `{"kind":"message","data":…}` or
/// `{"kind":"end","error":…}` — the shape `daemon-auth.ts` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    /// One SSE `data:` payload, byte-for-byte what the route wrote.
    Message {
        /// The event's data — a JSON document from the daemon.
        data: String,
    },
    /// The stream is over. The last event a stream sends.
    End {
        /// Why, when the bridge itself saw a failure; `None` when the route
        /// closed the body. A daemon-side failure arrives before this as a
        /// `{"type":"error"}` message.
        error: Option<String>,
    },
}

/// Open `path` through `router` with `ticket`, or say why it did not open.
///
/// Why the ticket stays (owner ruling Q3, 2026-10-23): it binds the stream to
/// a path the token holder asked for, exactly as it did for `EventSource`, and
/// keeps the router's guard the one place that decides.
/// What: refuses a `path` that is not an absolute path free of `?`, `#` and
/// whitespace, and a `ticket` outside `[A-Za-z0-9._~-]`, so neither can splice
/// a second query into the URI. Then `GET {path}?ticket={ticket}`. A non-`200`
/// or non-SSE answer is an `Err` quoting the status and the start of the body.
///
/// # Errors
///
/// A malformed `path` or `ticket`, or any answer but `200 text/event-stream`.
///
/// Test: `a_refused_open_is_an_error_not_an_empty_stream`,
/// `a_path_cannot_smuggle_a_query`, and
/// `a_ticket_opens_one_stream_then_is_spent` in `tests/bridge_uds.rs`.
pub async fn open(router: axum::Router, path: &str, ticket: &str) -> Result<Body, String> {
    if !path.starts_with('/') || path.contains(['?', '#']) || path.contains(char::is_whitespace) {
        return Err(format!("not a stream path: {path:?}"));
    }
    if ticket.is_empty()
        || !ticket
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
    {
        return Err("the ticket is malformed".to_string());
    }

    let request = http::Request::builder()
        .method("GET")
        .uri(format!("{path}?ticket={ticket}"))
        .body(Body::empty())
        .map_err(|e| format!("could not build the stream request: {e}"))?;
    let response = match router.oneshot(request).await {
        Ok(r) => r,
        Err(never) => match never {},
    };

    let status = response.status();
    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    if status == StatusCode::OK && is_sse {
        return Ok(response.into_body());
    }
    let quoted = match axum::body::to_bytes(response.into_body(), MAX_REFUSAL_BYTES).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::from("(body unreadable)"),
    };
    Err(format!("{path} did not open ({status}): {quoted}"))
}

/// Forward every event in `body` to `emit` until the body ends or `emit` says
/// the receiver is gone.
///
/// Why `emit` returns `bool`: a `Channel` whose webview reloaded fails to send,
/// and a pump that kept reading would hold the daemon socket open for nothing.
/// What: decodes SSE blocks, emits one [`StreamEvent::Message`] per `data:`
/// payload (comments such as `: heartbeat` are dropped), and finishes with one
/// [`StreamEvent::End`] — `error: None` when the body closed, `Some` when it
/// failed or could not be decoded. Returns early, with no `End`, once `emit`
/// returns `false`; dropping `body` then closes the daemon socket.
/// Test: `a_body_becomes_messages_then_one_end`,
/// `a_stream_that_drops_midway_ends_with_an_error`,
/// `a_gone_receiver_stops_the_pump`.
pub async fn pump(mut body: Body, mut emit: impl FnMut(StreamEvent) -> bool) {
    let mut decoder = SseDecoder::default();
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        let chunk = match frame {
            None => {
                emit(StreamEvent::End { error: None });
                return;
            }
            Some(Err(e)) => {
                emit(StreamEvent::End {
                    error: Some(format!("the stream failed: {e}")),
                });
                return;
            }
            Some(Ok(frame)) => match frame.into_data() {
                Ok(chunk) => chunk,
                // A trailers frame carries no events.
                Err(_) => continue,
            },
        };
        match decoder.push(&chunk) {
            Ok(events) => {
                for data in events {
                    if !emit(StreamEvent::Message { data }) {
                        return;
                    }
                }
            }
            Err(reason) => {
                emit(StreamEvent::End {
                    error: Some(reason),
                });
                return;
            }
        }
    }
}

/// Splits an SSE byte stream into `data:` payloads.
///
/// Why only `\n\n`: the one encoder upstream (`trusty_common::uds::sse`) ends
/// every event with it, and a decoder that guessed at other terminators could
/// split one event in two.
#[derive(Default)]
struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    /// Append `chunk` and return every event it completed.
    ///
    /// # Errors
    ///
    /// An event that is not UTF-8, or a buffer past [`MAX_EVENT_BYTES`] with no
    /// terminator. Both end the stream rather than skipping data.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = self.buf.drain(..end + 2).collect();
            let text = std::str::from_utf8(&block[..end])
                .map_err(|_| "the stream sent an event that is not UTF-8".to_string())?;
            let data: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        if self.buf.len() > MAX_EVENT_BYTES {
            return Err(format!(
                "the stream sent {} bytes with no event terminator",
                self.buf.len()
            ));
        }
        Ok(events)
    }
}

/// The live stream pumps, by the id the webview holds.
///
/// Why: the webview closes a stream when a component unmounts or switches
/// workstream; without a handle to abort, the pump would hold the daemon socket
/// until the next event failed to send — on a quiet workstream, indefinitely.
#[derive(Default)]
pub struct StreamRegistry {
    next_id: AtomicU64,
    live: Arc<Mutex<HashMap<u64, AbortHandle>>>,
}

impl StreamRegistry {
    /// Open `path` and pump it into `emit` on its own task; return its id.
    ///
    /// What: [`open`] first, so a refusal is an `Err` and no task exists; then
    /// the pump task, registered under a fresh id before it can finish and
    /// deregister itself.
    ///
    /// # Errors
    ///
    /// Whatever [`open`] refused.
    ///
    /// Test: `a_closed_stream_is_aborted_and_forgotten`,
    /// `a_finished_stream_forgets_itself`.
    pub async fn open(
        &self,
        router: axum::Router,
        path: &str,
        ticket: &str,
        emit: impl FnMut(StreamEvent) -> bool + Send + 'static,
    ) -> Result<u64, String> {
        let body = open(router, path, ticket).await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let live = Arc::clone(&self.live);
        // Held across the spawn so the task's own removal cannot run first.
        let mut map = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let task = tokio::spawn(async move {
            pump(body, emit).await;
            live.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        });
        map.insert(id, task.abort_handle());
        Ok(id)
    }

    /// Stop stream `id`. `false` when it had already ended or never existed.
    ///
    /// Test: `a_closed_stream_is_aborted_and_forgotten`.
    pub fn close(&self, id: u64) -> bool {
        let handle = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        handle.map(|h| h.abort()).is_some()
    }

    /// How many streams are pumping.
    #[must_use]
    pub fn live(&self) -> usize {
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::time::Duration;

    fn sse_body(chunks: Vec<Result<&'static str, std::io::Error>>) -> Body {
        Body::from_stream(futures_util::stream::iter(
            chunks.into_iter().map(|c| c.map(axum::body::Bytes::from)),
        ))
    }

    async fn collect(body: Body) -> Vec<StreamEvent> {
        let mut seen = Vec::new();
        pump(body, |e| {
            seen.push(e);
            true
        })
        .await;
        seen
    }

    fn message(data: &str) -> StreamEvent {
        StreamEvent::Message {
            data: data.to_string(),
        }
    }

    /// Why: every event must reach the webview as the payload the route wrote,
    /// across any chunking, with heartbeats dropped and exactly one `End`.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_body_becomes_messages_then_one_end() {
        let body = sse_body(vec![
            Ok("data: {\"seq\":1}\n\n: heartbeat\n\nda"),
            Ok("ta: {\"seq\":2}\n"),
            Ok("\n"),
        ]);
        assert_eq!(
            collect(body).await,
            vec![
                message("{\"seq\":1}"),
                message("{\"seq\":2}"),
                StreamEvent::End { error: None }
            ]
        );
    }

    /// Why (fail-open check): a body that fails mid-stream must end with an
    /// error the webview can see, not a clean close.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_stream_that_drops_midway_ends_with_an_error() {
        let body = sse_body(vec![
            Ok("data: {\"seq\":1}\n\n"),
            Err(std::io::Error::other("socket reset")),
        ]);
        let seen = collect(body).await;
        assert_eq!(seen[0], message("{\"seq\":1}"));
        assert!(
            matches!(&seen[1], StreamEvent::End { error: Some(e) } if e.contains("socket reset")),
            "{seen:?}"
        );
        assert_eq!(seen.len(), 2);
    }

    /// Why: an undecodable event must stop the stream with a reason, not be
    /// replaced by a guess while the stream carries on.
    /// Test: this is the test.
    #[tokio::test]
    async fn an_undecodable_event_ends_the_stream() {
        let body = Body::from(vec![b'd', b'a', b't', b'a', b':', 0xFF, b'\n', b'\n']);
        let seen = collect(body).await;
        assert!(
            matches!(&seen[..], [StreamEvent::End { error: Some(_) }]),
            "{seen:?}"
        );
    }

    /// Why: a webview that went away must release the stream at once.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_gone_receiver_stops_the_pump() {
        let body = sse_body(vec![Ok("data: 1\n\ndata: 2\n\n")]);
        let mut calls = 0;
        pump(body, |_| {
            calls += 1;
            false
        })
        .await;
        assert_eq!(calls, 1, "no event may be sent after the receiver is gone");
    }

    /// Why (fail-open check): a refused open must be an error the webview can
    /// act on, never an empty stream that reads as a quiet one.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_refused_open_is_an_error_not_an_empty_stream() {
        let router = axum::Router::new().route(
            "/sessions/{id}/events",
            get(|| async { (StatusCode::UNAUTHORIZED, "no") }),
        );
        let err = open(router, "/sessions/s-1/events", "t1")
            .await
            .expect_err("a 401 must not open");
        assert!(err.contains("401"), "{err}");
    }

    /// Why: the path and ticket are spliced into a URI; neither may add a
    /// second query parameter.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_path_cannot_smuggle_a_query() {
        let router = axum::Router::new();
        for (path, ticket) in [
            ("/sessions/s-1/events?ticket=x", "t1"),
            ("/sessions/s 1/events", "t1"),
            ("sessions/s-1/events", "t1"),
            ("/sessions/s-1/events", "t1&ticket=x"),
            ("/sessions/s-1/events", ""),
        ] {
            assert!(
                open(router.clone(), path, ticket).await.is_err(),
                "{path} / {ticket} must be refused"
            );
        }
    }

    fn quiet_sse_router() -> axum::Router {
        axum::Router::new().route(
            "/sessions/{id}/events",
            get(|| async {
                axum::response::Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::pending::<
                        Result<axum::body::Bytes, std::io::Error>,
                    >()))
                    .expect("response")
            }),
        )
    }

    /// Why: a component that unmounts must stop its stream; a pump left running
    /// on a quiet workstream would hold the daemon socket forever.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_closed_stream_is_aborted_and_forgotten() {
        let registry = StreamRegistry::default();
        let id = registry
            .open(quiet_sse_router(), "/sessions/s-1/events", "t1", |_| true)
            .await
            .expect("open");
        assert_eq!(registry.live(), 1);
        assert!(registry.close(id));
        assert_eq!(registry.live(), 0);
        assert!(!registry.close(id), "a second close finds nothing");
    }

    /// Why: a stream that ended on its own must not linger in the registry.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_finished_stream_forgets_itself() {
        let router = axum::Router::new().route(
            "/sessions/{id}/events",
            get(|| async { ([(header::CONTENT_TYPE, "text/event-stream")], "data: 1\n\n") }),
        );
        let registry = StreamRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry
            .open(router, "/sessions/s-1/events", "t1", move |e| {
                tx.send(e).is_ok()
            })
            .await
            .expect("open");
        let mut seen = Vec::new();
        while let Ok(Some(e)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            seen.push(e);
        }
        assert_eq!(seen, vec![message("1"), StreamEvent::End { error: None }]);
        for _ in 0..50 {
            if registry.live() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the finished stream is still registered");
    }
}

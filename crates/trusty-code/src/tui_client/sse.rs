//! [`SseLines`]: a minimal Server-Sent-Events line pump over a
//! `reqwest::Response` body (issue #3415).
//!
//! Why: `CodeEngine` consumes two SSE routes
//! (`GET /sessions/{id}/events`, `GET /workstreams/{id}/events`) that both
//! frame their payloads as plain `data: <json>\n\n` lines (axum's
//! `axum::response::sse::Event::default().json_data(..)`, see
//! `crate::serve::http::sse_event_for`/`crate::workstreams::sse::sse_event_for`).
//! Pulling in a full SSE client crate for "read `data:` lines from a byte
//! stream" would be disproportionate; `crate::cli_client::stdio` already
//! shows this crate's convention of hand-rolling the minimal wire parser
//! for a trusted, in-house transport rather than adding a dependency for a
//! few lines of line-splitting (mirrors
//! `trusty_common::chat::openai_compat::sse_pump::pump_openai_sse`'s
//! `buf.find('\n')` loop, adapted to this crate's own `data:` framing).
//! What: [`SseLines::new`] wraps a `reqwest::Response`'s byte stream;
//! [`SseLines::next_data`] returns the next non-empty `data:` payload
//! (already stripped of the prefix and trimmed) as `Ok(Some(json_text))`,
//! `Ok(None)` on a clean upstream close, or `Err` on a stream I/O failure.
//! SSE comment lines (`:` keep-alive pings) and blank `data:` lines are
//! silently skipped, never surfaced.
//! Test: `sse_tests::*` against synthetic byte chunks (no network).

use std::pin::Pin;

use futures_util::{Stream, StreamExt};

/// See module docs.
pub struct SseLines {
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Vec<u8>>> + Send>>,
    buf: String,
}

impl SseLines {
    /// Wrap `resp`'s body as an SSE line source.
    pub fn new(resp: reqwest::Response) -> Self {
        // `.map` converts each `bytes::Bytes` chunk to `Vec<u8>` so this
        // module never needs to name the `bytes` crate's type directly (not
        // otherwise a dependency of this crate).
        let stream = resp.bytes_stream().map(|r| r.map(|b| b.to_vec()));
        Self {
            stream: Box::pin(stream),
            buf: String::new(),
        }
    }

    /// Return the next `data:` payload, reading more of the underlying byte
    /// stream as needed.
    ///
    /// Why/What: see module docs.
    /// Test: `sse_tests::next_data_skips_comments_and_blank_lines`,
    /// `sse_tests::next_data_returns_none_on_clean_close`,
    /// `sse_tests::next_data_on_empty_body_returns_none`.
    pub async fn next_data(&mut self) -> Result<Option<String>, reqwest::Error> {
        loop {
            if let Some(idx) = self.buf.find('\n') {
                let line: String = self.buf.drain(..=idx).collect();
                let line = line.trim();
                if let Some(payload) = line.strip_prefix("data:") {
                    let payload = payload.trim();
                    if !payload.is_empty() {
                        return Ok(Some(payload.to_string()));
                    }
                }
                // Every other line shape (blank keep-alive, `event:`,
                // `id:`, `:comment`) is intentionally ignored — neither SSE
                // route this client consumes uses named events or ids.
                continue;
            }
            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        self.buf.push_str(text);
                    }
                    // Invalid UTF-8 mid-chunk is dropped rather than erroring
                    // — SSE framing here is always JSON text; a malformed
                    // chunk boundary is exceedingly unlikely and, if it
                    // happens, the next chunk still resyncs on the next
                    // `\n`.
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build an `SseLines` whose byte stream comes from a `wiremock`
    /// response body — the simplest way to get a REAL `reqwest::Response`
    /// (needed for `.bytes_stream()`) without standing up a hand-rolled
    /// hyper server.
    async fn sse_lines_for_body(body: &str) -> SseLines {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_string(), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let resp = reqwest::get(format!("{}/events", server.uri()))
            .await
            .expect("get");
        SseLines::new(resp)
    }

    /// Comment lines (`: keep-alive`) and blank `data:` framing must be
    /// skipped; only real payloads are returned, in order.
    #[tokio::test]
    async fn next_data_skips_comments_and_blank_lines() {
        let mut lines =
            sse_lines_for_body(": keep-alive\n\ndata: {\"a\":1}\n\ndata:\n\ndata: {\"a\":2}\n\n")
                .await;
        assert_eq!(
            lines.next_data().await.expect("read"),
            Some("{\"a\":1}".to_string())
        );
        assert_eq!(
            lines.next_data().await.expect("read"),
            Some("{\"a\":2}".to_string())
        );
        assert_eq!(lines.next_data().await.expect("read"), None);
    }

    /// A clean upstream close (no more bytes) must yield `Ok(None)`, not an
    /// error — this is the normal "server finished the stream" case, not a
    /// transport failure.
    #[tokio::test]
    async fn next_data_returns_none_on_clean_close() {
        let mut lines = sse_lines_for_body("data: {\"only\":true}\n\n").await;
        assert_eq!(
            lines.next_data().await.expect("read"),
            Some("{\"only\":true}".to_string())
        );
        assert_eq!(lines.next_data().await.expect("read"), None);
    }

    /// An empty body must yield `Ok(None)` on the very first call.
    #[tokio::test]
    async fn next_data_on_empty_body_returns_none() {
        let mut lines = sse_lines_for_body("").await;
        assert_eq!(lines.next_data().await.expect("read"), None);
    }
}

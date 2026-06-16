//! Shared test-only helpers for the SM provider mock servers.
//!
//! Why: the Anthropic and OpenRouter provider tests both drive `complete`
//! against a raw `TcpListener` HTTP mock. A naive mock that does a single
//! `read` may reply before the client has finished writing the request
//! (headers + body), which surfaces as intermittent connection-reset flakiness.
//! Centralising a robust "read the whole request first" helper keeps both mocks
//! deterministic without pulling in an external mock-server crate.
//! What: [`read_full_request`] loops reading from the socket until the HTTP
//! header terminator (`\r\n\r\n`) is seen, then drains any declared
//! `Content-Length` body bytes, so the server only replies once the full
//! request has arrived.
//! Test: exercised transitively by every `complete_*` provider round-trip test.

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

/// Read an entire HTTP/1.1 request (headers + any `Content-Length` body) from
/// `sock` before the caller writes a response.
///
/// Why: a single `sock.read()` can return before the client has flushed the
/// full request, letting the mock reply early and reset the connection — the
/// source of the flaky-mock finding. Reading to the header terminator (and then
/// the declared body) makes the mock wait for the complete request.
/// What: appends reads into a buffer until `\r\n\r\n` appears, parses an
/// optional case-insensitive `Content-Length`, then keeps reading until the
/// buffer holds the full header + body (or the peer closes / errors). All I/O
/// errors are swallowed: this is a best-effort test mock, and the assertions
/// live in the response path.
/// Test: used by the Anthropic + OpenRouter `complete_*` tests.
pub async fn read_full_request(sock: &mut TcpStream) {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];

    // Phase 1: read until the end of the request headers (`\r\n\r\n`).
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        match sock.read(&mut chunk).await {
            Ok(0) => return, // peer closed before completing headers
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    };

    // Phase 2: if a Content-Length is present, drain that many body bytes.
    let content_len = parse_content_length(&buf[..header_end]);
    let want_total = header_end + content_len;
    while buf.len() < want_total {
        match sock.read(&mut chunk).await {
            Ok(0) => return, // peer closed; reply with what we have
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
}

/// Return the byte offset just past the `\r\n\r\n` header terminator, if present.
///
/// Why: marks where headers end and the body begins.
/// What: scans for the 4-byte CRLFCRLF sequence; returns `Some(end)` past it.
/// Test: exercised via [`read_full_request`].
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Parse a case-insensitive `Content-Length` header value from header bytes.
///
/// Why: lets the mock drain the full request body before replying.
/// What: scans CRLF-delimited header lines for `content-length:` and parses the
/// decimal value; returns `0` when absent or unparseable.
/// Test: exercised via [`read_full_request`].
fn parse_content_length(header_bytes: &[u8]) -> usize {
    let headers = String::from_utf8_lossy(header_bytes);
    for line in headers.split("\r\n") {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

//! Cancel-safe, size-capped NDJSON line reader shared by stdio JSON-RPC
//! clients.
//!
//! Why: #10360 — `StdioMcpClient` and trusty-code's `StdioRpcClient` both
//! read newline-delimited frames from a child under a timeout. tokio's
//! `read_line` loses consumed bytes when the future drops, and an unbounded
//! buffer lets a child that never ends a line exhaust memory. One helper
//! keeps both clients on the same semantics.
//! What: [`read_capped_line`] plus the [`CappedLine`] outcome.
//! Test: `capped_line::tests::*`, and the client-level tests
//! `oversized_line_errors_and_next_call_skips_its_tail` (trusty-common) and
//! `oversize_line_errors_then_next_call_resyncs` (trusty-code).

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Outcome of one [`read_capped_line`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum CappedLine {
    /// One line, `\n` included unless the stream ended without one.
    Line(Vec<u8>),
    /// The line passed the cap. Its bytes were dropped, and the bytes up to
    /// the next `\n` are dropped by the following calls.
    TooLong,
    /// The stream ended with no partial line buffered.
    Eof,
}

/// Read one line of at most `cap` bytes from `reader`.
///
/// Why: see the module doc — cancel safety and a memory bound in one place.
/// What: copies from `reader` into `buf` in `fill_buf` / `consume` steps with
/// no await between copy and consume, so a cancelled call loses nothing and
/// the next call resumes the same line from `buf`. A line that would pass
/// `cap` empties `buf`, sets `discard`, and returns [`CappedLine::TooLong`];
/// while `discard` is set, bytes up to the next `\n` are dropped. The caller
/// owns `buf` and `discard` and resets both when it replaces `reader` (a
/// respawn). At EOF a partial line is returned as a `Line`.
/// Test: `capped_line::tests::*`.
pub async fn read_capped_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    discard: &mut bool,
    cap: usize,
) -> std::io::Result<CappedLine> {
    loop {
        let avail = reader.fill_buf().await?;
        if avail.is_empty() {
            if buf.is_empty() {
                return Ok(CappedLine::Eof);
            }
            return Ok(CappedLine::Line(std::mem::take(buf)));
        }
        let newline = avail.iter().position(|&b| b == b'\n');
        let len = newline.map_or(avail.len(), |i| i + 1);
        if *discard {
            reader.consume(len);
            *discard = newline.is_none();
            continue;
        }
        if buf.len() + len > cap {
            reader.consume(len);
            *buf = Vec::new();
            *discard = newline.is_none();
            return Ok(CappedLine::TooLong);
        }
        buf.extend_from_slice(&avail[..len]);
        reader.consume(len);
        if newline.is_some() {
            return Ok(CappedLine::Line(std::mem::take(buf)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line over the cap reports `TooLong`; its tail, split across reads,
    /// is skipped and the next line comes back intact.
    #[tokio::test]
    async fn too_long_line_is_reported_then_skipped_to_next_newline() {
        let data: &[u8] = b"0123456789abcdef\nok\n";
        // A 4-byte inner buffer splits the long line over several fills.
        let mut reader = tokio::io::BufReader::with_capacity(4, data);
        let (mut buf, mut discard) = (Vec::new(), false);
        let first = read_capped_line(&mut reader, &mut buf, &mut discard, 8).await;
        assert_eq!(first.expect("io"), CappedLine::TooLong);
        assert!(buf.is_empty(), "no oversize bytes are retained");
        let next = read_capped_line(&mut reader, &mut buf, &mut discard, 8).await;
        assert_eq!(next.expect("io"), CappedLine::Line(b"ok\n".to_vec()));
        let end = read_capped_line(&mut reader, &mut buf, &mut discard, 8).await;
        assert_eq!(end.expect("io"), CappedLine::Eof);
    }

    /// A line exactly at the cap is accepted; an unterminated tail is
    /// returned at EOF.
    #[tokio::test]
    async fn line_at_cap_and_eof_partial_are_returned() {
        let data: &[u8] = b"abc\nxy";
        let mut reader = tokio::io::BufReader::new(data);
        let (mut buf, mut discard) = (Vec::new(), false);
        let first = read_capped_line(&mut reader, &mut buf, &mut discard, 4).await;
        assert_eq!(first.expect("io"), CappedLine::Line(b"abc\n".to_vec()));
        let tail = read_capped_line(&mut reader, &mut buf, &mut discard, 4).await;
        assert_eq!(tail.expect("io"), CappedLine::Line(b"xy".to_vec()));
    }
}

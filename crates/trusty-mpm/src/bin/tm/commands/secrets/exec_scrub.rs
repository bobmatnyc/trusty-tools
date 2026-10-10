//! The output scrubber of `tm secrets exec` (#9621).
//!
//! Why: owner amendment 3 (2026-09-11) — "echoed values are scrubbed from
//! captured output". When stdout is not a terminal, a caller (an agent's
//! Bash call, a pipe, a file) captures what the command prints, so every
//! injected value is removed from it first. Architect ruling 2026-10-10:
//! terminal output is not captured and is passed through.
//! What: [`Scrubber`] removes every value of [`MIN_SCRUB_CHARS`] or more
//! from a byte stream as `[REDACTED]`, leftmost-longest. It holds back a
//! tail that could still grow into a value, so a value split across two
//! reads is still removed. It works on bytes, not text (unlike
//! `scrub_secrets`), so binary output passes through unchanged. [`pump`]
//! runs one stream.
//! Test: `exec_tests.rs` — `scrubber_removes_a_value_split_across_reads`,
//! `scrubber_never_cuts_a_value_that_starts_with_another`,
//! `scrubber_passes_short_values_and_binary_through`.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Shortest value scrubbed. A shorter one would rewrite ordinary output; it
/// matches trusty-common's `MIN_SCRUBBABLE_SECRET_CHARS`.
pub(crate) const MIN_SCRUB_CHARS: usize = 8;

/// The text that replaces a value; the same marker `scrub_secrets` writes.
const REDACTED: &[u8] = b"[REDACTED]";

/// One stream's scrubbing state.
pub(crate) struct Scrubber {
    /// Values to remove, longest first.
    needles: Vec<Vec<u8>>,
    /// Bytes read but not yet emitted.
    pending: Vec<u8>,
}

impl Scrubber {
    /// A scrubber for `values`; values under [`MIN_SCRUB_CHARS`] characters
    /// are left in the output.
    pub(crate) fn new<S: AsRef<str>>(values: &[S]) -> Self {
        let mut needles: Vec<Vec<u8>> = values
            .iter()
            .map(AsRef::as_ref)
            .filter(|v| v.chars().count() >= MIN_SCRUB_CHARS)
            .map(|v| v.as_bytes().to_vec())
            .collect();
        needles.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        needles.dedup();
        Self {
            needles,
            pending: Vec::new(),
        }
    }

    /// Take `chunk`; return the bytes that are safe to emit now.
    ///
    /// What: appends `chunk` and runs [`Self::drain`] without end of stream,
    /// so a tail that could still grow into a value is kept back.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        self.drain(false)
    }

    /// Everything still held, scrubbed: call once at end of stream.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.drain(true)
    }

    /// Scan `pending` left to right, leftmost-longest.
    ///
    /// What: at each position, when more input could still complete a value
    /// longer than what is left (and `eof` is false), stop and keep the rest;
    /// else replace the longest value that starts here; else emit one byte.
    /// Waiting for the longer value first means a value that starts with a
    /// shorter one is never cut in two.
    fn drain(&mut self, eof: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pending.len());
        let mut i = 0;
        while i < self.pending.len() {
            let rest = &self.pending[i..];
            if !eof
                && self
                    .needles
                    .iter()
                    .any(|n| n.len() > rest.len() && n.starts_with(rest))
            {
                break;
            }
            match self.needles.iter().find(|n| rest.starts_with(n)) {
                Some(needle) => {
                    out.extend_from_slice(REDACTED);
                    i += needle.len();
                }
                None => {
                    out.push(rest[0]);
                    i += 1;
                }
            }
        }
        self.pending.drain(..i);
        out
    }
}

/// Copy `from` to `to` through `scrubber` until end of stream.
///
/// What: reads 8 KiB at a time and writes what [`Scrubber::push`] releases,
/// flushing after each read so output keeps its timing; at end of stream it
/// writes [`Scrubber::finish`]. A write error ends the pump; the command's
/// own exit status is reported either way.
pub(crate) async fn pump<R, W>(
    mut from: R,
    mut to: W,
    mut scrubber: Scrubber,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 8192];
    loop {
        let n = from.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let out = scrubber.push(&buf[..n]);
        to.write_all(&out).await?;
        to.flush().await?;
    }
    let rest = scrubber.finish();
    to.write_all(&rest).await?;
    to.flush().await
}

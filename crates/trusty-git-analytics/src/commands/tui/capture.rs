//! Divert `tracing` output away from the terminal while `tga tui` owns it.
//!
//! Why: `init_tracing`-style subscribers write to stderr on purpose — stdout
//! has to stay clean for MCP JSON-RPC framing — but stderr and the alternate
//! screen are the same device. A `warn!` from the collect pipeline therefore
//! prints straight into the drawn frame, and because ratatui only repaints
//! cells it believes changed, the garbling survives every later redraw and is
//! permanent for the session. This is not hypothetical at the default level: a
//! bare worktree whose `origin` is not fetchable emits the fetch-failure WARN
//! with no `RUST_LOG` and no `-v` (#5197).
//!
//! What: [`LogCapture`], a `MakeWriter` that is a plain `io::stderr()` handle
//! until it is armed, and a bounded in-memory line buffer while it is. `tga
//! tui` arms it once the alternate screen is up and disarms it after the
//! terminal is restored, and drains it once per render tick into the ACTIVITY
//! pane — so the operator READS the log rather than losing it.
//!
//! Suppressing the lines outright was rejected: at the default level the only
//! thing tracing emits during a pull is warnings, and discarding a warning to
//! protect a redraw trades one defect for a quieter one.
//!
//! Scope: this is `commands/`-private, so it adds nothing to the published
//! `tga` library API. It also does not touch `trusty_common::init_tracing`;
//! `tga` builds its own subscriber in `main.rs`, and the stderr default there
//! is unchanged for every subcommand except `tui`.
//!
//! Test: `tests` in this module, plus
//! `super::tests::pump_progress_surfaces_captured_log_lines`.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// How many undrained log lines the capture holds.
///
/// Why: the TUI drains every 100 ms, so this only has to cover a burst. An
/// unbounded buffer would let a `RUST_LOG=trace` run grow without limit behind
/// a paused draw loop.
/// What: `1024` lines, oldest evicted first — the same drop-oldest policy, and
/// the same reasoning, as `tga::core::progress::ProgressBus`.
/// Test: `tests::overflow_drops_oldest_and_counts`.
const CAPACITY: usize = 1024;

/// Shared state behind a [`LogCapture`]. Never exposed.
#[derive(Debug, Default)]
struct CaptureInner {
    /// Complete lines waiting to be drained.
    lines: VecDeque<String>,
    /// Bytes seen since the last newline, held until the line completes.
    partial: String,
    /// Set while the TUI owns the terminal.
    armed: bool,
}

/// A `tracing` writer that can be diverted into memory for a TUI's lifetime.
///
/// Why: see the module doc — writing to stderr while ratatui owns the screen
/// permanently corrupts it.
/// What: a cheap clonable handle. Disarmed (the default, and what every
/// non-`tui` subcommand gets) [`LogCapture::make_writer`] hands back a real
/// `io::Stderr`, so the subscriber behaves exactly as it did before this
/// existed. Armed, writes are split on newlines and buffered; [`LogCapture::drain`]
/// hands them to the caller.
///
/// A line still buffered at [`LogCapture::disarm`] is discarded rather than
/// flushed to the terminal: the pane has been showing them live, and dumping a
/// run's worth of `INFO` onto the restored shell on quit would be its own
/// defect.
///
/// Test: `tests::disarmed_capture_writes_to_stderr`,
/// `tests::armed_capture_buffers_whole_lines`.
#[derive(Debug, Clone, Default)]
pub struct LogCapture {
    inner: Arc<Mutex<CaptureInner>>,
    dropped: Arc<AtomicU64>,
}

impl LogCapture {
    /// A disarmed capture — writes pass through to stderr.
    ///
    /// Why/What/Test: `Default`; see `tests::disarmed_capture_writes_to_stderr`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start diverting tracing output into memory.
    ///
    /// Why/What/Test: called once the alternate screen is up; see
    /// `tests::arming_diverts_and_disarming_restores`.
    pub fn arm(&self) {
        self.lock().armed = true;
    }

    /// Stop diverting, and discard whatever was still buffered.
    ///
    /// Why/What/Test: called after the terminal is restored; see
    /// `tests::arming_diverts_and_disarming_restores`.
    pub fn disarm(&self) {
        let mut inner = self.lock();
        inner.armed = false;
        inner.lines.clear();
        inner.partial.clear();
    }

    /// Whether output is currently being diverted.
    ///
    /// Why/What/Test: see `tests::arming_diverts_and_disarming_restores`.
    pub fn is_armed(&self) -> bool {
        self.lock().armed
    }

    /// Take every complete line captured so far, oldest first.
    ///
    /// Why: the render loop wants one cheap call per tick, exactly like
    /// `ProgressBus::drain`.
    /// What: leaves any incomplete trailing line in place, so a half-written
    /// event is never shown as a truncated one.
    /// Test: `tests::armed_capture_buffers_whole_lines`.
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().lines).into()
    }

    /// How many lines the capacity bound has discarded.
    ///
    /// Why/What/Test: see `tests::overflow_drops_oldest_and_counts`.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Append raw formatted bytes, splitting them into lines.
    ///
    /// What: invalid UTF-8 is replaced rather than dropped — a mangled log line
    /// is worth more than a silent one, and this must never fail the write.
    fn push(&self, buf: &[u8]) {
        let text = String::from_utf8_lossy(buf);
        let mut inner = self.lock();
        for ch in text.chars() {
            if ch != '\n' {
                inner.partial.push(ch);
                continue;
            }
            let mut line = std::mem::take(&mut inner.partial);
            if line.ends_with('\r') {
                line.pop();
            }
            while inner.lines.len() >= CAPACITY {
                inner.lines.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            inner.lines.push_back(line);
        }
    }

    /// Lock the shared state, recovering in place from a poisoned mutex.
    ///
    /// Why: a panicking consumer must not turn every later log write into a
    /// second panic — least of all from inside a panic hook.
    fn lock(&self) -> std::sync::MutexGuard<'_, CaptureInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where one formatted tracing event goes.
///
/// Why/What/Test: see [`LogCapture`]; the variant is chosen per event by
/// [`LogCapture::make_writer`], so arming takes effect on the very next event.
#[derive(Debug)]
pub enum CaptureWriter {
    /// The capture is disarmed: this is the ordinary stderr path.
    Stderr(io::Stderr),
    /// The capture is armed: the event is buffered for the ACTIVITY pane.
    Buffered(LogCapture),
}

impl CaptureWriter {
    /// Whether this writer diverts rather than writing to the terminal.
    ///
    /// Test-only, and `#[cfg(test)]` to keep it that way: it lets a test assert
    /// the routing without writing to the real stderr of the test binary, and
    /// production code has no reason to ask.
    #[cfg(test)]
    pub fn is_buffered(&self) -> bool {
        matches!(self, Self::Buffered(_))
    }
}

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Stderr(w) => w.write(buf),
            Self::Buffered(c) => {
                c.push(buf);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Stderr(w) => w.flush(),
            // Nothing is buffered behind an OS handle; drain() is the flush.
            Self::Buffered(_) => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        if self.is_armed() {
            CaptureWriter::Buffered(self.clone())
        } else {
            CaptureWriter::Stderr(io::stderr())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disarmed_capture_writes_to_stderr() {
        let capture = LogCapture::new();
        assert!(!capture.is_armed());
        assert!(
            !capture.make_writer().is_buffered(),
            "a disarmed capture must hand back the ordinary stderr writer"
        );
    }

    #[test]
    fn arming_diverts_and_disarming_restores() {
        let capture = LogCapture::new();
        capture.arm();
        assert!(capture.is_armed());
        assert!(capture.make_writer().is_buffered());

        capture
            .make_writer()
            .write_all(b"buffered\n")
            .expect("write");
        capture.disarm();
        assert!(!capture.is_armed());
        assert!(!capture.make_writer().is_buffered());
        assert!(
            capture.drain().is_empty(),
            "disarm discards what the pane already showed"
        );
    }

    #[test]
    fn armed_capture_buffers_whole_lines() {
        let capture = LogCapture::new();
        capture.arm();
        let mut w = capture.make_writer();
        w.write_all(b"2026-08-08T16:53:27Z  WARN fetch failed\n")
            .expect("write");
        w.write_all(b"second line\n").expect("write");
        assert_eq!(
            capture.drain(),
            vec![
                "2026-08-08T16:53:27Z  WARN fetch failed".to_string(),
                "second line".to_string()
            ]
        );
        assert!(capture.drain().is_empty(), "drain empties the buffer");
    }

    #[test]
    fn a_partial_line_is_held_until_its_newline_arrives() {
        let capture = LogCapture::new();
        capture.arm();
        let mut w = capture.make_writer();
        w.write_all(b"half a li").expect("write");
        assert!(
            capture.drain().is_empty(),
            "an unterminated line must not render as a truncated one"
        );
        w.write_all(b"ne\n").expect("write");
        assert_eq!(capture.drain(), vec!["half a line".to_string()]);
    }

    #[test]
    fn overflow_drops_oldest_and_counts() {
        let capture = LogCapture::new();
        capture.arm();
        let mut w = capture.make_writer();
        for i in 0..CAPACITY + 5 {
            w.write_all(format!("line {i}\n").as_bytes())
                .expect("write");
        }
        let lines = capture.drain();
        assert_eq!(lines.len(), CAPACITY);
        assert_eq!(lines[0], format!("line {}", 5), "the oldest are evicted");
        assert_eq!(capture.dropped(), 5);
    }

    #[test]
    fn clones_share_one_buffer() {
        let capture = LogCapture::new();
        let clone = capture.clone();
        clone.arm();
        assert!(capture.is_armed(), "arming is visible through every handle");
        capture
            .make_writer()
            .write_all(b"from the original\n")
            .expect("write");
        assert_eq!(clone.drain(), vec!["from the original".to_string()]);
    }
}

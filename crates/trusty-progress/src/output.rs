//! Centralised output mode + sink.
//!
//! Every renderer in this crate routes through [`Output`] so terminal
//! detection, the quiet/non-TTY fallback, and the write sink are decided in
//! exactly one place.

use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};

use indicatif::ProgressDrawTarget;

use crate::error::Result;

/// Why: A progress display behaves very differently in an interactive terminal
/// (live spinners, in-place redraws, colour) versus a pipe / CI log / `--json`
/// run (plain, append-only lines). Centralising the choice in one enum stops
/// every call site from re-deriving it and keeps the fallback consistent.
/// What: The three rendering modes the crate supports.
/// Test: `output::tests::mode_selects_draw_target` and the renderer tests in
/// `narrator`/`components` assert plain output in the non-interactive modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Interactive terminal: animated indicatif draw targets are used.
    Interactive,
    /// Not a terminal (pipe, file, CI) or `--json`/quiet requested: emit plain
    /// append-only lines and hide indicatif animations.
    Plain,
    /// Fully silent: suppress all output (used for `--quiet` where even plain
    /// lines are unwanted). Renderers become no-ops.
    Silent,
}

/// Why: Tests must assert on rendered bytes (e.g. "no ANSI escapes when not a
/// TTY") without touching the real stderr, and production callers want output
/// on stderr so stdout stays clean for JSON-RPC framing. A trait object behind
/// an `Arc<Mutex<…>>` lets us swap stderr for an in-memory buffer.
/// What: The shared, thread-safe write target for rendered lines.
type SharedSink = Arc<Mutex<dyn Write + Send>>;

/// Why: Bundles the rendering [`Mode`] with the line sink so the narrator,
/// component tracker, and progress handles all share one source of truth for
/// "where does text go and how animated should it be".
/// What: The handle every renderer holds; cheap to clone (`Arc` inside).
/// Test: `output::tests::*` plus the renderer modules construct `Output` via
/// [`Output::for_capture`] and assert on the captured buffer.
#[derive(Clone)]
pub struct Output {
    mode: Mode,
    sink: SharedSink,
}

impl Output {
    /// Why: The common production constructor — autodetect the terminal so
    /// callers get animations interactively and plain lines in CI/pipes for
    /// free, writing to stderr (stdout stays clean for protocol framing).
    /// What: Builds an `Output` writing to stderr, choosing [`Mode::Interactive`]
    /// when stderr is a terminal and [`Mode::Plain`] otherwise.
    /// Test: Side-effect-only (depends on the ambient TTY); the mode-selection
    /// logic it delegates to is covered by `mode_selects_draw_target`.
    pub fn to_stderr() -> Self {
        let mode = if is_stderr_terminal() {
            Mode::Interactive
        } else {
            Mode::Plain
        };
        Self {
            mode,
            sink: Arc::new(Mutex::new(io::stderr())),
        }
    }

    /// Why: Lets a CLI honour an explicit `--quiet` / `--json` flag, overriding
    /// autodetection (e.g. force plain output even on a TTY for `--json`).
    /// What: Builds an stderr-backed `Output` with the caller-chosen mode.
    /// Test: Side-effect-only; mode handling is covered via `for_capture`.
    pub fn to_stderr_with_mode(mode: Mode) -> Self {
        Self {
            mode,
            sink: Arc::new(Mutex::new(io::stderr())),
        }
    }

    /// Why: Enables deterministic unit tests — render into an owned buffer and
    /// assert on the exact bytes, including the "no ANSI escapes" guarantee of
    /// the plain fallback.
    /// What: Builds an `Output` with the given mode whose sink is a shared
    /// in-memory buffer; the returned [`Capture`] reads the accumulated bytes.
    /// Test: `output::tests::capture_roundtrips_written_bytes`.
    pub fn for_capture(mode: Mode) -> (Self, Capture) {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let output = Self {
            mode,
            sink: Arc::new(Mutex::new(SharedBuf(buf.clone()))),
        };
        (output, Capture(buf))
    }

    /// Why: Renderers branch on the mode to decide between animated and plain
    /// rendering; exposing it keeps that branching out of the sink internals.
    /// What: Returns the current [`Mode`].
    /// Test: `output::tests::mode_selects_draw_target`.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Why: `true` in any mode that should emit text at all; lets renderers
    /// short-circuit cleanly in [`Mode::Silent`] without duplicating the check.
    /// What: Returns whether plain/animated line output is enabled.
    /// Test: `output::tests::visible_reflects_mode`.
    pub fn is_visible(&self) -> bool {
        self.mode != Mode::Silent
    }

    /// Why: indicatif needs a [`ProgressDrawTarget`]; in non-interactive modes
    /// we must hand it a hidden target so spinners don't spam plain logs with
    /// control characters. One place decides this for all progress handles.
    /// What: Returns the indicatif draw target appropriate for the mode —
    /// stderr for [`Mode::Interactive`], hidden otherwise.
    /// Test: `output::tests::mode_selects_draw_target`.
    pub fn draw_target(&self) -> ProgressDrawTarget {
        match self.mode {
            Mode::Interactive => ProgressDrawTarget::stderr(),
            Mode::Plain | Mode::Silent => ProgressDrawTarget::hidden(),
        }
    }

    /// Why: The narrator and component tracker emit whole lines; funnelling
    /// them through one method guarantees consistent newline handling, the
    /// silent-mode short-circuit, and a single I/O error surface.
    /// What: Writes `line` followed by `\n` to the sink unless silent; flushes
    /// so piped consumers see output promptly.
    /// Test: `narrator::tests` and `components::tests` assert on the bytes this
    /// writes; the silent short-circuit is covered by
    /// `output::tests::silent_writes_nothing`.
    pub(crate) fn writeln(&self, line: &str) -> Result<()> {
        if self.mode == Mode::Silent {
            return Ok(());
        }
        let mut sink = self
            .sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sink.write_all(line.as_bytes())?;
        sink.write_all(b"\n")?;
        sink.flush()?;
        Ok(())
    }
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// Why: Tests need to read back exactly what was rendered to assert on layout
/// and the absence of ANSI escapes; this is the read side of [`Output::for_capture`].
/// What: A cloneable handle over the same in-memory buffer the `Output` writes.
/// Test: `output::tests::capture_roundtrips_written_bytes`.
#[derive(Clone)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Why: Assertions want a `String`, and the renderers only ever write UTF-8.
    /// What: Returns the accumulated output as a lossy UTF-8 string.
    /// Test: `output::tests::capture_roundtrips_written_bytes`.
    pub fn contents(&self) -> String {
        let buf = self.0.lock().unwrap_or_else(|p| p.into_inner());
        String::from_utf8_lossy(&buf).into_owned()
    }
}

/// Internal newtype so the shared `Vec<u8>` can implement `Write` behind the
/// `dyn Write + Send` sink object.
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut buf = self.0.lock().unwrap_or_else(|p| p.into_inner());
        buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Why: Terminal detection is the one piece of environment-coupled logic; we
/// use std's `IsTerminal` (stable, well below the 1.91 MSRV) rather than adding
/// an `is-terminal`/`atty` crate, keeping the dep tree to just `indicatif` +
/// `thiserror`.
/// What: Returns whether stderr is an interactive terminal.
/// Test: Side-effect-only (ambient FD state); not unit-tested directly.
fn is_stderr_terminal() -> bool {
    io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The draw-target choice is what suppresses spinner control codes in
    /// non-interactive contexts; a regression here would corrupt plain logs.
    /// The crucial, environment-independent invariant is that Plain and Silent
    /// are *always* hidden. (Interactive delegates to
    /// `ProgressDrawTarget::stderr()`, which is itself hidden when stderr is not
    /// a TTY — as under the test harness — so we cannot assert it is visible in
    /// a deterministic way; we assert it matches stderr's own report instead.)
    /// What: Asserts Plain/Silent are unconditionally hidden, and Interactive
    /// tracks the real stderr terminal state.
    /// Test: This is the test.
    #[test]
    fn mode_selects_draw_target() {
        for mode in [Mode::Plain, Mode::Silent] {
            let (out, _) = Output::for_capture(mode);
            assert!(out.draw_target().is_hidden());
        }

        let (interactive, _) = Output::for_capture(Mode::Interactive);
        assert_eq!(
            interactive.draw_target().is_hidden(),
            ProgressDrawTarget::stderr().is_hidden()
        );
    }

    /// Why: Renderers rely on `is_visible` to gate output; verify the mapping.
    /// What: Asserts only `Silent` is invisible.
    /// Test: This is the test.
    #[test]
    fn visible_reflects_mode() {
        let (i, _) = Output::for_capture(Mode::Interactive);
        let (p, _) = Output::for_capture(Mode::Plain);
        let (s, _) = Output::for_capture(Mode::Silent);
        assert!(i.is_visible());
        assert!(p.is_visible());
        assert!(!s.is_visible());
    }

    /// Why: The capture sink is the foundation of every other test; confirm it
    /// faithfully records writes.
    /// What: Writes a line and reads it back via `Capture`.
    /// Test: This is the test.
    #[test]
    fn capture_roundtrips_written_bytes() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        out.writeln("hello").expect("write");
        assert_eq!(cap.contents(), "hello\n");
    }

    /// Why: Silent mode must emit absolutely nothing, even when asked to write.
    /// What: Writes in silent mode and asserts the buffer stays empty.
    /// Test: This is the test.
    #[test]
    fn silent_writes_nothing() {
        let (out, cap) = Output::for_capture(Mode::Silent);
        out.writeln("should not appear").expect("write");
        assert_eq!(cap.contents(), "");
    }
}

//! Rustup-style `info:` narrator lines.

use crate::error::Result;
use crate::output::Output;

/// Prefix rustup uses for informational narration.
const INFO_PREFIX: &str = "info:";
/// Prefix for warnings (rustup uses `warning:`).
const WARN_PREFIX: &str = "warning:";
/// Prefix for errors (rustup uses `error:`).
const ERROR_PREFIX: &str = "error:";

/// Why: The rustup output Bob wants is framed by `info: …` narration lines
/// (`info: downloading 6 components`, `info: default toolchain set to …`).
/// Centralising the prefix + sink avoids every call site re-spelling the
/// format and routing decision.
/// What: A thin printer that emits prefixed status lines through a shared
/// [`Output`], honouring its TTY/quiet/silent mode.
/// Test: `narrator::tests` assert exact line text and the silent/plain paths.
#[derive(Debug, Clone)]
pub struct Narrator {
    output: Output,
}

impl Narrator {
    /// Why: Callers that already hold an [`Output`] (e.g. alongside a component
    /// tracker) should share the same mode/sink rather than constructing a new
    /// one with possibly divergent terminal detection.
    /// What: Wraps an existing `Output` in a `Narrator`.
    /// Test: `narrator::tests::info_line_matches_rustup`.
    pub fn new(output: Output) -> Self {
        Self { output }
    }

    /// Why: Convenience for the common "I just want narration on stderr" case
    /// so a CLI need not touch the `Output` type at all.
    /// What: Builds a `Narrator` over an autodetected stderr [`Output`].
    /// Test: Side-effect-only (ambient TTY); delegates to tested `Output`.
    pub fn to_stderr() -> Self {
        Self::new(Output::to_stderr())
    }

    /// Why: The primary verb — print an `info:`-prefixed status line exactly as
    /// rustup does.
    /// What: Emits `info: <msg>` (or nothing in silent mode).
    /// Test: `narrator::tests::info_line_matches_rustup`.
    pub fn info(&self, msg: &str) -> Result<()> {
        self.output.writeln(&format!("{INFO_PREFIX} {msg}"))
    }

    /// Why: Some flows need to flag a recoverable problem in the same visual
    /// register as the narration.
    /// What: Emits `warning: <msg>`.
    /// Test: `narrator::tests::warn_and_error_prefixes`.
    pub fn warn(&self, msg: &str) -> Result<()> {
        self.output.writeln(&format!("{WARN_PREFIX} {msg}"))
    }

    /// Why: Lets a flow report a hard failure line consistently with the rest
    /// of the progress output (the caller still returns its own error type).
    /// What: Emits `error: <msg>`.
    /// Test: `narrator::tests::warn_and_error_prefixes`.
    pub fn error(&self, msg: &str) -> Result<()> {
        self.output.writeln(&format!("{ERROR_PREFIX} {msg}"))
    }

    /// Why: A component tracker and its surrounding narration must share one
    /// sink so lines interleave in order; expose the inner `Output` for that.
    /// What: Returns a clone of the underlying [`Output`].
    /// Test: `narrator::tests::shares_output_clone`.
    pub fn output(&self) -> Output {
        self.output.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Mode;

    /// Why: The exact `info: ` prefix (lowercase, single space) is the
    /// load-bearing visual contract with the rustup style.
    /// What: Asserts a single `info` call renders `info: downloading 6 components\n`.
    /// Test: This is the test.
    #[test]
    fn info_line_matches_rustup() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let narrator = Narrator::new(out);
        narrator.info("downloading 6 components").expect("info");
        assert_eq!(cap.contents(), "info: downloading 6 components\n");
    }

    /// Why: Guard the warn/error prefixes too so they stay rustup-faithful.
    /// What: Asserts both prefixes render verbatim.
    /// Test: This is the test.
    #[test]
    fn warn_and_error_prefixes() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let narrator = Narrator::new(out);
        narrator.warn("disk almost full").expect("warn");
        narrator.error("download failed").expect("error");
        assert_eq!(
            cap.contents(),
            "warning: disk almost full\nerror: download failed\n"
        );
    }

    /// Why: Silent mode must drop narration entirely (e.g. scripted use).
    /// What: Asserts no bytes are emitted in silent mode.
    /// Test: This is the test.
    #[test]
    fn silent_mode_emits_nothing() {
        let (out, cap) = Output::for_capture(Mode::Silent);
        let narrator = Narrator::new(out);
        narrator.info("hidden").expect("info");
        assert_eq!(cap.contents(), "");
    }

    /// Why: Narration lines must never contain ANSI escape sequences in the
    /// non-TTY/plain fallback, or piped logs and `--json` consumers break.
    /// What: Asserts the rendered plain output has no ESC (`0x1b`) byte.
    /// Test: This is the test.
    #[test]
    fn plain_output_has_no_ansi_escapes() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let narrator = Narrator::new(out);
        narrator.info("downloading 6 components").expect("info");
        assert!(!cap.contents().contains('\u{1b}'));
    }

    /// Why: A component tracker shares the narrator's sink; verify the clone
    /// writes to the same buffer.
    /// What: Writes through both the narrator and its cloned output and asserts
    /// both lines land in order.
    /// Test: This is the test.
    #[test]
    fn shares_output_clone() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let narrator = Narrator::new(out);
        narrator.info("first").expect("info");
        narrator.output().writeln("second").expect("write");
        assert_eq!(cap.contents(), "info: first\nsecond\n");
    }
}

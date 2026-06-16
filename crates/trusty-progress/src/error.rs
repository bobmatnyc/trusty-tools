//! Error type for the `trusty-progress` crate.

use std::io;

/// Why: Library crates in this workspace must surface structured errors via
/// `thiserror` (never `anyhow`, never `unwrap`) so callers can match on the
/// failure mode. The only fallible path in this crate is writing rendered
/// output to the underlying sink, so we model that single failure.
/// What: Enumerates the error conditions a progress sink can raise.
/// Test: Exercised indirectly by `output` tests that write to an in-memory
/// sink (which never errors) and by the `Display`/`From` derives below; the
/// `io::Error` arm is covered by the `From<io::Error>` conversion test in
/// `error::tests`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProgressError {
    /// Why: Writing a narrator line or a component row to the configured sink
    /// (stderr, a file, or a captured buffer) can fail with an I/O error; the
    /// caller needs that surfaced rather than silently dropped.
    /// What: Wraps the underlying [`std::io::Error`] from a failed write/flush.
    /// Test: `error::tests::io_error_converts_and_displays`.
    #[error("failed to write progress output: {0}")]
    Io(#[from] io::Error),
}

/// Why: Saves every public function from repeating the full `Result<T,
/// ProgressError>` spelling and gives callers a single canonical alias.
/// What: Crate-local `Result` specialised to [`ProgressError`].
/// Test: Used as the return type throughout the crate; compilation is the test.
pub type Result<T> = std::result::Result<T, ProgressError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Guards the `#[from]` conversion and the `Display` message so a
    /// future refactor cannot silently change the public error surface.
    /// What: Builds a `ProgressError` from an `io::Error` and asserts the
    /// rendered message contains the wrapped cause.
    /// Test: This is the test.
    #[test]
    fn io_error_converts_and_displays() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe gone");
        let err: ProgressError = io_err.into();
        let rendered = err.to_string();
        assert!(rendered.contains("failed to write progress output"));
        assert!(rendered.contains("pipe gone"));
    }
}

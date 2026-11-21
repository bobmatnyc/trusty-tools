//! Errors of the Intent Ledger port and their exit codes.
//!
//! Why: `datg.py` raises one `DatgError` that the CLI prints as `datg: <msg>` and
//! maps to exit 2, and lets an `OSError` escape as a traceback. The port keeps
//! the exit code and message of the first and turns every I/O failure into the
//! same loud exit 2, so a failed write never reads as success.
//! What: [`IlError`] and [`IlResult`]; [`invalid`] builds the `DatgError` case.
//! Test: `error_variants_render_the_message_only`.

use std::path::Path;

/// A ledger operation that failed; the CLI exits 2 for every variant.
#[derive(Debug, thiserror::Error)]
pub enum IlError {
    /// Invalid input or a refused write (`DatgError`). Nothing was changed.
    #[error("{0}")]
    Invalid(String),
    /// `state.json` is unreadable or has a bad shape. Nothing is repaired.
    #[error("{0}")]
    Corrupt(String),
    /// Another writer held `.lock` past the wait bound. Nothing was changed.
    #[error("{0}")]
    Locked(String),
    /// A file operation failed; `what` names the file and the step.
    #[error("{what}: {source}")]
    Io {
        /// The file and step.
        what: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// Result of a ledger operation.
pub type IlResult<T> = Result<T, IlError>;

/// The `DatgError` case: `Err(IlError::Invalid(msg))`.
pub fn invalid<T>(msg: impl Into<String>) -> IlResult<T> {
    Err(IlError::Invalid(msg.into()))
}

impl IlError {
    /// An [`IlError::Io`] builder for `.map_err`: `step` then the file path.
    pub fn io(step: &str, path: &Path) -> impl FnOnce(std::io::Error) -> IlError {
        let what = format!("{step} {}", path.display());
        move |source| IlError::Io { what, source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variants_render_the_message_only() {
        assert_eq!(IlError::Invalid("bad".into()).to_string(), "bad");
        assert_eq!(IlError::Locked("held".into()).to_string(), "held");
        let e = IlError::io("write", Path::new("/x/state.json"))(std::io::Error::other("full"));
        assert_eq!(e.to_string(), "write /x/state.json: full");
    }
}

//! The crate's error type.
//!
//! Why: `trusty-audit` is a library with a thin CLI over it (#5502), so the
//! library owes callers a structured error rather than `anyhow` — the CLI, and
//! later the Tauri shell, each render failures their own way.
//! What: one `#[non_exhaustive]` enum covering the three things the scaffold can
//! fail at — touching the working directory, reading the companion files, and
//! being asked for a capability that is not built yet.
//! Test: `super::error_tests`.

use std::path::PathBuf;

/// Everything `trusty-audit`'s library surface can fail with.
///
/// Why: every variant names the path or tool it concerns, because the recipient
/// running this is not the author and a bare `io::Error` tells them nothing
/// about which of the working directory's several files was unreadable.
/// What: `#[non_exhaustive]` so adding a variant in a later milestone is not a
/// breaking change (CLAUDE.md, semver gate).
/// Test: `super::error_tests::messages_name_the_offending_path`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// A working-directory entry could not be created or inspected.
    #[error("working directory {path}: {source}")]
    WorkDir {
        /// The path that failed.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A companion file (engagement config, audit manifest) could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A companion file was read but is not valid TOML for its schema.
    #[error("{path} is not a valid {what}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// Which schema was expected, e.g. `"audit manifest"`.
        what: &'static str,
        /// The underlying TOML error.
        ///
        /// Boxed because `toml::de::Error` carries the full input span and is
        /// large enough on its own to trip `clippy::result_large_err` for every
        /// `Result<_, AuditError>` in the crate.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// A capability this scaffold declares but does not yet implement.
    ///
    /// Why: the scaffold must fail closed. Returning an explicit error keeps a
    /// half-built capability from silently reporting success, and — for tool
    /// downloads specifically — keeps this crate from ever growing a bespoke
    /// download path while #5491's entry point is still being built.
    #[error("{capability} is not implemented yet — tracked in #{issue}")]
    NotImplemented {
        /// Human-readable capability name.
        capability: &'static str,
        /// The issue that lands it.
        issue: u32,
    },
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn messages_name_the_offending_path() {
        let err = AuditError::Read {
            path: PathBuf::from("/tmp/engagement/manifest.toml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("/tmp/engagement/manifest.toml"),
            "error should name the file: {rendered}"
        );
    }

    #[test]
    fn not_implemented_cites_the_issue_that_lands_it() {
        let err = AuditError::NotImplemented {
            capability: "tool downloads",
            issue: 5491,
        };
        assert_eq!(
            err.to_string(),
            "tool downloads is not implemented yet — tracked in #5491"
        );
    }
}

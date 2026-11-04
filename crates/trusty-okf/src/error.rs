//! The crate error type (SPEC-OKF-01 §4.4).
//!
//! Why: every public entry point reports failure through one enum, so a caller
//! can tell a refused write (`Conformance`, `InvalidId`) from an I/O failure.
//! What: the variants slice S2 of #10746 needs. Later slices add the rest of
//! the §4.4 list; the enum is `#[non_exhaustive]` so that is not a break.
//! Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
//! `c115_foreign_paths_read_only_and_write_is_invalid_id`.

use std::path::PathBuf;

use crate::format::validate::Finding;

/// Why a trusty-okf call failed (SPEC-OKF-01 §4.4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OkfError {
    /// A file system call failed.
    #[error("i/o error at {path:?}: {source}")]
    Io {
        /// The file or directory the call touched.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// Frontmatter is not valid YAML, or not a mapping, or cannot be split
    /// into top-level entries for a raw-preserving write (§2.2.1).
    #[error("parse error in {path:?} (line {line:?}): {message}")]
    Parse {
        /// The file, or an empty path when the caller passed text only.
        path: PathBuf,
        /// The 1-based line the YAML parser named, when it named one.
        line: Option<usize>,
        /// The parser's message.
        message: String,
    },
    /// A write refused under the active `WriteMode`; holds the blocking
    /// findings.
    #[error("write refused by the active write mode: {0:?}")]
    Conformance(Vec<Finding>),
    /// A concept ID that breaks a §2.11 lexical rule.
    #[error("invalid concept id {input:?}: breaks SPEC-OKF-01 §2.11 rule {rule}")]
    InvalidId {
        /// The rejected input.
        input: String,
        /// The §2.11 rule number (1-8) it breaks; 0 when no rule applies
        /// but the path is still foreign.
        rule: u8,
    },
    /// A path leaves the bundle root, or crosses a symbolic link (§2.11).
    #[error("path escapes the bundle root or crosses a link: {0:?}")]
    PathEscape(PathBuf),
}

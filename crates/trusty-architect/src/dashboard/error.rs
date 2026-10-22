//! The dashboard store's error type.
//!
//! Why: callers (the daemon method, the MCP tool and the CLI of #10063 PR 3)
//! must tell a conflict from a refusal from an unreadable store without
//! parsing text. The mapping to RPC classes and exit codes is left to PR 3:
//! the conflict exit code is an open owner question (D34), so nothing here
//! fixes one.
//! What: [`DashError`], one variant per outcome class of the prototype
//! (`StoreError`, `NotFound`, `Conflict`, `SchemaInvalid`) plus the ones the
//! prototype raised as plain `StoreError` but a caller must treat apart: a
//! store in the other format, a corrupt or partial file, a held lock, an I/O
//! failure and a schema file the validator refuses, and a rollback that
//! could not restore every file.
//! Test: `stale_if_version_conflicts_and_leaves_bytes`,
//! `other_format_refused_not_empty`, `lock_contention_is_unavailable_and_writes_nothing`,
//! `failed_restore_names_the_torn_files`.

use std::path::{Path, PathBuf};

/// Why a dashboard store call did not complete. Nothing was written, except
/// after [`DashError::RollbackIncomplete`]: rollback covers a failed write,
/// not a crash.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DashError {
    /// A bad id, body, op or argument; the text names the rule.
    #[error("{0}")]
    Invalid(String),
    /// The document does not exist.
    #[error("{0}")]
    NotFound(String),
    /// `if_version` did not match the stored version (LDG-2.8.1).
    #[error("{0}")]
    Conflict(String),
    /// The store is laid out in the other format; it is never read as empty
    /// (LDG-3.2).
    #[error("{0}")]
    WrongFormat(String),
    /// A written document fails its schema; every problem is listed.
    #[error("{}", summarize(.0))]
    SchemaInvalid(Vec<String>),
    /// A schema file could not be loaded, uses a keyword or a pattern the
    /// validator does not implement, or holds an unresolved `$ref`.
    #[error("{0}")]
    Schema(String),
    /// A document or collection file is not readable JSON of the store's shape.
    #[error("{path}: {reason}")]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The store lock was not granted within the wait.
    #[error("{0}")]
    Unavailable(String),
    /// A batch write failed and rollback could not restore every earlier
    /// file: the store is torn at `unrestored` (#10063 df-G01).
    #[error("{cause}; rollback did not restore {}", list(.unrestored))]
    RollbackIncomplete {
        /// The files left with the batch's bytes (or absence).
        unrestored: Vec<PathBuf>,
        /// The write failure that started the rollback.
        #[source]
        cause: Box<DashError>,
    },
}

/// `a, b` of the paths' display forms.
fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl DashError {
    /// [`DashError::Io`] for `path`; a short form for `map_err`.
    pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// [`DashError::Corrupt`] for `path`.
    pub(crate) fn corrupt(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self::Corrupt {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        }
    }
}

/// The prototype's `SchemaInvalid` text: the first five problems, then a count.
fn summarize(errors: &[String]) -> String {
    let head = errors
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    match errors.len() {
        n if n > 5 => format!("{head} (+{} more)", n - 5),
        _ => head,
    }
}

impl From<crate::error::LedgerError> for DashError {
    /// The shared lock and atomic-write helpers report [`crate::LedgerError`].
    fn from(e: crate::error::LedgerError) -> Self {
        use crate::error::LedgerError;
        match e {
            LedgerError::Io { path, source } => Self::Io { path, source },
            LedgerError::Unavailable(m) => Self::Unavailable(m),
            other => Self::Invalid(other.to_string()),
        }
    }
}

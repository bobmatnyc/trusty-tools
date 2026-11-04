//! The crate error type (SPEC-OKF-01 §4.4).
//!
//! Why: every public entry point reports failure through one enum, so a caller
//! can tell a refused write (`Conformance`, `InvalidId`) from an I/O failure.
//! What: the variants slice S2 of #10746 needs. Later slices add the rest of
//! the §4.4 list; the enum is `#[non_exhaustive]` so that is not a break.
//! Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
//! `c115_foreign_paths_read_only_and_write_is_invalid_id`,
//! `c35_verify_with_stale_hash_is_conflict_and_writes_nothing`,
//! `c140_fold_collision_on_put_and_move`.

use std::path::PathBuf;

use crate::format::id::ConceptId;
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
    /// The concept does not exist (§4.4).
    #[error("concept not found: {0}")]
    NotFound(ConceptId),
    /// The write conflicts with the bundle's state; no file changed (§4.4).
    /// #10746 S3 reasons: `exists`, `stale_write`, `fold_collision`,
    /// `stale_verify_hash`, `stale_after_extension`, `deprecated`,
    /// `verified_is_owned_by_verify`, `legacy_verified_exists`,
    /// `legacy_generated_exists`, `target_exists`,
    /// `verification_record_exists`, `human_not_confirmed` (§2.4).
    #[error("conflict on {id}: {reason}")]
    Conflict {
        /// The concept the write targets.
        id: ConceptId,
        /// Why the write was refused.
        reason: String,
    },
    /// #10746 S5: an operation of the journal is incomplete and conflicted;
    /// a write touching one of its paths needs `Bundle::recover` first
    /// (SPEC-OKF-01 §4.11 rule 2).
    #[error("operation {op_id} needs an explicit recover; paths {paths:?}")]
    RecoveryPending {
        /// The incomplete operation.
        op_id: String,
        /// Its file-step paths, bundle-relative.
        paths: Vec<PathBuf>,
    },
    /// #10746 S5: an `IndexStateDir` inside the bundle root (§4.9.1).
    #[error("index state directory lies inside the bundle root: {0:?}")]
    StateDirInBundle(PathBuf),
    /// #10746 S7: the search index root is missing, not absolute, or does not
    /// contain the bundle root; the feed pass refused before any push
    /// (SPEC-OKF-01 §5.4 F5).
    #[error(
        "index {index_id} root {index_root:?} does not contain the bundle root {bundle_root:?}"
    )]
    RootMismatch {
        /// The caller's index ID.
        index_id: String,
        /// The root the daemon reported, when it reported an absolute one.
        index_root: Option<PathBuf>,
        /// The canonical bundle root.
        bundle_root: PathBuf,
    },
    /// #10746 S7: the daemon confirmed a write but the index journal append
    /// failed; the task counts as not indexed and the pass stopped (§5.4 F3).
    #[error("index journal append failed at {path:?}: {source}")]
    JournalWrite {
        /// The journal file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// #10746 S7: the index holds a path under the bundle's `_sources/` or
    /// `_state/`; the pass removed the prefix, pushed nothing and advanced no
    /// state (§5.3.1 rule 3, F16).
    #[error("index {index_id} holds bookkeeping paths: {leaked:?}")]
    IndexScope {
        /// The caller's index ID.
        index_id: String,
        /// The leaked paths, index-relative.
        leaked: Vec<PathBuf>,
    },
    /// #10746 S7: a trusty-search call failed (feed preflight and `retrieve`
    /// only). Feature `search-feed`.
    #[cfg(feature = "search-feed")]
    #[error("{0}")]
    Search(crate::feed::SearchError),
    /// #10746 S4: the doc-store read policy refused a directory; nothing was
    /// read. The display is the reason alone, so a caller that matched
    /// trusty-kb's message text still matches (SPEC-OKF-01 §2.13 reuses
    /// `DocStorePolicy`; §2.14 names the item skip `policy_refused`).
    #[error("{reason}")]
    PolicyRefused {
        /// The directory as given, or as resolved when resolution succeeded.
        path: PathBuf,
        /// Why it is not permitted.
        reason: String,
    },
}

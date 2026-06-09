//! Filesystem tools for the tcode tool registry.
//!
//! Why: AI coding loops (e.g. the ai-coding-bake-off L1 agentic loop) need the
//! agent to create, read, and patch files on the host filesystem. Providing typed
//! `ToolExecutor` implementations instead of raw shell-exec keeps error handling
//! structured, scoping to a working directory enforced, and path traversal
//! impossible.
//! What: Three `ToolExecutor` impls — `ReadFileTool`, `WriteFileTool`, and
//! `EditTool` — each with an OpenAI-function-call schema and registration helpers.
//! Errors use `thiserror` for clean structured error types.
//! Test: `read.rs`, `write.rs`, and `edit.rs` each carry tempdir-based unit
//! tests; `registry_tests` verifies all three appear in `schemas()` and dispatch
//! through `dispatch_gated`.

pub mod edit;
pub mod read;
pub mod write;

pub use edit::EditTool;
pub use read::ReadFileTool;
pub use write::WriteFileTool;

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors produced by filesystem tools.
///
/// Why: A single error type for all three FS tools makes calling code concise
/// and lets `ToolResult::err` convert cleanly via `Display`.
/// What: Variants cover all failure modes: traversal, missing file, size cap,
/// IO, and unique-match violations.
/// Test: Each variant is exercised by at least one unit test in the tool modules.
#[derive(Debug, Error)]
pub enum FsError {
    /// The requested path would escape the permitted working directory.
    #[error("path escapes working directory: {0}")]
    PathTraversal(PathBuf),

    /// The file or directory was not found.
    #[error("not found: {0}")]
    NotFound(PathBuf),

    /// The file exceeds the maximum allowed read size.
    #[error("file too large ({bytes} bytes, max {max} bytes): {path}")]
    FileTooLarge { path: PathBuf, bytes: u64, max: u64 },

    /// `edit` found zero occurrences of `old_string`.
    #[error("edit: old_string not found in {path}")]
    EditNotFound { path: PathBuf },

    /// `edit` found more than one occurrence of `old_string`.
    #[error("edit: old_string is ambiguous ({count} matches) in {path}")]
    EditAmbiguous { path: PathBuf, count: usize },

    /// Underlying IO error.
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl FsError {
    /// Wrap an `std::io::Error` with a path context.
    ///
    /// Why: Every IO call needs the affected path in the error message.
    /// What: Boxes the `std::io::Error` as `FsError::Io { path, source }`.
    /// Test: Used internally by all three tool modules.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        FsError::Io {
            path: path.into(),
            source,
        }
    }
}

/// Resolve `raw_path` relative to `base_dir`, canonicalizing both and asserting
/// the result is strictly inside `base_dir`.
///
/// Why: The tool arguments come from an LLM. Normalising before the prefix check
/// collapses `..` components so `../../etc/passwd` cannot escape the sandbox.
/// Absolute paths that resolve inside `base_dir` are accepted (the bake-off uses
/// absolute output dirs built from the working dir).
/// What: Returns the canonical absolute path on success. Returns
/// `FsError::PathTraversal` if the resolved path is not prefixed by `base_dir`.
/// On macOS, `/var/folders/…` is a symlink to `/private/var/…`; we canonicalize
/// the base dir AND resolve as much of the candidate as possible (by
/// canonicalizing the longest existing prefix) so both sides use the same
/// physical root.
/// Test: `read::tests::path_traversal_is_rejected`,
///       `write::tests::path_traversal_is_rejected`,
///       `edit::tests::path_traversal_is_rejected`.
pub(crate) fn scoped_path(base_dir: &Path, raw_path: &Path) -> Result<PathBuf, FsError> {
    // Build the candidate before canonicalising — the file may not exist yet.
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        base_dir.join(raw_path)
    };

    // Canonicalize the base_dir so symlinks are resolved (e.g. /var → /private/var on macOS).
    let canonical_base = std::fs::canonicalize(base_dir).map_err(|e| FsError::io(base_dir, e))?;

    // Resolve the candidate using the same symlink-aware approach: canonicalize
    // the longest existing ancestor, then append the non-existing tail.
    let resolved = resolve_candidate(&canonical_base, &joined)?;

    if !resolved.starts_with(&canonical_base) {
        return Err(FsError::PathTraversal(raw_path.to_path_buf()));
    }

    Ok(resolved)
}

/// Resolve a candidate path that may not fully exist by canonicalizing its
/// longest existing ancestor and appending the non-existing tail.
///
/// Why: `std::fs::canonicalize` fails on non-existent paths; `write_file` needs
/// to validate paths for files that do not exist yet. By walking up until we find
/// an existing prefix, we resolve symlinks in that prefix, then normalise the
/// non-existing tail lexically.
/// What: Returns a fully-resolved `PathBuf`. Any `..` in the non-existing tail
/// is expanded by `normalize_path`.
/// Test: Exercised by `write_file` tests (new files) and traversal-guard tests.
fn resolve_candidate(_canonical_base: &Path, candidate: &Path) -> Result<PathBuf, FsError> {
    // Try to canonicalize the full path first (handles the file-exists case).
    if let Ok(c) = std::fs::canonicalize(candidate) {
        return Ok(c);
    }

    // Walk up to find the longest existing ancestor.
    let mut existing = candidate.to_path_buf();
    let mut tail = std::collections::VecDeque::new();

    loop {
        if existing.exists() {
            break;
        }
        match existing.file_name() {
            Some(name) => {
                tail.push_front(name.to_owned());
                existing = existing
                    .parent()
                    .unwrap_or(existing.as_path())
                    .to_path_buf();
            }
            None => break,
        }
    }

    // Canonicalize whatever we found (may be the fs root in pathological cases).
    let mut resolved = if existing.exists() {
        std::fs::canonicalize(&existing).unwrap_or(existing)
    } else {
        existing
    };

    for component in tail {
        resolved.push(component);
    }

    // A final lexical normalize in case the tail had `..`.
    Ok(normalize_path(&resolved))
}

/// Lexically normalize a path by resolving `.` and `..` components without
/// touching the filesystem (no `canonicalize` call).
///
/// Why: `std::fs::canonicalize` requires the path to exist; for `write_file` the
/// target may not yet exist. This function produces a clean absolute path that
/// `starts_with` checks work correctly on.
/// What: Iterates path components, skipping `.` and backtracking on `..`, then
/// reconstructs into a `PathBuf`.
/// Test: Exercised indirectly via `scoped_path` in traversal-guard tests.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Prefix(p) => components.push(Component::Prefix(p).as_os_str().to_owned()),
            Component::RootDir => {
                components.clear();
                components.push(std::ffi::OsString::from("/"));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop if there is a non-root component to remove.
                let last_is_root = components
                    .last()
                    .map(|c| c.as_encoded_bytes() == b"/")
                    .unwrap_or(false);
                if !components.is_empty() && !last_is_root {
                    components.pop();
                }
            }
            Component::Normal(n) => components.push(n.to_owned()),
        }
    }

    // Reconstruct.
    let mut out = PathBuf::new();
    for (i, c) in components.iter().enumerate() {
        if i == 0 && c.as_encoded_bytes() == b"/" {
            out.push("/");
        } else {
            out.push(c);
        }
    }
    out
}

// ── Tests for the shared helpers ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// `scoped_path` accepts a relative path inside the working dir.
    ///
    /// Why: Happy-path guard for the traversal helper.
    /// What: `scoped_path(tmp, "foo.txt")` returns a path under the canonical tmp.
    /// Test: This test.
    #[test]
    fn scoped_path_accepts_relative_inside_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Use the canonical form for comparison — on macOS /var is a symlink to
        // /private/var, so `tmp.path()` and the resolved path differ in prefix.
        let canonical_tmp = std::fs::canonicalize(tmp.path()).expect("canonicalize tmp");
        let resolved = scoped_path(tmp.path(), Path::new("foo.txt")).expect("should accept");
        assert!(
            resolved.starts_with(&canonical_tmp),
            "resolved {resolved:?} must be inside canonical tmp {canonical_tmp:?}"
        );
    }

    /// `scoped_path` rejects an attempt to escape via `../`.
    ///
    /// Why: Core traversal guard contract.
    /// What: `scoped_path(tmp, "../etc/passwd")` returns `FsError::PathTraversal`.
    /// Test: This test.
    #[test]
    fn scoped_path_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = scoped_path(tmp.path(), Path::new("../etc/passwd"));
        assert!(
            matches!(err, Err(FsError::PathTraversal(_))),
            "expected PathTraversal, got {err:?}"
        );
    }

    /// `scoped_path` accepts an absolute path that resolves inside the working dir.
    ///
    /// Why: The bake-off uses absolute output paths built from the working dir.
    /// What: An absolute path whose canonical form is a child of `base_dir` is OK.
    /// Test: This test.
    #[test]
    fn scoped_path_accepts_absolute_inside_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let canonical_tmp = std::fs::canonicalize(tmp.path()).expect("canonicalize tmp");
        // Build the absolute path using the canonical form so it passes the check.
        let abs = canonical_tmp.join("subdir").join("file.py");
        let resolved = scoped_path(tmp.path(), &abs).expect("should accept absolute inside dir");
        assert!(
            resolved.starts_with(&canonical_tmp),
            "resolved {resolved:?} must be inside canonical tmp {canonical_tmp:?}"
        );
    }

    /// `scoped_path` rejects an absolute path that is outside the working dir.
    ///
    /// Why: Absolute paths not rooted in `base_dir` must be blocked.
    /// What: `/etc/passwd` is not under `tmp` — `PathTraversal` is returned.
    /// Test: This test.
    #[test]
    fn scoped_path_rejects_absolute_outside_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = scoped_path(tmp.path(), Path::new("/etc/passwd"));
        assert!(
            matches!(err, Err(FsError::PathTraversal(_))),
            "expected PathTraversal, got {err:?}"
        );
    }
}

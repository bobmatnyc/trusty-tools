//! Path-resolution helpers and result-bounding constants shared by the
//! read-only filesystem tools.
//!
//! Why: All three explorer tools (`read_file`, `list_dir`, `grep_files`) must
//! enforce the same "stay inside the working directory" rule and the same
//! result-size caps. Centralizing them here keeps the policy in one place.
//! What: Exposes the truncation/result-cap constants plus `resolve_within_cwd`.
//! Test: `super::resolve_rejects_traversal` / `super::resolve_accepts_subpath`
//! in the parent module's test block.

use std::path::{Path, PathBuf};

/// Maximum characters returned by `read_file` before truncation.
pub(super) const READ_FILE_MAX_CHARS: usize = 50_000;
/// Default `max_results` for `grep_files`.
pub(super) const GREP_DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `max_results` to bound result size regardless of caller.
pub(super) const GREP_HARD_CAP: usize = 500;

/// Resolve a user-supplied path relative to CWD and verify it stays inside.
///
/// Why: Security — an LLM could (accidentally or deliberately) request paths
/// like `../../etc/passwd`. Canonicalizing both the CWD and the candidate
/// then comparing prefixes keeps all reads inside the working directory.
/// What: Returns the canonicalized target path on success, or an error string
/// describing the rejection.
/// Test: See `resolve_rejects_traversal` and `resolve_accepts_subpath`.
pub(super) fn resolve_within_cwd(path: &str) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("could not resolve CWD: {e}"))?;
    let cwd_canon = cwd
        .canonicalize()
        .map_err(|e| format!("could not canonicalize CWD {}: {e}", cwd.display()))?;

    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };

    // If the path doesn't exist yet (unlikely for read-only tools) we still
    // need to reject traversal. canonicalize() fails on missing paths, so
    // walk the path component-by-component instead for that case; for
    // existing paths, canonicalize is strictly better (resolves symlinks).
    let canon = match candidate.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return Err(format!(
                "path does not exist or is unreadable: {}: {e}",
                candidate.display()
            ));
        }
    };

    if !canon.starts_with(&cwd_canon) {
        return Err(format!(
            "path escapes working directory: {} (cwd: {})",
            canon.display(),
            cwd_canon.display()
        ));
    }

    Ok(canon)
}

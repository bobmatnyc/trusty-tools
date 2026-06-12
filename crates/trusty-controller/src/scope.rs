//! Project-scope detection helpers.
//!
//! Why: DOC-3 §3 requires the default `--scope` to depend on whether the
//! working directory is inside a project (git root found → `all`, else
//! `system`). Centralising this detection keeps all command handlers
//! consistent and avoids re-implementing the walk in multiple places.
//!
//! What: Exposes `detect_default_scope` — walks up from the provided directory
//! to the first `.git` root or recognised tool marker. Returns `ScopeArg::All`
//! when inside a project, `ScopeArg::System` otherwise.  The full project-identity
//! helpers (`detect_project` + `id_from_path`) will be hoisted into
//! `trusty_common` per DOC-6 §7 / ADR-0008; this module is a Phase-0 stub that
//! replicates just enough to satisfy the default-scope rule.
//!
//! Test: `detect_default_scope` on this repo's root (which contains `.git`)
//! returns `ScopeArg::All`; on `/tmp` (no `.git`) returns `ScopeArg::System`.

use std::path::Path;

use crate::cli::ScopeArg;

/// Detect the default scope for a command given the current working directory.
///
/// Why: DOC-3 §3 requires scope to default to `all` inside a project directory
/// and `system` outside one, so every command handler can call this when the
/// user has not supplied an explicit `--scope`.
///
/// What: Walks up from `cwd` looking for a `.git` directory. If found, the
/// working directory is considered "inside a project" and `ScopeArg::All` is
/// returned; otherwise `ScopeArg::System`.
///
/// Test: Pass the repo root (containing `.git`) → returns `All`; pass `/tmp`
/// → returns `System`.
pub fn detect_default_scope(cwd: &Path) -> ScopeArg {
    if has_git_root(cwd) {
        ScopeArg::All
    } else {
        ScopeArg::System
    }
}

/// Resolve the effective scope: use the explicit value if provided, else detect.
///
/// Why: Every command handler needs an unambiguous scope value, but the user
/// may or may not have supplied `--scope`. This helper collapses both paths.
///
/// What: Returns the provided `explicit` scope unchanged when set; otherwise
/// calls `detect_default_scope(cwd)`.
///
/// Test: `resolve_scope(Some(ScopeArg::System), _)` == `ScopeArg::System`
/// regardless of `cwd`. `resolve_scope(None, repo_root)` == `ScopeArg::All`.
pub fn resolve_scope(explicit: Option<ScopeArg>, cwd: &Path) -> ScopeArg {
    explicit.unwrap_or_else(|| detect_default_scope(cwd))
}

/// Return `true` if any ancestor of `cwd` (inclusive) contains a `.git` entry.
///
/// Why: This is the first step of the DOC-3 / ADR-0008 project-detection walk.
/// The full walk (marker files, fallback) is deferred to when
/// `trusty_common::detect_project` is hoisted there (DOC-6 §7).
///
/// What: Iterates through ancestor directories, returning `true` as soon as a
/// `.git` directory or file (worktree case) is found. Returns `false` if the
/// root is reached without a match.
///
/// Test: `has_git_root(Path::new("/tmp"))` == `false`;
/// `has_git_root(env!("CARGO_MANIFEST_DIR"))` == `true` (this repo has `.git`).
fn has_git_root(cwd: &Path) -> bool {
    let mut current = cwd;
    loop {
        if current.join(".git").exists() {
            return true;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A path containing `.git` → `ScopeArg::All`.
    #[test]
    fn git_root_gives_all() {
        // The workspace root is a git repo; CARGO_MANIFEST_DIR is inside it.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(detect_default_scope(&manifest_dir), ScopeArg::All);
    }

    /// `/tmp` has no `.git` → `ScopeArg::System`.
    #[test]
    fn no_git_root_gives_system() {
        assert_eq!(detect_default_scope(Path::new("/tmp")), ScopeArg::System);
    }

    /// Explicit scope always wins regardless of `cwd`.
    #[test]
    fn explicit_scope_wins() {
        // Even though /tmp has no .git, explicit System is honoured.
        assert_eq!(
            resolve_scope(Some(ScopeArg::System), Path::new("/tmp")),
            ScopeArg::System
        );
        // Even though the manifest dir has .git, explicit Project is honoured.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            resolve_scope(Some(ScopeArg::Project), &manifest_dir),
            ScopeArg::Project
        );
    }

    /// `resolve_scope(None, _)` falls through to detection.
    #[test]
    fn none_scope_detects() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(resolve_scope(None, &manifest_dir), ScopeArg::All);
        assert_eq!(resolve_scope(None, Path::new("/tmp")), ScopeArg::System);
    }
}

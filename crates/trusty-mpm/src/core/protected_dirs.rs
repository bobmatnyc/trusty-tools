//! Guards against traversing `$HOME` or macOS TCC-protected user folders.
//!
//! Why: tm's remit is git worktrees under the managed projects root — it never
//! legitimately walks, watches, or runs a session in the user's home directory
//! or a TCC-protected folder (Downloads, Desktop, Documents, Pictures, Movies,
//! Music, `~/Library`). When it does, macOS surfaces a cascade of "trusty-mpm
//! would like to access your Downloads/Photos/… folder" consent prompts — one
//! per protected subfolder — because a recursive watch/scan of `$HOME` touches
//! each guarded folder in turn (issue #2758). The two culprits are (a) the file
//! watcher registering a RECURSIVE `notify` watch on a session workdir and
//! (b) a managed session whose cwd defaulted to `$HOME` (no explicit cwd). This
//! module centralises the "is this a home/protected root?" decision so both call
//! sites — and any future scan — share one definition rather than drifting.
//! What: [`PROTECTED_HOME_SUBDIRS`] lists the guarded top-level folders;
//! [`is_home_or_protected_dir`] is the pure lexical predicate; [`safe_session_cwd`]
//! resolves a managed session's cwd to a value that is never `$HOME`/protected/`/`.
//! Test: unit tests in this module.

use std::path::{Path, PathBuf};

use tracing::warn;

/// Top-level `$HOME` subdirectories that macOS guards behind a per-folder TCC
/// consent prompt, plus the broad media/`Library` folders a code tool never
/// wants to traverse.
///
/// Why: recursively watching/scanning any of these — or `$HOME` itself — makes
/// macOS pop a "would like to access …" folder prompt and drags the whole home
/// tree into the operation. `Pictures` covers the Photos Library
/// (`kTCCServicePhotos`); `Downloads`/`Desktop`/`Documents` are the classic
/// `SystemPolicy*Folder` services.
/// What: bare directory base-names joined onto the resolved home directory by
/// [`is_home_or_protected_dir`].
/// Test: `is_home_or_protected_dir_rejects_home_and_tcc_dirs`.
pub const PROTECTED_HOME_SUBDIRS: &[&str] = &[
    "Downloads",
    "Desktop",
    "Documents",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "Public",
];

/// True when `path` must NEVER be used as a recursive scan/watch/run root.
///
/// Why: a recursive traversal rooted at `$HOME` (or a TCC-protected subfolder)
/// triggers the macOS folder-access prompt cascade described in the module docs.
/// What: returns true when `path` is the filesystem root `/`, equals `home`, or
/// equals `home/<one of PROTECTED_HOME_SUBDIRS>`. Comparison is purely lexical —
/// it never stats or canonicalizes `path`, so this check can never itself trip a
/// TCC prompt. A path that merely lives *inside* one of these folders (deeper
/// than the bare directory — an operator's explicit choice) is NOT rejected.
/// When `home` is `None` only `/` is rejected.
/// Test: `is_home_or_protected_dir_rejects_home_and_tcc_dirs`,
/// `is_home_or_protected_dir_allows_normal_project`.
pub fn is_home_or_protected_dir(path: &Path, home: Option<&Path>) -> bool {
    if path == Path::new("/") {
        return true;
    }
    let Some(home) = home else {
        return false;
    };
    if path == home {
        return true;
    }
    PROTECTED_HOME_SUBDIRS
        .iter()
        .any(|name| path == home.join(name))
}

/// Resolve a managed session's working directory to a value that is never
/// `$HOME`, a TCC-protected folder, or `/`.
///
/// Why: [`crate::session_manager`] previously defaulted a session with no
/// explicit cwd to `dirs::home_dir()`. Such a session runs (and is recursively
/// watched) in `$HOME`, causing the macOS folder-access prompt cascade. Routing
/// every managed cwd through this helper guarantees a session never traverses the
/// home tree, keeping scan/watch/run roots explicit rather than `$HOME`-wide.
/// What: returns `cwd` unchanged when it is `Some` and NOT a home/protected/`/`
/// root; otherwise (missing, or resolving to such a root) logs a warning and
/// returns a neutral per-process scratch directory ([`std::env::temp_dir`]). The
/// scratch dir is writable and outside every TCC-protected location.
/// Test: `safe_session_cwd_replaces_home_and_missing`,
/// `safe_session_cwd_keeps_explicit_project_dir`.
pub fn safe_session_cwd(cwd: Option<PathBuf>) -> PathBuf {
    let home = dirs::home_dir();
    match cwd {
        Some(p) if !is_home_or_protected_dir(&p, home.as_deref()) => p,
        other => {
            let fallback = std::env::temp_dir();
            match other {
                Some(p) => warn!(
                    requested = %p.display(),
                    fallback = %fallback.display(),
                    "managed session cwd resolves to $HOME or a TCC-protected folder; \
                     using a scratch dir to avoid recursive home traversal / macOS \
                     folder-access prompts"
                ),
                None => warn!(
                    fallback = %fallback.display(),
                    "no managed session cwd supplied; using a scratch dir instead of $HOME"
                ),
            }
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_home_or_protected_dir_rejects_home_and_tcc_dirs() {
        let home = Path::new("/Users/tester");
        assert!(is_home_or_protected_dir(Path::new("/"), Some(home)));
        assert!(is_home_or_protected_dir(home, Some(home)));
        for d in PROTECTED_HOME_SUBDIRS {
            assert!(
                is_home_or_protected_dir(&home.join(d), Some(home)),
                "bare ~/{d} must be rejected"
            );
        }
        // A trailing slash still matches (Path comparison is component-wise).
        assert!(is_home_or_protected_dir(
            Path::new("/Users/tester/Downloads/"),
            Some(home)
        ));
    }

    #[test]
    fn is_home_or_protected_dir_allows_normal_project() {
        let home = Path::new("/Users/tester");
        // The managed projects root and its worktrees are always allowed.
        assert!(!is_home_or_protected_dir(
            Path::new("/Users/tester/trusty-mpm-projects/owner/repo/.worktrees/x"),
            Some(home)
        ));
        // A project living INSIDE Downloads (deeper than the bare folder) is an
        // explicit operator choice and is NOT rejected.
        assert!(!is_home_or_protected_dir(
            &home.join("Downloads").join("my-project"),
            Some(home)
        ));
        // With no resolvable home only "/" is rejected.
        assert!(!is_home_or_protected_dir(
            Path::new("/Users/tester/Downloads"),
            None
        ));
    }

    #[test]
    fn safe_session_cwd_keeps_explicit_project_dir() {
        let p = PathBuf::from("/Users/tester/trusty-mpm-projects/o/r/.worktrees/s");
        assert_eq!(safe_session_cwd(Some(p.clone())), p);
    }

    #[test]
    fn safe_session_cwd_replaces_home_and_missing() {
        let scratch = std::env::temp_dir();
        // Missing cwd → scratch, never $HOME.
        assert_eq!(safe_session_cwd(None), scratch);
        if let Some(home) = dirs::home_dir() {
            // Explicit $HOME → scratch.
            assert_eq!(safe_session_cwd(Some(home.clone())), scratch);
            // Explicit ~/Downloads → scratch.
            assert_eq!(safe_session_cwd(Some(home.join("Downloads"))), scratch);
        }
        // Filesystem root → scratch.
        assert_eq!(safe_session_cwd(Some(PathBuf::from("/"))), scratch);
    }
}

//! Path-containment guard for workspace deletion (#1511).
//!
//! Why: `SessionManager::decommission` previously `remove_dir_all`'d
//! `workspace_path` unconditionally, which deleted a live user repo when the
//! #1502 local-path spawn set `workspace_path` to a real on-disk directory.
//! This module provides the belt-and-suspenders containment guard that prevents
//! any path OUTSIDE the SM's managed workspace root from being deleted —
//! regardless of the `workspace_owned` flag.
//! What: [`is_safe_to_remove`] canonicalizes both paths and verifies that the
//! workspace is strictly INSIDE the managed root, rejecting: path == root, path
//! outside root, paths with too few components, and `$HOME`.
//! Test: `is_safe_to_remove_*` unit tests below.

use std::path::Path;

use tracing::warn;

/// Decide whether `workspace_path` is safe to `remove_dir_all` (#1511).
///
/// Why: even an `workspace_owned = true` record should only be deleted when the
/// path is strictly INSIDE the SM's managed workspaces root. This prevents
/// decommission from deleting a directory if `workspace_owned` were stale or if a
/// bug let a real path slip through as "owned". The guard rejects: path == root,
/// path outside root, path with too few components (filesystem / volume root), and
/// the user's home directory.
///
/// What: canonicalizes both paths and checks that `workspace_path` starts with
/// `managed_root` AND is not equal to it (strictly INSIDE). Also rejects any path
/// that is `$HOME` or has fewer than 3 components (e.g. `/`, `/tmp`) as an extra
/// safety net against catastrophic deletion. Returns `false` on any I/O error
/// during canonicalization (e.g. path does not exist) — never errors out.
///
/// Test: `is_safe_to_remove_rejects_outside_root`, `is_safe_to_remove_rejects_root_itself`,
/// `is_safe_to_remove_rejects_home`, `is_safe_to_remove_rejects_shallow_path`,
/// `is_safe_to_remove_accepts_valid_child` in this module.
pub(crate) fn is_safe_to_remove(workspace_path: &Path, managed_root: &Path) -> bool {
    // Reject suspiciously shallow paths before trying to canonicalize them.
    // A real SM-provisioned workspace always nests at least 3 segments under
    // the root (e.g. <root>/<owner>/<repo>/<session-id>/).
    let component_count = workspace_path.components().count();
    if component_count < 3 {
        warn!(
            path = %workspace_path.display(),
            components = component_count,
            "is_safe_to_remove: rejecting — too few path components"
        );
        return false;
    }

    // Reject $HOME outright — no managed session should ever BE the home dir.
    if dirs::home_dir().is_some_and(|home| workspace_path == home) {
        warn!(
            path = %workspace_path.display(),
            "is_safe_to_remove: rejecting — path is $HOME"
        );
        return false;
    }

    // Canonicalize both paths to resolve symlinks and `..` so a symlink into
    // the managed root (or vice-versa) cannot trick the prefix check.
    let canon_ws = match workspace_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                path = %workspace_path.display(),
                "is_safe_to_remove: cannot canonicalize workspace path ({e}); skipping deletion"
            );
            return false;
        }
    };
    let canon_root = match managed_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                root = %managed_root.display(),
                "is_safe_to_remove: cannot canonicalize managed root ({e}); skipping deletion"
            );
            return false;
        }
    };

    // Must be strictly INSIDE the root (starts_with AND not equal to root).
    if canon_ws == canon_root {
        warn!(
            path = %workspace_path.display(),
            "is_safe_to_remove: rejecting — path IS the managed root"
        );
        return false;
    }
    if !canon_ws.starts_with(&canon_root) {
        warn!(
            path = %workspace_path.display(),
            root = %managed_root.display(),
            "is_safe_to_remove: rejecting — path is outside the managed root"
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::is_safe_to_remove;

    // ── is_safe_to_remove unit tests (#1511) ────────────────────────────────

    /// A valid child of the managed root passes the guard.
    ///
    /// Why: this is the happy path — an SM-provisioned workspace is always
    /// nested under the managed root.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_accepts_valid_child() {
        let root = TempDir::new().unwrap();
        let child = root.path().join("owner").join("repo").join("session-abc");
        std::fs::create_dir_all(&child).unwrap();
        assert!(
            is_safe_to_remove(&child, root.path()),
            "a path strictly inside the managed root must pass"
        );
    }

    /// A path equal to the managed root is rejected.
    ///
    /// Why: deleting the root itself would wipe all managed workspaces at once.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_root_itself() {
        let root = TempDir::new().unwrap();
        assert!(
            !is_safe_to_remove(root.path(), root.path()),
            "the managed root itself must be rejected"
        );
    }

    /// A path outside the managed root is rejected even if it exists.
    ///
    /// Why: a stale or stale `workspace_owned` flag must not cause out-of-root
    /// deletion.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_outside_root() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap(); // different temp dir
        let outside_child = outside.path().join("some").join("path");
        std::fs::create_dir_all(&outside_child).unwrap();
        assert!(
            !is_safe_to_remove(&outside_child, root.path()),
            "a path outside the managed root must be rejected"
        );
    }

    /// A path with fewer than 3 components is rejected.
    ///
    /// Why: `/`, `/tmp`, or a single-segment path is never a valid SM workspace.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_shallow_path() {
        let root = TempDir::new().unwrap();
        // A 1-component path like "/" or a 2-component path like "/tmp" must be
        // rejected before even reaching canonicalization.
        let shallow = PathBuf::from("/tmp");
        assert!(
            !is_safe_to_remove(&shallow, root.path()),
            "a shallow path (/tmp) must be rejected by the component-count guard"
        );
    }

    /// `$HOME` is rejected outright.
    ///
    /// Why: deleting the user's home directory is catastrophic and must be
    /// impossible regardless of what the managed root is set to.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_home() {
        if let Some(home) = dirs::home_dir() {
            // Use home as both path and root — even if "home is inside home" the
            // home-directory check fires first and rejects.
            assert!(
                !is_safe_to_remove(&home, &home),
                "$HOME must always be rejected"
            );
        }
    }

    /// A non-existent path returns false (canonicalize fails).
    ///
    /// Why: a workspace that has already been deleted must not cause a panic;
    /// the guard should simply return false so decommission logs and moves on.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_returns_false_for_nonexistent_path() {
        let root = TempDir::new().unwrap();
        let nonexistent = root.path().join("ghost").join("nope").join("absent");
        // path does not exist → canonicalize fails → returns false
        assert!(
            !is_safe_to_remove(&nonexistent, root.path()),
            "a non-existent workspace path must return false (canonicalize fails)"
        );
    }
}

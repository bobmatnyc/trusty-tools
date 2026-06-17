//! Project auto-detection for trusty-search.
//!
//! Why: Users should be able to run `trusty-search search "foo"` from anywhere
//! inside a project tree without manually specifying an index name. This module
//! walks up the directory tree looking for `.git` or a `.trusty-search` marker
//! to identify the current project context.
//!
//! What: Provides `detect_project()` which returns a `ProjectContext` containing
//! the inferred index ID, project root, and detection method used.
//!
//! Test: Create a temp directory with a `.git` subdirectory, call detect_project()
//! from a nested path, assert the returned root and detection_method::GitRoot.

use std::path::{Path, PathBuf};

/// Detected project context from the current working directory.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub index_id: String,
    pub root_path: PathBuf,
    pub detection_method: DetectionMethod,
}

/// How the project was detected — drives whether to warn the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionMethod {
    /// Found a `.git` directory walking up from CWD.
    GitRoot,
    /// Found a `.trusty-search` marker file walking up from CWD.
    MarkerFile,
    /// No marker found — used CWD basename (warn the user).
    Fallback,
}

/// Walk up from `start` looking for a `.git` directory or `.trusty-search` marker.
///
/// Why: Centralizes project-root inference so every command (search, watch,
/// status, etc.) shares the same detection logic.
/// What: Returns the detected `ProjectContext`, falling back to the CWD basename
/// if no marker is found.
/// Test: Pass a path inside a `.git`-rooted tree → assert GitRoot. Pass a path
/// with no markers → assert Fallback and that index_id == basename.
pub fn detect_project(start: &Path) -> ProjectContext {
    let mut current = start.to_path_buf();
    loop {
        // Prefer .git as the strongest signal of a project root.
        if current.join(".git").exists() {
            // Single source of truth (#1373): derive the id via the shared
            // `trusty_common::derive_index_id` so trusty-mpm's register-and-pin
            // and this CLI path always produce the identical index id.
            let name = trusty_common::derive_index_id(&current);
            return ProjectContext {
                index_id: name,
                root_path: current,
                detection_method: DetectionMethod::GitRoot,
            };
        }
        // Then check for an explicit trusty-search marker.
        if current.join(".trusty-search").exists() {
            let name = trusty_common::derive_index_id(&current);
            return ProjectContext {
                index_id: name,
                root_path: current,
                detection_method: DetectionMethod::MarkerFile,
            };
        }
        if !current.pop() {
            break;
        }
    }
    // Fallback: use CWD basename so commands still have something to call the index.
    let cwd = start.to_path_buf();
    let name = trusty_common::derive_index_id(&cwd);
    ProjectContext {
        index_id: name,
        root_path: cwd,
        detection_method: DetectionMethod::Fallback,
    }
}

/// Why: the tests below assert that the derived `index_id` equals the path
/// basename; production code now derives ids via `trusty_common::derive_index_id`
/// (#1373), so this local helper is test-only.
/// What: Returns the final path component as a `String`, lossy on non-UTF8.
/// Test: basename(Path::new("/foo/bar")) == "bar"; empty path returns "".
#[cfg(test)]
fn basename(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_detects_git_root() {
        let tmp = tempdir_unique("detect-git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let ctx = detect_project(&nested);
        assert_eq!(ctx.detection_method, DetectionMethod::GitRoot);
        assert_eq!(ctx.root_path, tmp);
        assert_eq!(ctx.index_id, basename(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_detects_marker_file() {
        let tmp = tempdir_unique("detect-marker");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join(".trusty-search"), "index = \"x\"\n").unwrap();
        let nested = tmp.join("sub");
        fs::create_dir_all(&nested).unwrap();

        let ctx = detect_project(&nested);
        assert_eq!(ctx.detection_method, DetectionMethod::MarkerFile);
        assert_eq!(ctx.root_path, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_falls_back_to_cwd_basename() {
        let tmp = tempdir_unique("detect-fallback");
        fs::create_dir_all(&tmp).unwrap();

        let ctx = detect_project(&tmp);
        assert_eq!(ctx.detection_method, DetectionMethod::Fallback);
        assert_eq!(ctx.index_id, basename(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    fn tempdir_unique(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("trusty-{}-{}-{}", label, pid, nanos));
        let _ = std::fs::remove_dir_all(&p);
        p
    }
}

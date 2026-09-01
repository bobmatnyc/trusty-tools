//! Project auto-detection for trusty-search.
//!
//! Why: Users should be able to run `trusty-search search "foo"` from anywhere
//! inside a project tree without manually specifying an index name. This module
//! walks up the directory tree looking for `.git` or a `.trusty-search` marker
//! to identify the current project context.
//!
//! What: Provides `detect_project()` which returns a `ProjectContext` containing
//! the inferred index ID, project root, and detection method used — or refuses,
//! when the walk resolves to a directory that names no project (#6550).
//!
//! Test: Create a temp directory with a `.git` subdirectory, call detect_project()
//! from a nested path, assert the returned root and detection_method::GitRoot;
//! `detect_project_refuses_the_home_directory` covers the refusal.

use std::path::{Path, PathBuf};
use trusty_common::IndexRootRefusal;

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

/// Auto-detection resolved a root that must not become an index (#6550).
///
/// Why: the fallback arm below returns the START path when no marker is found,
/// and `derive_index_id` takes its basename — so running a project-scoped
/// command from `$HOME` derived index `masa`, a well-formed id naming the
/// operator rather than any project. The caller cannot tell that id apart from
/// a correct one, so detection refuses instead of guessing, and the operator
/// names the index explicitly with `--index`.
/// What: carries the refused root and which of the two refusals fired. Its
/// `Display` is the user-facing CLI message.
/// Test: `detect_project_refuses_the_home_directory`,
/// `detect_project_refuses_the_filesystem_root`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "refusing to derive an index id from {path}: it is {refusal}, which names no project. \
     Run this command from inside the project, or pass `--index <id>`."
)]
pub struct ProjectRootRefused {
    /// The root the upward walk resolved to.
    pub path: PathBuf,
    /// Why it was refused.
    pub refusal: IndexRootRefusal,
}

/// Walk up from `start` looking for a `.git` directory or `.trusty-search` marker.
///
/// Why: Centralizes project-root inference so every command (search, watch,
/// status, etc.) shares the same detection logic.
/// What: Returns the detected `ProjectContext`, falling back to the start-path
/// basename if no marker is found — except when the resolved root is one that
/// names no project, which is refused rather than guessed (#6550, see
/// [`ProjectRootRefused`]). The refusal is checked against the resolved root,
/// so it also covers a `$HOME` that IS a git repository or carries a
/// `.trusty-search` marker: the id would be the operator's name either way.
/// Test: Pass a path inside a `.git`-rooted tree → assert GitRoot. Pass a path
/// with no markers → assert Fallback and that index_id == basename.
/// `detect_project_refuses_the_home_directory` covers the refusal.
pub fn detect_project(start: &Path) -> Result<ProjectContext, ProjectRootRefused> {
    detect_project_against(start, dirs::home_dir().as_deref())
}

/// [`detect_project`] with the home directory supplied by the caller.
///
/// Why: `$HOME` is process-wide state a test cannot vary without racing every
/// other thread in the binary, so the refusal decision takes it as an argument
/// and [`detect_project`] owns the one lookup. Same split as
/// `trusty_common::refuse_unindexable_root_against`, which this delegates to.
/// What: performs the upward walk, then refuses or derives. Never reads the
/// environment itself.
/// Test: `detect_project_refuses_the_home_directory`,
/// `detect_project_indexes_a_project_under_the_home_directory`.
pub fn detect_project_against(
    start: &Path,
    home: Option<&Path>,
) -> Result<ProjectContext, ProjectRootRefused> {
    let (root_path, detection_method) = walk_for_root(start);
    // #6550: refuse before deriving — a basename looks equally plausible for a
    // real project root and for a directory that identifies no project at all.
    if let Some(refusal) = trusty_common::refuse_unindexable_root_against(&root_path, home) {
        return Err(ProjectRootRefused {
            path: root_path,
            refusal,
        });
    }
    // Single source of truth (#1373): derive the id via the shared
    // `trusty_common::derive_index_id` so trusty-mpm's register-and-pin and this
    // CLI path always produce the identical index id.
    Ok(ProjectContext {
        index_id: trusty_common::derive_index_id(&root_path),
        root_path,
        detection_method,
    })
}

/// The upward marker walk, with no derivation and no refusal.
///
/// Why/What/Test: see [`detect_project_against`], the only caller — split out so
/// the walk has exactly one exit and the refusal cannot be bypassed by a future
/// arm that forgets it.
fn walk_for_root(start: &Path) -> (PathBuf, DetectionMethod) {
    let mut current = start.to_path_buf();
    loop {
        // Prefer .git as the strongest signal of a project root.
        if current.join(".git").exists() {
            return (current, DetectionMethod::GitRoot);
        }
        // Then check for an explicit trusty-search marker.
        if current.join(".trusty-search").exists() {
            return (current, DetectionMethod::MarkerFile);
        }
        if !current.pop() {
            break;
        }
    }
    // Fallback: use the start path so commands still have something to call the
    // index — subject to the caller's refusal check.
    (start.to_path_buf(), DetectionMethod::Fallback)
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

        let ctx = detect_project(&nested).expect("a git-rooted project is indexable");
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

        let ctx = detect_project(&nested).expect("a marker-rooted project is indexable");
        assert_eq!(ctx.detection_method, DetectionMethod::MarkerFile);
        assert_eq!(ctx.root_path, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_falls_back_to_cwd_basename() {
        let tmp = tempdir_unique("detect-fallback");
        fs::create_dir_all(&tmp).unwrap();

        let ctx = detect_project(&tmp).expect("an ordinary directory is indexable");
        assert_eq!(ctx.detection_method, DetectionMethod::Fallback);
        assert_eq!(ctx.index_id, basename(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// #6550, the defect itself: with no marker anywhere up the chain the walk
    /// returns the start path, and `$HOME`'s basename is the operator's name.
    /// The live daemon held index `masa` for a real repository because of it.
    /// This assertion fails against the pre-fix code, which returned
    /// `ProjectContext { index_id: "masa", .. }` instead.
    #[test]
    fn detect_project_refuses_the_home_directory() {
        let home = tempdir_unique("detect-home");
        fs::create_dir_all(&home).unwrap();

        let err = detect_project_against(&home, Some(&home)).expect_err("must refuse");
        assert_eq!(err.refusal, IndexRootRefusal::HomeDirectory);
        assert_eq!(err.path, home);
        assert!(
            err.to_string().contains("--index"),
            "the message must say how to proceed: {err}"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A `$HOME` that is itself a git repository (dotfiles) resolves to the same
    /// root through a different arm, and the derived id is wrong in exactly the
    /// same way — so the refusal is checked against the resolved root, not the
    /// arm that produced it.
    #[test]
    fn detect_project_refuses_a_home_directory_that_is_a_git_repo() {
        let home = tempdir_unique("detect-home-git");
        fs::create_dir_all(home.join(".git")).unwrap();
        let nested = home.join("notes");
        fs::create_dir_all(&nested).unwrap();

        let err = detect_project_against(&nested, Some(&home)).expect_err("must refuse");
        assert_eq!(err.refusal, IndexRootRefusal::HomeDirectory);

        let _ = fs::remove_dir_all(&home);
    }

    /// The guard has to stay usable: an ordinary checkout INSIDE the home
    /// directory — where nearly every project lives — still detects normally.
    #[test]
    fn detect_project_indexes_a_project_under_the_home_directory() {
        let home = tempdir_unique("detect-home-with-project");
        let project = home.join("code/acme-api");
        fs::create_dir_all(project.join(".git")).unwrap();

        let ctx = detect_project_against(&project, Some(&home)).expect("indexable");
        assert_eq!(ctx.index_id, "acme-api");
        assert_eq!(ctx.root_path, project);

        let _ = fs::remove_dir_all(&home);
    }

    /// The filesystem root derives the empty id, which every downstream caller
    /// would have treated as a real index name.
    #[test]
    fn detect_project_refuses_the_filesystem_root() {
        let err = detect_project_against(Path::new("/"), None).expect_err("must refuse");
        assert_eq!(err.refusal, IndexRootRefusal::FilesystemRoot);
    }

    /// The real home directory reaches the same refusal through the real
    /// `dirs::home_dir()` lookup, which is the path production takes.
    #[test]
    fn detect_project_refuses_the_real_home_directory() {
        let Some(home) = dirs::home_dir() else {
            panic!("this test needs a resolvable home directory");
        };
        let err = detect_project(&home).expect_err("must refuse the operator's home directory");
        assert_eq!(err.refusal, IndexRootRefusal::HomeDirectory);
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

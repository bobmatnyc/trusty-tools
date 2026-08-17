//! Claude Code project directory discovery.
//!
//! Why: trusty-memory's `setup` command walks the user's home directory looking
//! for Claude Code project directories (those carrying a `.claude/` directory
//! or a `CLAUDE.md` file) so it can offer to register them. The walk, the
//! marker detection, and the default search roots are generally useful — this
//! module hoists them into trusty-common so trusty-search and trusty-analyze
//! can reuse them instead of growing their own copies.
//!
//! What: a [`ClaudeProject`] record plus a [`discover_claude_projects`] walker.
//! No global state.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only` covers
//! default-search-dir wiring; the filesystem-walking test is `#[ignore]`.

use std::path::{Path, PathBuf};

use crate::claude_config::SCAN_SKIP_DIRS;

/// Default depth [`discover_claude_projects`] recurses inside each search root.
const DEFAULT_PROJECT_MAX_DEPTH: usize = 3;

/// Relative directory names under `$HOME` searched by default for Claude Code
/// projects.
///
/// Why: developers keep code in a small, conventional set of top-level folders.
/// Sharing the list keeps every trusty-* setup command searching the same
/// places, and gives callers a sensible default they can override.
/// What: a slice of directory base-names relative to the home directory.
/// Test: `default_search_dirs_are_stable` pins the contents.
pub const DEFAULT_SEARCH_DIRS: &[&str] = &["Projects", "src", "dev", "code", "work", "workspace"];

/// A discovered Claude Code project directory.
///
/// Why: callers need to know not just that a directory looks like a project but
/// *why* — whether it has a `.claude/` directory, a `CLAUDE.md`, or a `.git/`.
/// A setup UI uses those flags to label and prioritise entries.
/// What: the absolute project `path` plus three boolean markers.
/// Test: populated and asserted by `discover_claude_projects_finds_marked_dirs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeProject {
    /// Absolute path to the project directory.
    pub path: PathBuf,
    /// Directory contains a `.claude/` subdirectory.
    pub has_claude_dir: bool,
    /// Directory contains a `CLAUDE.md` file.
    pub has_claude_md: bool,
    /// Directory contains a `.git/` subdirectory.
    pub has_git: bool,
}

/// Default [`discover_claude_projects`] recursion depth, exposed so callers can
/// use the library default without hard-coding the number.
///
/// Why: keeps the "3" in one place.
/// What: returns [`DEFAULT_PROJECT_MAX_DEPTH`].
/// Test: compile-time constant; no runtime test needed.
pub const fn default_project_max_depth() -> usize {
    DEFAULT_PROJECT_MAX_DEPTH
}

/// Discover Claude Code project directories under `home`.
///
/// Why: setup commands want to present the user with a list of their Claude
/// Code projects. Scanning a few conventional roots (rather than all of `$HOME`)
/// keeps the walk fast and the results relevant.
/// What: for each entry of `search_dirs` (joined onto `home`), recursively walks
/// up to `max_depth` directories deep, skipping any directory in
/// [`SCAN_SKIP_DIRS`]. Every directory carrying a `.claude/` directory or a
/// `CLAUDE.md` file is reported as a [`ClaudeProject`] with its marker flags
/// populated. A directory matching is not recursed into (its subdirectories are
/// considered part of the same project). Use [`DEFAULT_SEARCH_DIRS`] and
/// [`default_project_max_depth`] for the standard configuration. Results are
/// sorted by path and de-duplicated.
/// Test: `discover_claude_projects_finds_marked_dirs` (`#[ignore]`, real fs).
pub fn discover_claude_projects(
    home: &Path,
    search_dirs: &[&str],
    max_depth: usize,
) -> Vec<ClaudeProject> {
    let mut found = Vec::new();
    for rel in search_dirs {
        let root = home.join(rel);
        if root.is_dir() {
            collect_projects(&root, max_depth, &mut found);
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    found
}

/// Recursive worker for [`discover_claude_projects`].
fn collect_projects(dir: &Path, depth_remaining: usize, out: &mut Vec<ClaudeProject>) {
    if let Some(project) = inspect_project_dir(dir) {
        // A matched directory IS the project — don't descend into it.
        out.push(project);
        return;
    }

    if depth_remaining == 0 {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // permission denied / not a dir — skip silently
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if SCAN_SKIP_DIRS.contains(&name) {
            continue;
        }
        collect_projects(&path, depth_remaining.saturating_sub(1), out);
    }
}

/// Inspect a single directory; return a [`ClaudeProject`] if it carries a
/// Claude Code marker (`.claude/` or `CLAUDE.md`), else `None`.
fn inspect_project_dir(dir: &Path) -> Option<ClaudeProject> {
    let has_claude_dir = dir.join(".claude").is_dir();
    let has_claude_md = dir.join("CLAUDE.md").is_file();
    if !has_claude_dir && !has_claude_md {
        return None;
    }
    Some(ClaudeProject {
        path: dir.to_path_buf(),
        has_claude_dir,
        has_claude_md,
        has_git: dir.join(".git").is_dir(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-project-disco-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn default_search_dirs_are_stable() {
        assert_eq!(
            DEFAULT_SEARCH_DIRS,
            &["Projects", "src", "dev", "code", "work", "workspace"]
        );
    }

    #[test]
    fn default_project_max_depth_is_three() {
        assert_eq!(default_project_max_depth(), 3);
    }

    #[test]
    fn inspect_project_dir_rejects_unmarked() {
        let dir = scratch_dir("unmarked");
        assert!(inspect_project_dir(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn inspect_project_dir_detects_markers() {
        let dir = scratch_dir("markers");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "# project").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let p = inspect_project_dir(&dir).expect("marked dir should be a project");
        assert!(p.has_claude_dir);
        assert!(p.has_claude_md);
        assert!(p.has_git);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn discover_claude_projects_finds_marked_dirs() {
        let home = scratch_dir("home");

        // home/Projects/alpha has a .claude dir.
        let alpha = home.join("Projects").join("alpha");
        std::fs::create_dir_all(alpha.join(".claude")).unwrap();

        // home/src/beta has a CLAUDE.md.
        let beta = home.join("src").join("beta");
        std::fs::create_dir_all(&beta).unwrap();
        std::fs::write(beta.join("CLAUDE.md"), "# beta").unwrap();

        // home/Projects/node_modules/gamma is skipped.
        let gamma = home.join("Projects").join("node_modules").join("gamma");
        std::fs::create_dir_all(gamma.join(".claude")).unwrap();

        let found =
            discover_claude_projects(&home, DEFAULT_SEARCH_DIRS, default_project_max_depth());
        assert_eq!(found.len(), 2, "alpha + beta, gamma skipped: {found:?}");
        assert!(found.iter().any(|p| p.path == alpha && p.has_claude_dir));
        assert!(found.iter().any(|p| p.path == beta && p.has_claude_md));
        assert!(
            found
                .iter()
                .all(|p| !p.path.to_string_lossy().contains("node_modules"))
        );

        std::fs::remove_dir_all(&home).ok();
    }
}

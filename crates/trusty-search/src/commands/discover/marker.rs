//! Project-marker detection: identifies directories worth indexing.
//!
//! Why: keeps detection rules in one focused module so the priority order is
//!      consistent between the scanner and any future caller (doctor, MCP,
//!      tests). Each probe is a single `is_dir`/`is_file`/`exists` call —
//!      O(1) stat, no recursion.
//! What: exports `ProjectMarker`, `detect_project_marker`, and
//!       `default_scan_paths` for use by the discovery orchestrator.
//! Test: `detect_project_marker_*` unit tests at the bottom of this file.

use std::path::{Path, PathBuf};

/// The `.trusty-tools/` directory name used as a project-discovery signal.
///
/// Why: defines the constant in one place so the detection logic and tests
///      agree on the exact spelling. Matches the convention established by
///      `trusty-memory`'s `project_root::TRUSTY_TOOLS_DIR`.
/// What: `".trusty-tools"` — a directory at a project root indicates the
///       project participates in the trusty-tools ecosystem.
/// Test: `detect_project_marker_trusty_tools_dir` in the tests module below.
pub(super) const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

/// Signal that identifies a directory as worth indexing.
///
/// Why: priority matters — a `.claude/` directory is the strongest signal that
///      this project is being worked on with Claude Code and should be indexed
///      first. `.git/` and `.trusty-tools/` are weaker but still useful hints.
///      `.trusty-tools/` is added here (#470) so projects following the
///      trusty-tools convention are discovered at startup alongside Claude Code
///      and git projects, with a single marginal `path.exists()` stat per
///      iterated subdirectory — no additional tree walk.
/// What: ordered by strength; `Claude` > `ClaudeMd` > `Git` > `TrustyTools`.
///       `None` means skip.
/// Test: `detect_project_marker_*` unit tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectMarker {
    Claude,
    ClaudeMd,
    Git,
    /// A `.trusty-tools/` directory is present — the project follows the
    /// trusty-tools ecosystem convention (#470).
    TrustyTools,
    None,
}

/// Inspect a directory for the strongest project marker present.
///
/// Why: keeps the detection rules in one place so the priority order stays
///      consistent between the scanner and any future caller (doctor, MCP,
///      tests). Each probe is a single `is_dir`/`is_file`/`exists` call —
///      O(1) stat, no recursion — so the marginal cost per subdirectory in
///      `auto_discover_and_index` is negligible.
/// What: probes `.claude/` first, then `CLAUDE.md`, then `.git/`, then
///       `.trusty-tools/`, returning the first match.
/// Test: see `detect_project_marker_*` below.
pub(super) fn detect_project_marker(dir: &Path) -> ProjectMarker {
    if dir.join(".claude").is_dir() {
        return ProjectMarker::Claude;
    }
    if dir.join("CLAUDE.md").is_file() {
        return ProjectMarker::ClaudeMd;
    }
    if dir.join(".git").exists() {
        return ProjectMarker::Git;
    }
    if dir.join(TRUSTY_TOOLS_DIR).is_dir() {
        return ProjectMarker::TrustyTools;
    }
    ProjectMarker::None
}

/// Default scan paths when the user has not set `scan_paths` in
/// `~/.config/trusty-search/config.yaml`.
///
/// Why: a fresh install needs to do something useful. Picking the three most
///      common project-root conventions covers nearly every developer setup
///      without over-eager filesystem walks.
/// What: returns `~/Projects`, `~/code`, `~/src`, filtered to those that
///       actually exist on the current machine. Returns empty when `$HOME` is
///       not set (unusual; only happens in restricted CI sandboxes).
/// Test: covered indirectly by `auto_discover_and_index_smoke` (when wired).
pub(super) fn default_scan_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["Projects", "code", "src"]
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir_unique(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("trusty-discover-{label}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detect_project_marker_claude_dir_wins() {
        let dir = tempdir_unique("claude");
        fs::create_dir_all(dir.join(".claude")).unwrap();
        fs::write(dir.join("CLAUDE.md"), "x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::Claude);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_claude_md_beats_git() {
        let dir = tempdir_unique("claudemd");
        fs::write(dir.join("CLAUDE.md"), "x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::ClaudeMd);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_git_when_only_git() {
        let dir = tempdir_unique("git");
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::Git);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_none_when_empty() {
        let dir = tempdir_unique("empty");
        assert_eq!(detect_project_marker(&dir), ProjectMarker::None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_ignores_claude_md_as_dir() {
        // CLAUDE.md must be a file, not a directory.
        let dir = tempdir_unique("claudedir");
        fs::create_dir_all(dir.join("CLAUDE.md")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::None);
        let _ = fs::remove_dir_all(&dir);
    }

    // ── Issue #470: .trusty-tools/ marker detection ──────────────────────────

    /// Why: a directory containing only `.trusty-tools/` (and none of the
    /// pre-existing markers) must be recognised as a project by
    /// `detect_project_marker` so `auto_discover_and_index` queues it for
    /// indexing. This is the primary acceptance criterion for #470.
    ///
    /// What: creates a temp dir with only a `.trusty-tools/` subdirectory,
    /// then asserts the marker variant is `TrustyTools`. Confirms the signal
    /// fires from a single `is_dir` stat — the function is pure/bounded and
    /// contains no recursion.
    ///
    /// Test: this test itself; bounded by structural inspection (the function
    /// has exactly four probe calls — see `detect_project_marker` source).
    #[test]
    fn detect_project_marker_trusty_tools_dir() {
        let dir = tempdir_unique("trustytools");
        fs::create_dir_all(dir.join(TRUSTY_TOOLS_DIR)).unwrap();
        assert_eq!(
            detect_project_marker(&dir),
            ProjectMarker::TrustyTools,
            ".trusty-tools/ dir must yield TrustyTools marker"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Why: priority ordering must be preserved — `.claude/` must still win
    /// over `.trusty-tools/` when both are present (a Claude Code workspace
    /// may also follow the trusty-tools convention).
    ///
    /// What: creates a temp dir with both `.claude/` and `.trusty-tools/`,
    /// asserts `Claude` is returned.
    ///
    /// Test: this test itself.
    #[test]
    fn detect_project_marker_claude_wins_over_trusty_tools() {
        let dir = tempdir_unique("claude-plus-trusty");
        fs::create_dir_all(dir.join(".claude")).unwrap();
        fs::create_dir_all(dir.join(TRUSTY_TOOLS_DIR)).unwrap();
        assert_eq!(
            detect_project_marker(&dir),
            ProjectMarker::Claude,
            ".claude/ must take priority over .trusty-tools/"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Why: `.trusty-tools` as a file (not a directory) must not trigger the
    /// marker — the convention requires a directory, matching the `is_dir`
    /// probe in `detect_project_marker`.
    ///
    /// What: writes a regular file named `.trusty-tools` and asserts `None`.
    ///
    /// Test: this test itself.
    #[test]
    fn detect_project_marker_trusty_tools_file_not_dir_is_none() {
        let dir = tempdir_unique("trustytools-file");
        fs::write(dir.join(TRUSTY_TOOLS_DIR), "not a dir").unwrap();
        assert_eq!(
            detect_project_marker(&dir),
            ProjectMarker::None,
            ".trusty-tools as a file must not trigger TrustyTools marker"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Why: a directory with none of the four markers must still return `None`
    /// after the `.trusty-tools/` probe was added — regression guard ensuring
    /// we didn't accidentally widen detection.
    ///
    /// What: creates an empty temp dir and asserts `None`.
    ///
    /// Test: this test itself.
    #[test]
    fn detect_project_marker_none_when_no_markers_after_trusty_tools_added() {
        let dir = tempdir_unique("no-markers");
        // No .claude/, no CLAUDE.md, no .git, no .trusty-tools/
        assert_eq!(
            detect_project_marker(&dir),
            ProjectMarker::None,
            "directory with no markers must still return None"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_scan_paths_does_not_panic() {
        // We can't assert exact contents (depends on the user's $HOME) but the
        // call must always return cleanly.
        let _ = default_scan_paths();
    }
}

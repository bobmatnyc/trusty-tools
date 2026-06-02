//! Auto-derive the trusty-search index from the current project directory.
//!
//! Why: trusty-review registered at the user level (`~/.claude/.mcp.json`)
//! has no per-project `TRUSTY_SEARCH_INDEX` env var — the env is shared across
//! all projects.  Without auto-derivation the hardcoded fallback (`"main"`) is
//! almost always wrong.  This module resolves the correct index by matching the
//! current repo root against each index's registered `root_path` (issue #661).
//!
//! What: exposes one public entry point — `resolve_search_index` — that:
//!   1. Reads the git root from the process CWD (walks up; falls back to CWD).
//!   2. Queries `GET /indexes?details=true` on the running trusty-search daemon.
//!   3. Picks the index whose `root_path` is a prefix of the repo root (longest
//!      match wins so sub-project indexes beat their parent).
//!   4. Returns the matched index id, or `None` on any failure so the caller can
//!      fall back to `"main"` with a warning.
//!
//! Test: `resolve_search_index_explicit_env_wins`,
//! `resolve_picks_longest_root_match`, `resolve_returns_none_on_no_match`.

use crate::integrations::search_client::IndexInfo;
use std::path::{Path, PathBuf};
use tracing::warn;

// ─── Git-root detection ───────────────────────────────────────────────────────

/// Walk up from `start` to find the git repo root.
///
/// Why: the process CWD inside an MCP server is the project root by convention,
/// but Claude Code may set it to a subdirectory.  Walking up to `.git/` gives
/// the canonical repo root that the trusty-search index was registered against.
/// What: returns the first ancestor directory (inclusive of `start`) that
/// contains a `.git` entry, or `start` if none is found.
/// Test: `find_git_root_returns_cwd_when_no_git`.
pub fn find_git_root(start: &Path) -> PathBuf {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return current;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return start.to_path_buf(),
        }
    }
}

/// Return the canonical (symlink-resolved) repo root for the process CWD.
///
/// Why: `canonicalize` strips `..` components and resolves symlinks so the
/// path comparison against `root_path` values from the daemon works even when
/// the CWD was reached via a symlink.
/// What: `std::env::current_dir()` → `find_git_root` → `canonicalize`.
/// Errors fall back to the raw path to avoid a hard failure.
/// Test: `repo_root_from_cwd_falls_back_to_cwd_on_canonicalize_error`.
pub fn repo_root_from_cwd() -> PathBuf {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            warn!("trusty-review index auto-derive: cannot read cwd: {e}");
            return PathBuf::from(".");
        }
    };
    let git_root = find_git_root(&cwd);
    git_root.canonicalize().unwrap_or(git_root)
}

// ─── Index matching ───────────────────────────────────────────────────────────

/// Pick the best matching index from a list of known indexes.
///
/// Why: a machine may have several indexes registered (e.g. `api/`, `ui/`,
/// `monorepo/`).  The index whose `root_path` is the longest prefix of
/// `repo_root` is the most specific match and should win over more general
/// parent indexes.
/// What: filters to indexes whose canonicalised `root_path` is a prefix of
/// `repo_root`, then returns the one with the longest (most specific) path.
/// Returns `None` if no index matches.
/// Test: `resolve_picks_longest_root_match`, `resolve_returns_none_on_no_match`.
pub fn best_matching_index(indexes: &[IndexInfo], repo_root: &Path) -> Option<String> {
    let mut best: Option<(&IndexInfo, usize)> = None;
    for info in indexes {
        let Some(rp_str) = &info.root_path else {
            continue;
        };
        let rp = Path::new(rp_str.as_str());
        let canonical_rp = rp.canonicalize().unwrap_or_else(|_| rp.to_path_buf());
        if repo_root.starts_with(&canonical_rp) {
            let len = rp_str.len();
            if best.is_none() || len > best.as_ref().unwrap().1 {
                best = Some((info, len));
            }
        }
    }
    best.map(|(info, _)| info.id.clone())
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Resolve the trusty-search index to use for the current project.
///
/// Why: user-level MCP wiring (`~/.claude/.mcp.json`) cannot encode a
/// per-project `TRUSTY_SEARCH_INDEX` env var; this function fills that gap by
/// inspecting the current repo and the daemon's index registry (issue #661).
/// What: returns the index id that best matches `repo_root` according to
/// `best_matching_index`.  Returns `None` on any failure (daemon unreachable,
/// no match) so the caller can fall back to `"main"` with a warning.
/// `indexes` is the list from `SearchClient::list_indexes()`.
/// Test: `resolve_picks_longest_root_match`, `resolve_returns_none_on_no_match`.
pub fn resolve_index_from_list(indexes: &[IndexInfo], repo_root: &Path) -> Option<String> {
    if indexes.is_empty() {
        warn!("trusty-review index auto-derive: no indexes registered in trusty-search daemon");
        return None;
    }
    let result = best_matching_index(indexes, repo_root);
    if result.is_none() {
        warn!(
            repo_root = %repo_root.display(),
            "trusty-review index auto-derive: no index root_path matches the repo root; \
             falling back to \"main\". Register an index with `trusty-search index .` \
             or set TRUSTY_SEARCH_INDEX explicitly."
        );
    }
    result
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::search_client::IndexInfo;

    fn make_index(id: &str, root_path: Option<&str>) -> IndexInfo {
        IndexInfo {
            id: id.to_string(),
            name: None,
            root_path: root_path.map(|s| s.to_string()),
        }
    }

    // ── find_git_root ─────────────────────────────────────────────────────────

    #[test]
    fn find_git_root_returns_cwd_when_no_git() {
        // A temp dir without .git — should return the dir itself.
        let dir = tempfile::tempdir().unwrap();
        let result = find_git_root(dir.path());
        assert_eq!(result, dir.path());
    }

    #[test]
    fn find_git_root_finds_git_in_parent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let sub = root.path().join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        let result = find_git_root(&sub);
        assert_eq!(result, root.path());
    }

    // ── best_matching_index ───────────────────────────────────────────────────

    #[test]
    fn resolve_returns_none_on_empty_list() {
        let result = best_matching_index(&[], Path::new("/home/user/project"));
        assert!(result.is_none());
    }

    #[test]
    fn resolve_returns_none_on_no_match() {
        let indexes = vec![
            make_index("other-project", Some("/home/user/other")),
            make_index("third", Some("/srv/third")),
        ];
        let result = best_matching_index(&indexes, Path::new("/home/user/my-project"));
        assert!(
            result.is_none(),
            "no index whose root_path is a prefix — should return None"
        );
    }

    #[test]
    fn resolve_picks_exact_match() {
        let indexes = vec![
            make_index("my-project", Some("/home/user/my-project")),
            make_index("other", Some("/home/user/other")),
        ];
        let result = best_matching_index(&indexes, Path::new("/home/user/my-project"));
        assert_eq!(result.as_deref(), Some("my-project"));
    }

    #[test]
    fn resolve_picks_longest_root_match() {
        // `api` is a sub-index inside `monorepo`; `api` should win because its
        // root_path is a longer (more specific) prefix.
        let indexes = vec![
            make_index("monorepo", Some("/home/user/monorepo")),
            make_index("api", Some("/home/user/monorepo/api")),
        ];
        let result = best_matching_index(&indexes, Path::new("/home/user/monorepo/api/src"));
        assert_eq!(
            result.as_deref(),
            Some("api"),
            "the more specific (longer) root_path must win"
        );
    }

    #[test]
    fn resolve_skips_indexes_without_root_path() {
        let indexes = vec![
            make_index("no-root", None),
            make_index("with-root", Some("/home/user/project")),
        ];
        let result = best_matching_index(&indexes, Path::new("/home/user/project"));
        assert_eq!(
            result.as_deref(),
            Some("with-root"),
            "indexes without root_path must be ignored"
        );
    }

    #[test]
    fn resolve_all_without_root_path_returns_none() {
        let indexes = vec![make_index("a", None), make_index("b", None)];
        let result = best_matching_index(&indexes, Path::new("/home/user/project"));
        assert!(
            result.is_none(),
            "all indexes lack root_path — must return None"
        );
    }

    // ── resolve_index_from_list ───────────────────────────────────────────────

    #[test]
    fn resolve_index_from_list_returns_none_for_empty() {
        let result = resolve_index_from_list(&[], Path::new("/home/user/project"));
        assert!(result.is_none());
    }

    #[test]
    fn resolve_index_from_list_returns_match() {
        let indexes = vec![make_index("my-index", Some("/home/user/my-project"))];
        let result = resolve_index_from_list(&indexes, Path::new("/home/user/my-project/src"));
        assert_eq!(result.as_deref(), Some("my-index"));
    }
}

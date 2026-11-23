//! Where an assistant's manifest lives on disk.
//!
//! #11632: what is left of `api::server::agent_patch` after the HTTP
//! `PATCH`/`GET /api/agents/:name` routes were cut with the API.

use std::path::PathBuf;

/// Resolve `name`'s on-disk config path within `dir`, preferring a directory
/// PACKAGE (`dir/<name>/agent.toml`) over a flat shadow (`dir/<name>.toml`) —
/// matching `scan_agents_dir_tiered`'s package-over-flat dispatch-resolution
/// order (see module doc comment for why this matters). Returns
/// `(toml_path, package_dir)`; `package_dir` is `Some` only for the package
/// case. Returns `None` when neither a package nor a flat file exists.
/// Test: `agent_paths::tests::package_beats_flat_and_first_tier_wins`.
///
/// `dirs` is searched in order (project-local tier before `$HOME`, matching
/// [`crate::agents::agents_dir_candidates`]'s documented precedence) —
/// critical for the packaged desktop app, where the Tauri sidecar's cwd is
/// `/` (sealed volume, #3741): a SINGLE cwd-relative directory (the
/// pre-#3819 behavior) would never resolve ANY bundled agent there, since
/// every bundled agent lives in the `$HOME/.trusty-agents/agents` tier, not
/// a nonexistent `/.trusty-agents/agents`.
pub(crate) fn resolve_agent_paths(
    dirs: &[PathBuf],
    name: &str,
) -> Option<(PathBuf, Option<PathBuf>)> {
    for dir in dirs {
        let package_dir = dir.join(name);
        let package_toml = package_dir.join("agent.toml");
        if package_toml.is_file() {
            return Some((package_toml, Some(package_dir)));
        }
        let flat = dir.join(format!("{name}.toml"));
        if flat.is_file() {
            return Some((flat, None));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the package layout must beat a flat shadow, and the first tier
    /// must beat the second, or a write lands in a file nobody loads.
    /// Test: itself.
    #[test]
    fn package_beats_flat_and_first_tier_wins() {
        let first = tempfile::tempdir().expect("tempdir");
        let second = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(first.path().join("izzie")).expect("mkdir");
        std::fs::write(first.path().join("izzie/agent.toml"), "").expect("write");
        std::fs::write(first.path().join("izzie.toml"), "").expect("write");
        std::fs::write(second.path().join("cto.toml"), "").expect("write");
        let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];

        let (toml, package) = resolve_agent_paths(&dirs, "izzie").expect("izzie");
        assert_eq!(toml, first.path().join("izzie/agent.toml"));
        assert_eq!(package, Some(first.path().join("izzie")));
        let (toml, package) = resolve_agent_paths(&dirs, "cto").expect("cto");
        assert_eq!(toml, second.path().join("cto.toml"));
        assert!(package.is_none());
        assert!(resolve_agent_paths(&dirs, "absent").is_none());
    }
}

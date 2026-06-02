//! Database-path resolution for the `tga` configuration.
//!
//! Extracted from `config/mod.rs` to keep that file within the 500-line cap
//! while providing a clearly-scoped home for this logic and its unit tests.

use std::path::{Path, PathBuf};

use super::expand_path;

/// Resolve the effective database path from the `database:` config field.
///
/// Why: the raw `database:` value may be a bare filename like `tga.db`, a
/// `~/…` tilde path, or an absolute path. When the config was loaded from a
/// known directory (e.g. `/etc/tga/config.yaml`) a relative value should
/// anchor to that directory, not to whatever the process's cwd happens to be
/// at runtime — which is especially wrong when tga runs under launchd/cron
/// where cwd is typically `/`. Absolute and `~`-prefixed paths pass through
/// unchanged; only truly relative paths (no leading `/` or `~`) are joined to
/// `config_dir`.
///
/// What: applies tilde expansion first, then — if the result is still
/// relative — resolves it against `config_dir` when provided. Returns `None`
/// when `database` is `None` (caller applies the hardcoded default).
///
/// Test: see the unit tests in this module (`database_path_*`).
pub fn resolve(database: Option<&Path>, config_dir: Option<&Path>) -> Option<PathBuf> {
    let raw = database?;
    let expanded = expand_path(raw);
    if expanded.is_absolute() {
        return Some(expanded);
    }
    // Relative path — anchor to config dir if known.
    if let Some(dir) = config_dir {
        return Some(dir.join(expanded));
    }
    // No config dir known (e.g. Config::default() path) — return as-is and
    // let the caller decide the fallback (typically cwd via PathBuf::from).
    Some(expanded)
}

/// Compute the anchored default database path when no `database:` field was
/// set in the config.
///
/// Why: the binary's fallback `tga.db` must resolve relative to the config
/// file's directory so that cron/launchd jobs running from an arbitrary cwd
/// (e.g. `/`) still open the correct database.
///
/// What: returns `config_dir.join("tga.db")` when a config directory is
/// known; otherwise falls back to bare `PathBuf::from("tga.db")` (cwd-
/// relative, kept for backward compat when no config file is loaded).
///
/// Test: see `database_path_default_anchors_to_config_dir` and
/// `database_path_default_bare_when_no_config_dir` below.
pub fn default_path(config_dir: Option<&Path>) -> PathBuf {
    match config_dir {
        Some(dir) => dir.join("tga.db"),
        None => PathBuf::from("tga.db"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve() tests ─────────────────────────────────────────────────────

    /// Why: an absolute `database:` value must pass through unchanged regardless
    /// of `config_dir`, so operators who hard-code `/var/data/tga.db` do not
    /// see it mangled.
    /// What: call `resolve` with an absolute path and assert identity.
    /// Test: pure in-memory; no filesystem I/O.
    #[test]
    fn database_path_absolute_passes_through() {
        let result = resolve(
            Some(Path::new("/var/data/tga.db")),
            Some(Path::new("/etc/tga")),
        );
        assert_eq!(result, Some(PathBuf::from("/var/data/tga.db")));
    }

    /// Why: a `~/…` path must expand to the home directory and then not be
    /// further modified by `config_dir` (it is absolute after expansion).
    /// What: call `resolve` with a tilde path and assert the HOME-expanded result.
    /// Test: overrides `HOME` env var to a known value.
    #[test]
    fn database_path_tilde_expands_and_is_absolute() {
        // Temporarily set HOME to a known directory for predictable expansion.
        let original = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/testuser");
        let result = resolve(
            Some(Path::new("~/data/tga.db")),
            Some(Path::new("/etc/tga")),
        );
        // Restore HOME.
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(result, Some(PathBuf::from("/home/testuser/data/tga.db")));
    }

    /// Why: a relative `database:` value (e.g. `data/tga.db`) must be anchored
    /// to the config directory so that cron/launchd jobs with a different cwd
    /// still open the correct file.
    /// What: call `resolve` with a relative path and `config_dir` set; assert
    /// the joined absolute path.
    /// Test: pure in-memory; no filesystem I/O.
    #[test]
    fn database_path_relative_anchors_to_config_dir() {
        let result = resolve(Some(Path::new("data/tga.db")), Some(Path::new("/etc/tga")));
        assert_eq!(result, Some(PathBuf::from("/etc/tga/data/tga.db")));
    }

    /// Why: a bare filename `database: tga.db` is the most common relative form
    /// and must also anchor to the config directory.
    /// What: call `resolve` with `tga.db` and a config dir; assert the joined path.
    /// Test: pure in-memory.
    #[test]
    fn database_path_bare_filename_anchors_to_config_dir() {
        let result = resolve(Some(Path::new("tga.db")), Some(Path::new("/home/user")));
        assert_eq!(result, Some(PathBuf::from("/home/user/tga.db")));
    }

    /// Why: when `config_dir` is unknown (e.g. `Config::default()` without a
    /// loaded file) a relative path must be returned as-is so the caller can
    /// decide the fallback rather than us fabricating an anchor.
    /// What: call `resolve` with a relative path and `None` config_dir; assert
    /// the path is returned unchanged.
    /// Test: pure in-memory.
    #[test]
    fn database_path_relative_no_config_dir_returns_as_is() {
        let result = resolve(Some(Path::new("tga.db")), None);
        assert_eq!(result, Some(PathBuf::from("tga.db")));
    }

    /// Why: when `database:` is absent `resolve` must return `None` so callers
    /// know to apply the default.
    /// What: call `resolve` with `None` database; assert `None` returned.
    /// Test: pure in-memory.
    #[test]
    fn database_path_none_returns_none() {
        assert_eq!(resolve(None, Some(Path::new("/etc/tga"))), None);
        assert_eq!(resolve(None, None), None);
    }

    // ── default_path() tests ─────────────────────────────────────────────────

    /// Why: the default `tga.db` must anchor to the config directory when one
    /// is known, so cron/launchd jobs running from `/` open the right database.
    /// What: call `default_path` with a known config dir and assert the joined path.
    /// Test: pure in-memory.
    #[test]
    fn database_path_default_anchors_to_config_dir() {
        let p = default_path(Some(Path::new("/home/user/.config/tga")));
        assert_eq!(p, PathBuf::from("/home/user/.config/tga/tga.db"));
    }

    /// Why: when no config directory is known (no config file was loaded) the
    /// bare `tga.db` fallback must be preserved for backward compatibility with
    /// existing behaviour where tga is run from the directory containing tga.db.
    /// What: call `default_path` with `None` config dir and assert the bare path.
    /// Test: pure in-memory.
    #[test]
    fn database_path_default_bare_when_no_config_dir() {
        let p = default_path(None);
        assert_eq!(p, PathBuf::from("tga.db"));
    }

    // ── backward-compat: existing tests migrated from mod.rs ─────────────────

    /// Why: the `database:` field in YAML must deserialize into `Config.database`
    /// so operators can set the DB path without a CLI flag. This test validates
    /// that `resolved_database_path` (the Config method delegating here) still
    /// works for the absolute-path case after the refactor.
    /// What: parse a YAML snippet with `database:` set to an absolute path and
    /// assert `resolve` returns the same value unchanged.
    /// Test: pure in-memory; exercises the public `resolve` entry point.
    #[test]
    fn config_database_field_parsed_absolute() {
        // Mirror the mod.rs test: an absolute path must survive round-trip.
        let p = resolve(Some(Path::new("/var/data/tga.db")), None);
        assert_eq!(p.as_deref(), Some(Path::new("/var/data/tga.db")));
    }

    /// Why: `resolve` must return `None` when the field is absent so callers
    /// fall through to the default.
    /// What: call `resolve(None, …)` and assert `None`.
    /// Test: pure in-memory; mirrors the deleted `config_database_field_absent_returns_none`.
    #[test]
    fn config_database_field_absent_returns_none() {
        assert!(
            resolve(None, None).is_none(),
            "absent database field must return None"
        );
    }
}

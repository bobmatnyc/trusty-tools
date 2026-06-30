//! Configurable managed-root resolution for the `tm` standalone driver.
//!
//! Why: the managed root was previously hardcoded to `~/.trusty-mpm/`,
//! making it impossible to run multiple isolated environments or override
//! the root in CI/automation (closes #1566).
//! What: exposes [`ManagedPaths`] (holds `root` + `claude_config_dir`) and
//! [`resolve_managed_paths`] which applies a four-level precedence:
//! CLI flag `--root` > env var `TRUSTY_MPM_ROOT` > config-file key
//! `[standalone] root` > default `~/.trusty-mpm`.
//! The config file is discovered at a FIXED XDG-style location independent of
//! the managed root: `$XDG_CONFIG_HOME/trusty-mpm/config.toml` falling back
//! to `~/.config/trusty-mpm/config.toml`. This avoids the chicken-and-egg
//! problem of finding the config before the root is known.
//! Test: `test_resolve_*` unit tests in this module cover every precedence
//! level; see also `standalone.rs` integration path.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

// ──────────────────────────────────────────────────────────────────────────────
// Public types
// ──────────────────────────────────────────────────────────────────────────────

/// Resolved paths for the standalone managed driver.
///
/// Why: groups the two paths that every standalone command needs so they are
/// computed once at command entry and threaded as a single value — no global
/// state, no repeated resolution, no divergence between `root` and
/// `claude_config_dir`.
/// What: holds `root` (the managed root, e.g. `~/.trusty-mpm`) and
/// `claude_config_dir` (always `<root>/claude-config`).
/// Test: constructed by [`resolve_managed_paths`]; every `_cmd` function in
/// `standalone.rs` accepts a `&ManagedPaths`.
#[derive(Debug, Clone)]
pub(crate) struct ManagedPaths {
    /// Managed root directory (e.g. `~/.trusty-mpm`).
    pub(crate) root: PathBuf,
    /// CLAUDE_CONFIG_DIR for all tm-managed sessions (`<root>/claude-config`).
    pub(crate) claude_config_dir: PathBuf,
}

impl ManagedPaths {
    /// Construct from an already-resolved root.
    ///
    /// Why: lets callers (and tests) build `ManagedPaths` from any `PathBuf`
    /// without going through the full resolver.
    /// What: sets `claude_config_dir = root.join("claude-config")`.
    /// Test: `test_managed_paths_claude_config_dir_is_subdir`.
    pub(crate) fn from_root(root: PathBuf) -> Self {
        let claude_config_dir = root.join("claude-config");
        Self {
            root,
            claude_config_dir,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Config-file section
// ──────────────────────────────────────────────────────────────────────────────

/// `[standalone]` section of the XDG-located `trusty-mpm/config.toml`.
///
/// Why: the managed-root config file lives at a FIXED path independent of the
/// managed root itself to avoid a chicken-and-egg problem (the root cannot be
/// known before the config is read).
/// What: currently only `root` is supported; the struct is `#[non_exhaustive]`
/// so future keys can be added without breaking the deserializer.
/// Test: `test_resolve_config_file_wins_over_default`.
#[derive(Debug, Deserialize, Default)]
#[non_exhaustive]
pub(crate) struct StandaloneConfig {
    /// Optional managed root override (`[standalone] root = "/some/path"`).
    pub(crate) root: Option<String>,
}

/// Wrapper for the XDG config file to deserialize `[standalone]`.
///
/// Why: `toml::from_str` needs a top-level struct that maps to the file's
/// section structure; this thin wrapper lets `StandaloneConfig` live cleanly
/// under `[standalone]`.
/// What: deserializes only the `[standalone]` key; all other keys are ignored.
/// Test: `test_resolve_config_file_wins_over_default`.
#[derive(Debug, Deserialize, Default)]
struct XdgConfigFile {
    #[serde(default)]
    standalone: StandaloneConfig,
}

// ──────────────────────────────────────────────────────────────────────────────
// Resolver
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve the managed store root and auto-register `cwd` as a project alias.
///
/// Why: thin wrapper used by `run_guided_default`, `launch`, and `connect`
/// so each call site stays at one SLOC while still using the same resolved
/// root (via the four-level precedence chain) that `tm ls` reads from.
/// What: resolves the managed paths with `cli_root = None` (honours
/// `TRUSTY_MPM_ROOT` and config file), then calls
/// `try_register_project_alias` non-fatally. Silently does nothing when
/// root resolution fails.
/// Test: the underlying pieces are exercised by `try_register_and_load_round_trip_same_root`
/// in `core::project_aliases::tests` and by `test_resolve_default` here.
pub(crate) fn try_register_alias(cwd: &std::path::Path) {
    if let Ok(managed) = resolve_managed_paths(None) {
        trusty_mpm::core::project_aliases::try_register_project_alias(cwd, &managed.root);
    }
}

/// Resolve `ManagedPaths` using the four-level precedence.
///
/// Why: centralises root resolution so every standalone command uses the same
/// logic with zero divergence (closes #1566).
///
/// What: applies, highest-priority first:
///
/// 1. `cli_root` — the `--root` flag value (caller passes `None` if absent);
/// 2. `TRUSTY_MPM_ROOT` env var;
/// 3. `[standalone] root` key in `$XDG_CONFIG_HOME/trusty-mpm/config.toml`
///    (or `~/.config/trusty-mpm/config.toml` fallback);
/// 4. `~/.trusty-mpm` default.
///
/// Tilde (`~`) in any string source is expanded to the real home directory.
///
/// Test: `test_resolve_flag_wins`, `test_resolve_env_wins_over_config`,
/// `test_resolve_config_file_wins_over_default`, `test_resolve_default`.
pub(crate) fn resolve_managed_paths(cli_root: Option<&str>) -> anyhow::Result<ManagedPaths> {
    let root = resolve_root(cli_root)?;
    Ok(ManagedPaths::from_root(root))
}

/// Inner resolver that returns only the root `PathBuf`.
///
/// Why: separates the resolution logic from `ManagedPaths` construction so
/// unit tests can assert on the path alone.
/// What: walks the precedence chain and returns the first non-empty source.
/// Test: same tests as [`resolve_managed_paths`].
fn resolve_root(cli_root: Option<&str>) -> anyhow::Result<PathBuf> {
    // 1. CLI flag
    if let Some(raw) = cli_root {
        let p = expand_tilde(raw)?;
        return Ok(p);
    }

    // 2. Env var
    if let Ok(raw) = std::env::var("TRUSTY_MPM_ROOT")
        && !raw.is_empty()
    {
        let p = expand_tilde(&raw)?;
        return Ok(p);
    }

    // 3. Config file
    if let Some(cfg_path) = xdg_config_path()
        && let Some(root) = read_standalone_root_from_file(&cfg_path)
    {
        let p = expand_tilde(&root)?;
        return Ok(p);
    }

    // 4. Default
    dirs::home_dir()
        .map(|h| h.join(".trusty-mpm"))
        .context("cannot resolve home directory for default managed root")
}

/// Expand a leading `~/` to the real home directory.
///
/// Why: users naturally write `~/my-tm-root` in env vars and config files;
/// the shell does not expand those in non-interactive contexts.
/// What: replaces a leading `~/` or bare `~` with `$HOME`; leaves all other
/// paths unchanged. Returns an error when tilde expansion is needed but `HOME`
/// is unavailable.
/// Test: `test_expand_tilde_expands` and `test_expand_tilde_noop`.
pub(crate) fn expand_tilde(raw: &str) -> anyhow::Result<PathBuf> {
    if raw == "~" || raw.starts_with("~/") {
        let home = dirs::home_dir().context("cannot resolve home directory for tilde expansion")?;
        let rest = raw.trim_start_matches('~').trim_start_matches('/');
        if rest.is_empty() {
            Ok(home)
        } else {
            Ok(home.join(rest))
        }
    } else {
        Ok(PathBuf::from(raw))
    }
}

/// Return the XDG-style config path for trusty-mpm.
///
/// Why: the config file that can override the managed root must live at a
/// FIXED location that does not depend on the managed root — otherwise the
/// root cannot be known before the config file is read.
/// What: returns `$XDG_CONFIG_HOME/trusty-mpm/config.toml` when
/// `$XDG_CONFIG_HOME` is set and non-empty, else `~/.config/trusty-mpm/config.toml`.
/// Returns `None` when neither `XDG_CONFIG_HOME` nor `HOME` is available (rare
/// in practice; only affects stripped CI environments).
/// Test: `test_xdg_config_path_uses_xdg_home` and
/// `test_xdg_config_path_falls_back_to_home_config`.
pub(crate) fn xdg_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("trusty-mpm").join("config.toml"));
    }
    dirs::home_dir().map(|h| h.join(".config").join("trusty-mpm").join("config.toml"))
}

/// Read `[standalone] root` from a TOML config file, if present.
///
/// Why: the config-file tier in the precedence chain lets users set a permanent
/// managed-root override without exporting env vars in every shell.
/// What: reads `path`, parses TOML, extracts `standalone.root`; returns `None`
/// on any I/O or parse error (non-existent file, malformed TOML, absent key).
/// Errors are deliberately swallowed — a bad config file should never prevent
/// the standalone driver from starting with the default.
/// Test: `test_resolve_config_file_wins_over_default`.
fn read_standalone_root_from_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let cfg: XdgConfigFile = toml::from_str(&raw).ok()?;
    let root = cfg.standalone.root?;
    if root.is_empty() { None } else { Some(root) }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Serialize env-var mutations so parallel test threads do not race.
    // Each test that sets env vars must hold this lock for its entire duration.
    //
    // Note: `set_var`/`remove_var` are unsafe in Rust 2024 edition because they
    // are unsound in multi-threaded programs without synchronization. We hold
    // `ENV_LOCK` across every mutation so only one thread mutates the env at a
    // time, making the usage safe in practice.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── low-level unsafe helpers ─────────────────────────────────────────────

    /// Set an env var (caller must hold `ENV_LOCK`).
    ///
    /// Why: `std::env::set_var` is unsafe in Rust 2024; centralising the
    /// `unsafe` block here keeps the individual tests readable.
    /// What: calls `std::env::set_var` inside an unsafe block.
    /// Test: used internally by the RAII guard.
    fn set_env(key: &str, val: &str) {
        // SAFETY: we hold ENV_LOCK for the entire duration of every test that
        // calls this, ensuring no concurrent env mutations from other threads.
        unsafe { std::env::set_var(key, val) }
    }

    /// Remove an env var (caller must hold `ENV_LOCK`).
    ///
    /// Why: mirrors `set_env` for removals.
    /// What: calls `std::env::remove_var` inside an unsafe block.
    /// Test: used internally by the RAII guard.
    fn remove_env(key: &str) {
        // SAFETY: same as set_env — ENV_LOCK guarantees no concurrent mutation.
        unsafe { std::env::remove_var(key) }
    }

    // ── RAII env guard ───────────────────────────────────────────────────────

    /// RAII guard that holds `ENV_LOCK` and restores `TRUSTY_MPM_ROOT` +
    /// `XDG_CONFIG_HOME` on drop — even if the test panics.
    ///
    /// Why: the previous `with_clean_env` closure-based helper did not restore
    /// env vars when the closure panicked, poisoning `ENV_LOCK` and leaving env
    /// dirty for subsequent tests. A `Drop` impl restores state unconditionally.
    /// What: acquires `ENV_LOCK`, saves the current values of both vars, clears
    /// them; on `Drop` restores both vars to their saved state.
    /// Test: every `test_resolve_*` test relies on this guard for isolation.
    struct CleanEnvGuard {
        saved_root: Option<String>,
        saved_xdg: Option<String>,
        // Hold the lock for the guard's lifetime.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CleanEnvGuard {
        /// Acquire the lock, save both vars, and clear them.
        fn new() -> Self {
            let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved_root = std::env::var("TRUSTY_MPM_ROOT").ok();
            let saved_xdg = std::env::var("XDG_CONFIG_HOME").ok();
            remove_env("TRUSTY_MPM_ROOT");
            remove_env("XDG_CONFIG_HOME");
            Self {
                saved_root,
                saved_xdg,
                _lock,
            }
        }
    }

    impl Drop for CleanEnvGuard {
        fn drop(&mut self) {
            // Restore both vars unconditionally, even on panic.
            match &self.saved_root {
                Some(v) => set_env("TRUSTY_MPM_ROOT", v),
                None => remove_env("TRUSTY_MPM_ROOT"),
            }
            match &self.saved_xdg {
                Some(v) => set_env("XDG_CONFIG_HOME", v),
                None => remove_env("XDG_CONFIG_HOME"),
            }
        }
    }

    // ── ManagedPaths ────────────────────────────────────────────────────────

    #[test]
    fn test_managed_paths_claude_config_dir_is_subdir() {
        let root = PathBuf::from("/tmp/my-root");
        let mp = ManagedPaths::from_root(root.clone());
        assert_eq!(mp.root, root);
        assert_eq!(mp.claude_config_dir, root.join("claude-config"));
    }

    // ── expand_tilde ────────────────────────────────────────────────────────

    #[test]
    fn test_expand_tilde_expands() {
        let home = dirs::home_dir().expect("home dir required for this test");
        let result = expand_tilde("~/foo/bar").unwrap();
        assert_eq!(result, home.join("foo/bar"));
    }

    #[test]
    fn test_expand_tilde_bare_tilde() {
        let home = dirs::home_dir().expect("home dir required for this test");
        let result = expand_tilde("~").unwrap();
        assert_eq!(result, home);
    }

    #[test]
    fn test_expand_tilde_noop_absolute() {
        let result = expand_tilde("/absolute/path").unwrap();
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_tilde_noop_relative() {
        let result = expand_tilde("relative/path").unwrap();
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    // ── xdg_config_path ─────────────────────────────────────────────────────

    #[test]
    fn test_xdg_config_path_uses_xdg_home() {
        let _g = CleanEnvGuard::new();
        set_env("XDG_CONFIG_HOME", "/custom/config");
        let path = xdg_config_path();
        assert_eq!(
            path,
            Some(PathBuf::from("/custom/config/trusty-mpm/config.toml"))
        );
    }

    #[test]
    fn test_xdg_config_path_falls_back_to_home_config() {
        let _g = CleanEnvGuard::new();
        // XDG_CONFIG_HOME was cleared by the guard; test the fallback path.
        let path = xdg_config_path();
        if let Some(home) = dirs::home_dir() {
            assert_eq!(
                path,
                Some(home.join(".config").join("trusty-mpm").join("config.toml"))
            );
        }
    }

    // ── resolve_root precedence ──────────────────────────────────────────────

    #[test]
    fn test_resolve_default() {
        // Nothing set → default ~/.trusty-mpm
        let _g = CleanEnvGuard::new();
        let root = resolve_root(None).expect("resolve_root should succeed");
        let home = dirs::home_dir().expect("home dir required");
        assert_eq!(root, home.join(".trusty-mpm"));
    }

    #[test]
    fn test_resolve_env_wins_over_default() {
        // TRUSTY_MPM_ROOT set → env value wins over default
        let _g = CleanEnvGuard::new();
        set_env("TRUSTY_MPM_ROOT", "/tmp/from-env");
        let root = resolve_root(None).expect("resolve_root should succeed");
        assert_eq!(root, PathBuf::from("/tmp/from-env"));
    }

    #[test]
    fn test_resolve_flag_wins_over_env() {
        // CLI flag set + env set → flag wins
        let _g = CleanEnvGuard::new();
        set_env("TRUSTY_MPM_ROOT", "/tmp/from-env");
        let root = resolve_root(Some("/tmp/from-flag")).expect("resolve_root should succeed");
        assert_eq!(root, PathBuf::from("/tmp/from-flag"));
    }

    #[test]
    fn test_resolve_config_file_wins_over_default() {
        // Config file present → config value wins over default (when no flag/env set)
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(&cfg_path, "[standalone]\nroot = \"/tmp/from-config\"\n").unwrap();

        let _g = CleanEnvGuard::new();
        // Point XDG_CONFIG_HOME at our temp dir so resolve_root finds the file.
        // TRUSTY_MPM_ROOT is absent (cleared by guard) → config file is tier-3.
        set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        let root = resolve_root(None).expect("resolve_root should succeed");
        assert_eq!(root, PathBuf::from("/tmp/from-config"));
    }

    #[test]
    fn test_resolve_flag_wins_over_config_file() {
        // CLI flag wins even when config file is present
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(&cfg_path, "[standalone]\nroot = \"/tmp/from-config\"\n").unwrap();

        let _g = CleanEnvGuard::new();
        set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        let root = resolve_root(Some("/tmp/from-flag")).expect("resolve_root should succeed");
        assert_eq!(root, PathBuf::from("/tmp/from-flag"));
    }

    #[test]
    fn test_resolve_env_wins_over_config_file() {
        // Env var wins over config file
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(&cfg_path, "[standalone]\nroot = \"/tmp/from-config\"\n").unwrap();

        let _g = CleanEnvGuard::new();
        set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        set_env("TRUSTY_MPM_ROOT", "/tmp/from-env");
        let root = resolve_root(None).expect("resolve_root should succeed");
        assert_eq!(root, PathBuf::from("/tmp/from-env"));
    }

    /// Prove that the env var does NOT skip config.toml: when env is set,
    /// env wins; when env is absent, config.toml is consulted.
    ///
    /// This test guards against the regression described in review finding #1:
    /// if `env = "TRUSTY_MPM_ROOT"` is ever added back to the clap `#[arg]`,
    /// clap populates `root` from the env var before `main` calls
    /// `resolve_managed_paths(root.as_deref())`, which would make the env var
    /// look like a CLI flag (tier-1) and cause `resolve_managed_paths` to return
    /// immediately, silently skipping config.toml. With the env var read only
    /// inside `resolve_root` (tier-2), both branches below must pass.
    #[test]
    fn test_env_and_config_file_independent_tiers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(&cfg_path, "[standalone]\nroot = \"/tmp/from-config\"\n").unwrap();

        // Branch A: env set + config file present + no --root → env wins (tier 2)
        {
            let _g = CleanEnvGuard::new();
            set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
            set_env("TRUSTY_MPM_ROOT", "/tmp/from-env");
            let root = resolve_root(None).expect("resolve_root");
            assert_eq!(
                root,
                PathBuf::from("/tmp/from-env"),
                "env must win over config.toml when TRUSTY_MPM_ROOT is set"
            );
        } // guard drops here, restoring env

        // Branch B: env absent + config file present + no --root → config wins (tier 3)
        {
            let _g = CleanEnvGuard::new();
            set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
            // TRUSTY_MPM_ROOT is NOT set (guard cleared it)
            let root = resolve_root(None).expect("resolve_root");
            assert_eq!(
                root,
                PathBuf::from("/tmp/from-config"),
                "config.toml must be consulted when TRUSTY_MPM_ROOT is absent"
            );
        }
    }

    #[test]
    fn test_resolve_config_file_missing_is_ok() {
        // Absent config file falls through to default without error
        let tmp = tempfile::tempdir().expect("tempdir");
        let _g = CleanEnvGuard::new();
        // Point XDG_CONFIG_HOME at an empty temp dir (no config.toml exists)
        set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        let root = resolve_root(None).expect("resolve_root should succeed");
        let home = dirs::home_dir().expect("home dir required");
        assert_eq!(root, home.join(".trusty-mpm"));
    }

    #[test]
    fn test_resolve_config_file_malformed_is_ignored() {
        // Malformed TOML in config file falls through to default without error
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(&cfg_path, "this is not valid toml :::").unwrap();

        let _g = CleanEnvGuard::new();
        set_env("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        let root = resolve_root(None).expect("resolve_root should succeed");
        let home = dirs::home_dir().expect("home dir required");
        assert_eq!(root, home.join(".trusty-mpm"));
    }

    #[test]
    fn test_resolve_tilde_in_env() {
        // Tilde in TRUSTY_MPM_ROOT env var is expanded
        let _g = CleanEnvGuard::new();
        set_env("TRUSTY_MPM_ROOT", "~/my-tm-root");
        let root = resolve_root(None).expect("resolve_root should succeed");
        let home = dirs::home_dir().expect("home dir required");
        assert_eq!(root, home.join("my-tm-root"));
    }

    #[test]
    fn test_resolve_managed_paths_sets_claude_config_dir() {
        // resolve_managed_paths builds both root and claude_config_dir
        let _g = CleanEnvGuard::new();
        set_env("TRUSTY_MPM_ROOT", "/tmp/test-root");
        let mp = resolve_managed_paths(None).expect("should resolve");
        assert_eq!(mp.root, PathBuf::from("/tmp/test-root"));
        assert_eq!(
            mp.claude_config_dir,
            PathBuf::from("/tmp/test-root/claude-config")
        );
    }
}

//! tm-managed `CLAUDE_CODE_OAUTH_TOKEN` store (issue #2246).
//!
//! Why: managed sessions run `claude` under a relocated `CLAUDE_CONFIG_DIR`
//! (`~/.trusty-tools/trusty-mpm/claude-config/`, see
//! [`crate::core::trusty_tools_config::managed_claude_config_dir`]). On macOS,
//! Claude Code's primary OAuth login lives in the Keychain under a service
//! name suffixed with a hash of `CLAUDE_CONFIG_DIR` — so a `/login` run inside
//! a managed session and the login stored under the operator's default
//! (bare) `CLAUDE_CONFIG_DIR` resolve to two DIFFERENT Keychain entries. The
//! session reports "Login successful" and then immediately "Not logged in" in
//! a loop, because the credential it just wrote is never the one it reads back
//! from under the relocated dir. `CLAUDE_CODE_OAUTH_TOKEN` bypasses the
//! Keychain entirely — Claude Code reads it directly from the process
//! environment — so injecting it into every managed spawn/resume sidesteps
//! the divergence completely, independent of `CLAUDE_CONFIG_DIR`.
//! What: a durable token store at
//! `~/.trusty-tools/trusty-mpm/claude-code-oauth.token` (mode 0600, one line,
//! written via [`set_token_at`]/[`set_token`]) plus [`resolve_oauth_token`],
//! the single production entry point the daemon's spawn/resume path calls —
//! it returns an already-set `CLAUDE_CODE_OAUTH_TOKEN` process-env value first
//! (highest precedence), else the stored file, else `None` (inject nothing —
//! strictly no regression for operators relying on ambient/Keychain auth).
//! [`compute_status`] backs `tm auth status`; it reports presence/absence of
//! each signal only — it NEVER returns the token value itself.
//! Test: `set_then_load_token_round_trips`, `token_file_is_mode_0600`,
//! `clear_token_removes_file`, `clear_token_is_noop_when_absent`,
//! `resolve_oauth_token_prefers_env_over_stored_file`,
//! `resolve_oauth_token_none_when_neither_present`,
//! `compute_status_never_carries_the_token_value`.

use std::path::{Path, PathBuf};

use crate::core::trusty_tools_config::CRATE_NAME;

/// Filename of the stored token under the crate's `~/.trusty-tools/<crate>` dir.
///
/// Why: a single named constant keeps the store and the status/doctor probes
/// from ever disagreeing on the filename.
/// What: `"claude-code-oauth.token"`.
/// Test: `token_file_path_layout`.
pub const TOKEN_FILE: &str = "claude-code-oauth.token";

/// The environment variable Claude Code reads to authenticate, bypassing the
/// Keychain/`.credentials.json` entirely.
///
/// Why: this is the mechanism the whole module exists to inject — naming it
/// once avoids the literal drifting between the store, the spawn-command
/// builder, and `tm auth status`.
/// What: `"CLAUDE_CODE_OAUTH_TOKEN"`.
/// Test: `resolve_oauth_token_prefers_env_over_stored_file`.
pub const OAUTH_TOKEN_ENV_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Resolve the token file path under an explicit base (hermetic).
///
/// Why: unit tests must never write into a developer's real
/// `~/.trusty-tools/trusty-mpm/`; pointing `base` at a `tempfile::TempDir`
/// keeps every round-trip test hermetic and parallel-safe.
/// What: `<base>/.trusty-tools/trusty-mpm/claude-code-oauth.token`.
/// Test: `token_file_path_layout`.
pub fn token_file_path_at(base: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(base, CRATE_NAME).join(TOKEN_FILE)
}

/// Resolve the canonical token file path.
///
/// Why: the production accessor every caller (CLI + spawn path) uses so the
/// location can never drift between them.
/// What: `~/.trusty-tools/trusty-mpm/claude-code-oauth.token`. `None` only
/// when the home directory cannot be resolved (a stripped CI environment).
/// Test: `token_file_path_layout`.
pub fn token_file_path() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_dir(CRATE_NAME).map(|dir| dir.join(TOKEN_FILE))
}

/// Write `data` to `path` with Unix mode 0600, race-free.
///
/// Why: `OpenOptions::mode()` creates the file with the restrictive
/// permission bits atomically — a write-then-`chmod` sequence has a TOCTOU
/// window where the token is briefly world-readable. Mirrors
/// `session_manager::snapshot::secure_write`'s established pattern in this
/// crate.
/// What: opens with `O_CREAT | O_TRUNC | O_WRONLY | mode=0o600` and writes
/// `data` in one call. Falls back to a plain write on non-Unix (no mode bits
/// available there).
/// Test: `token_file_is_mode_0600` (Unix only).
#[cfg(unix)]
fn secure_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn secure_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// Store `token` at an explicit `path` (hermetic core of [`set_token`]).
///
/// Why: separating the I/O root from validation keeps `tm auth set-token`
/// testable against a temp dir instead of the real home directory.
/// What: trims `token`, rejects an empty value, creates the parent directory,
/// and writes it with mode 0600 via [`secure_write`].
/// Test: `set_then_load_token_round_trips`, `set_token_rejects_empty`.
pub fn set_token_at(path: &Path, token: &str) -> anyhow::Result<()> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        anyhow::bail!("token must not be empty");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    secure_write(path, trimmed.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write token file {}: {e}", path.display()))
}

/// Store `token` at the canonical path.
///
/// Why: the production entry point `tm auth set-token` calls.
/// What: resolves [`token_file_path`] and delegates to [`set_token_at`].
/// Test: covered via `set_token_at`'s hermetic tests; production path
/// exercised by the `tm auth` CLI handler.
pub fn set_token(token: &str) -> anyhow::Result<PathBuf> {
    let path =
        token_file_path().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    set_token_at(&path, token)?;
    Ok(path)
}

/// Remove the stored token at an explicit `path` (hermetic core of
/// [`clear_token`]).
///
/// Why: `tm auth clear-token` must be idempotent — running it twice, or on a
/// machine that never had a token stored, must not error.
/// What: removes the file; a missing file is `Ok(())`, not an error.
/// Test: `clear_token_removes_file`, `clear_token_is_noop_when_absent`.
pub fn clear_token_at(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "failed to remove token file {}: {e}",
            path.display()
        )),
    }
}

/// Remove the stored token at the canonical path.
///
/// Why: the production entry point `tm auth clear-token` calls.
/// What: resolves [`token_file_path`] and delegates to [`clear_token_at`].
/// Test: covered via `clear_token_at`'s hermetic tests.
pub fn clear_token() -> anyhow::Result<PathBuf> {
    let path =
        token_file_path().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    clear_token_at(&path)?;
    Ok(path)
}

/// Load the stored token from an explicit `path` (hermetic core of
/// [`load_token`]).
///
/// Why: [`resolve_oauth_token`] and `tm auth status` both need a read that
/// never panics and treats a missing/empty file as "no token", not an error.
/// What: `None` when the file is absent, unreadable, or contains only
/// whitespace; otherwise `Some(trimmed contents)`.
/// Test: `set_then_load_token_round_trips`, `load_token_none_when_absent`,
/// `load_token_none_when_blank`.
pub fn load_token_at(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Load the stored token from the canonical path.
///
/// Why: production entry point mirroring [`token_file_path`]/[`set_token`].
/// What: resolves [`token_file_path`] and delegates to [`load_token_at`];
/// `None` when home is unresolved or no token is stored.
/// Test: covered via `load_token_at`'s hermetic tests.
pub fn load_token() -> Option<String> {
    token_file_path().and_then(|p| load_token_at(&p))
}

/// Resolve the effective OAuth token to inject into a managed spawn/resume,
/// following the documented precedence.
///
/// Why: the daemon's spawn/resume path must never override an operator who
/// already exports `CLAUDE_CODE_OAUTH_TOKEN` themselves (e.g. a CI runner, or
/// an operator testing a different token) — that value must always win over
/// the tm-managed store. When neither is present this returns `None`, and the
/// caller injects nothing: a strict, zero-regression superset of today's
/// behaviour (ambient Keychain/`ANTHROPIC_API_KEY` auth continues to work
/// exactly as before).
/// What: `Some(process-env CLAUDE_CODE_OAUTH_TOKEN)` when it is set and
/// non-blank (highest precedence); else `Some(stored file token)` via
/// [`load_token`]; else `None`.
/// Test: `resolve_oauth_token_prefers_env_over_stored_file`,
/// `resolve_oauth_token_falls_back_to_stored_file`,
/// `resolve_oauth_token_none_when_neither_present`.
pub fn resolve_oauth_token() -> Option<String> {
    if let Ok(v) = std::env::var(OAUTH_TOKEN_ENV_VAR) {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    load_token()
}

/// Non-secret snapshot of every auth signal `tm auth status` reports.
///
/// Why: keeping this struct free of the token's actual value makes it
/// structurally impossible for the status command to leak it — the
/// presentation layer only ever has booleans and paths to print.
/// What: presence of the stored token file, presence of the
/// `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` env vars, and a best-effort
/// check for an on-disk Claude Code credentials file (the Keychain itself is
/// not introspectable from here without shelling out to `security`, so this
/// is advisory — it is meaningful on Linux, where credentials live in a
/// plain file, and absent-but-fine on macOS where the Keychain is used).
/// Test: `compute_status_never_carries_the_token_value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenStatus {
    /// Whether a token is stored on disk.
    pub stored_token_present: bool,
    /// Path the stored-token check probed (whether or not it was present).
    pub token_path: Option<PathBuf>,
    /// Whether `CLAUDE_CODE_OAUTH_TOKEN` is set in the current process env.
    pub env_var_set: bool,
    /// Whether `ANTHROPIC_API_KEY` is set in the current process env.
    pub anthropic_api_key_set: bool,
    /// Whether a Claude Code credentials file was found on disk.
    pub credentials_file_present: bool,
    /// Path the credentials-file check probed.
    pub credentials_path: Option<PathBuf>,
}

/// Build a [`TokenStatus`] against explicit inputs (hermetic core of
/// [`compute_status`]).
///
/// Why: separates the pure assembly from the I/O (file/env reads) so the
/// struct's invariant — it never carries the token itself — is checkable
/// without any real home directory or process env.
/// What: a thin, allocation-free constructor.
/// Test: `compute_status_never_carries_the_token_value`.
fn build_status(
    stored_token_present: bool,
    token_path: Option<PathBuf>,
    env_var_set: bool,
    anthropic_api_key_set: bool,
    credentials_file_present: bool,
    credentials_path: Option<PathBuf>,
) -> TokenStatus {
    TokenStatus {
        stored_token_present,
        token_path,
        env_var_set,
        anthropic_api_key_set,
        credentials_file_present,
        credentials_path,
    }
}

/// Compute the current [`TokenStatus`] against the real environment/home.
///
/// Why: the production entry point `tm auth status` calls; also backs the
/// `tm doctor` login-loop check (issue #2246).
/// What: checks [`token_file_path`] for a present, non-blank token; the
/// `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` process env vars; and
/// `~/.claude/.credentials.json` on disk (a best-effort signal — see
/// [`TokenStatus`]'s doc comment).
/// Test: exercised indirectly by the `tm auth status` CLI handler; the pure
/// assembly is covered by `compute_status_never_carries_the_token_value`
/// against [`build_status`] directly.
pub fn compute_status() -> TokenStatus {
    let token_path = token_file_path();
    let stored_token_present = token_path.as_deref().and_then(load_token_at).is_some();
    let env_var_set = std::env::var(OAUTH_TOKEN_ENV_VAR).is_ok_and(|v| !v.trim().is_empty());
    let anthropic_api_key_set =
        std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.trim().is_empty());
    let credentials_path = dirs::home_dir().map(|h| h.join(".claude").join(".credentials.json"));
    let credentials_file_present = credentials_path.as_deref().is_some_and(Path::is_file);

    build_status(
        stored_token_present,
        token_path,
        env_var_set,
        anthropic_api_key_set,
        credentials_file_present,
        credentials_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII guard that redirects `$HOME` to a temp dir and restores it on
    /// drop (including panic). Mirrors the identical pattern already used in
    /// `runtime::claude_code`'s tests and `core::standalone`'s — each test
    /// module keeps its own copy rather than sharing one across crates.
    struct HomeGuard {
        prev: Option<String>,
        _tmp: tempfile::TempDir,
    }
    impl HomeGuard {
        fn set() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let prev = std::env::var("HOME").ok();
            // SAFETY: callers are #[serial], so no other test thread reads
            // HOME concurrently; Drop restores the prior value even on panic.
            unsafe { std::env::set_var("HOME", tmp.path()) };
            Self { prev, _tmp: tmp }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    #[test]
    fn token_file_path_layout() {
        let p = token_file_path_at(Path::new("/home/bob"));
        assert_eq!(
            p,
            PathBuf::from("/home/bob/.trusty-tools/trusty-mpm/claude-code-oauth.token")
        );
    }

    #[test]
    fn set_then_load_token_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        set_token_at(&path, "  sk-ant-oat01-fake-token  ").unwrap();
        let loaded = load_token_at(&path).expect("token present");
        assert_eq!(loaded, "sk-ant-oat01-fake-token", "must be trimmed");
    }

    #[test]
    fn set_token_rejects_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        assert!(set_token_at(&path, "   ").is_err());
        assert!(!path.exists(), "no file must be written on rejection");
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        set_token_at(&path, "sk-ant-oat01-fake-token").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600, got {mode:o}");
    }

    #[test]
    fn clear_token_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        set_token_at(&path, "sk-ant-oat01-fake-token").unwrap();
        assert!(path.exists());
        clear_token_at(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn clear_token_is_noop_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        assert!(!path.exists());
        assert!(clear_token_at(&path).is_ok(), "must not error when absent");
    }

    #[test]
    fn load_token_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        assert_eq!(load_token_at(&path), None);
    }

    #[test]
    fn load_token_none_when_blank() {
        let tmp = tempfile::tempdir().unwrap();
        let path = token_file_path_at(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "   \n").unwrap();
        assert_eq!(load_token_at(&path), None);
    }

    #[test]
    fn compute_status_never_carries_the_token_value() {
        // Build a status as if a token *were* present and format every field
        // to a string; the known fake token value must never appear, because
        // TokenStatus structurally has no field capable of carrying it.
        let status = build_status(
            true,
            Some(PathBuf::from(
                "/home/bob/.trusty-tools/trusty-mpm/claude-code-oauth.token",
            )),
            true,
            false,
            true,
            Some(PathBuf::from("/home/bob/.claude/.credentials.json")),
        );
        let rendered = format!("{status:?}");
        assert!(
            !rendered.contains("sk-ant-oat01-fake-token"),
            "status debug output must never contain a token value: {rendered}"
        );
    }

    #[serial_test::serial]
    #[test]
    fn resolve_oauth_token_prefers_env_over_stored_file() {
        let _home = HomeGuard::set();
        unsafe { std::env::remove_var(OAUTH_TOKEN_ENV_VAR) };
        set_token("stored-token").expect("store under redirected HOME");
        assert_eq!(
            resolve_oauth_token().as_deref(),
            Some("stored-token"),
            "with no env var set, the stored file must be used"
        );
        unsafe { std::env::set_var(OAUTH_TOKEN_ENV_VAR, "env-token") };
        let resolved = resolve_oauth_token();
        unsafe { std::env::remove_var(OAUTH_TOKEN_ENV_VAR) };
        assert_eq!(
            resolved.as_deref(),
            Some("env-token"),
            "an already-set process env var must win over the stored file"
        );
    }

    #[serial_test::serial]
    #[test]
    fn resolve_oauth_token_falls_back_to_stored_file() {
        let _home = HomeGuard::set();
        unsafe { std::env::remove_var(OAUTH_TOKEN_ENV_VAR) };
        assert_eq!(
            resolve_oauth_token(),
            None,
            "no env var, no stored file → None"
        );
        set_token("stored-only-token").expect("store under redirected HOME");
        assert_eq!(resolve_oauth_token().as_deref(), Some("stored-only-token"));
    }

    #[serial_test::serial]
    #[test]
    fn resolve_oauth_token_none_when_neither_present() {
        // A fresh temp HOME with no stored file and no env var must resolve
        // to None — the strict no-regression fallback (no injection at all).
        let _home = HomeGuard::set();
        unsafe { std::env::remove_var(OAUTH_TOKEN_ENV_VAR) };
        assert_eq!(resolve_oauth_token(), None);
    }
}

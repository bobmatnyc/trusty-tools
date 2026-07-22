//! Shared test-only helpers for env-mutating OAuth credential tests.
//!
//! Why: Both `manager` and `oauth::client_store` tests need to point
//! `dirs::home_dir()` at an isolated temp directory (to exercise the
//! per-profile-client / global-client-fallback file resolution, issue #3518)
//! without ever touching the real `~/.gworkspace-mcp`. Duplicating this per
//! module risks drift, and every caller must run `#[serial]` since `HOME` is
//! process-global state.
//! What: [`EnvGuard`] snapshots/restores a fixed set of env vars (RAII, even
//! on panic); [`fresh_temp_home`] creates a scratch dir with a
//! `.gworkspace-mcp/` subdir already present.
//! Test: exercised transitively by every caller (`manager::tests`,
//! `oauth::client_store::tests`).

use std::path::PathBuf;

/// RAII guard that snapshots and restores a fixed set of env vars.
///
/// Why: Tests that override `HOME` (and OAuth client env vars) to isolate
/// credential-resolution behavior must never leak that override into later,
/// unrelated tests — even if the test body panics.
/// What: Captures each var's current value on construction; `Drop` restores
/// it (`set_var` if it was present, `remove_var` if absent).
pub(crate) struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    pub(crate) fn capture(vars: &[&'static str]) -> Self {
        let saved = vars.iter().map(|&v| (v, std::env::var(v).ok())).collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            // SAFETY: every caller of `EnvGuard` runs under `#[serial]`, so no
            // other thread reads/writes these vars concurrently.
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

/// Build a fresh temp dir with a `.gworkspace-mcp/` subdir, never touching
/// the real `~/.gworkspace-mcp`.
pub(crate) fn fresh_temp_home(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gw-test-home-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".gworkspace-mcp")).expect("mkdir temp home");
    dir
}

//! Supervisor façade for the `trusty-embedderd` subprocess.
//!
//! Why: trusty-embedderd is a core subprocess that owns ONNX model loading
//! and serves embedding RPC. We supervise it from trusty-search so the user
//! experiences a single daemon (`trusty-search start`) without manual
//! lifecycle management. This aligns with industry-standard ML serving
//! topology (Triton, vLLM, TEI, ollama) and reduces trusty-search daemon
//! RSS substantially by moving the ONNX arena out of the search process.
//!
//! What: re-exports the supervisor types from `trusty_common::embedder_client`
//! so callers inside trusty-search can import from a single stable path. Also
//! provides `SupervisorConfig::from_env()` with trusty-search–specific
//! defaults, the `default_socket_path()` helper for per-instance UDS sockets,
//! and the `locate_embedderd_binary()` wrapper that adds the actionable error
//! message format preferred by trusty-search's startup logs.
//!
//! Test: unit tests in the `tests` submodule cover config parsing, socket
//! path construction, and binary discovery. Integration tests in
//! `tests/embedder_supervisor_e2e.rs` cover the full process lifecycle
//! (marked `#[ignore]` since they spawn a real ONNX binary).

use std::path::PathBuf;

// Re-export the core supervisor type from trusty-common.
pub use trusty_common::embedder_client::EmbedderSupervisor;

// ── Configuration ────────────────────────────────────────────────────────────

/// Supervisor tuning knobs, all settable via environment variables.
///
/// Why: hard-coded constants make the supervisor untunable in production.
/// Env vars let operators increase `startup_timeout_secs` on slow machines or
/// `max_restarts` on flaky networks without recompiling.
/// What: wraps the field names used by `trusty_common::embedder_client::SupervisorConfig`
/// and provides a `from_env()` constructor that reads the `TRUSTY_EMBEDDERD_*`
/// environment variables with trusty-search's preferred defaults.
/// The `into_common()` method converts to the type expected by
/// `EmbedderSupervisor::spawn_stdio`.
/// Test: `config_from_env_defaults` and `config_from_env_overrides` in the
/// `tests` module below.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How long to wait for the startup readiness probe (seconds).
    /// Env: `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 30).
    pub startup_timeout_secs: u64,

    /// Maximum exponential back-off ceiling between crash restarts (seconds).
    /// Env: `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` (default 60).
    pub backoff_max_secs: u64,

    /// Maximum number of crashes before the supervisor gives up.
    /// Env: `TRUSTY_EMBEDDERD_MAX_RESTARTS` (default 5).
    pub max_restarts: u32,
}

impl SupervisorConfig {
    /// Read configuration from environment variables, falling back to defaults.
    ///
    /// Why: makes the supervisor tunable in CI / production without source changes.
    /// What: reads the three `TRUSTY_EMBEDDERD_*` vars; ignores malformed
    /// values and falls through to defaults.
    /// Test: `config_from_env_defaults` and `config_from_env_overrides`.
    pub fn from_env() -> Self {
        Self {
            startup_timeout_secs: parse_env_u64("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", 30),
            backoff_max_secs: parse_env_u64("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", 60),
            max_restarts: parse_env_u32("TRUSTY_EMBEDDERD_MAX_RESTARTS", 5),
        }
    }

    /// Convert to the `trusty_common` supervisor config type.
    ///
    /// Why: `EmbedderSupervisor::spawn_stdio` expects
    /// `trusty_common::embedder_client::SupervisorConfig`; this conversion
    /// avoids duplicating field names at the call site.
    /// What: maps fields 1:1.
    /// Test: `into_common_maps_fields`.
    pub fn into_common(self) -> trusty_common::embedder_client::SupervisorConfig {
        trusty_common::embedder_client::SupervisorConfig {
            startup_timeout_secs: self.startup_timeout_secs,
            backoff_max_secs: self.backoff_max_secs,
            max_restarts: self.max_restarts,
        }
    }
}

impl Default for SupervisorConfig {
    /// Default configuration — matches `from_env()` when no env vars are set.
    ///
    /// Why: unit tests need a cheap config without touching env vars.
    /// What: `startup_timeout_secs=30`, `backoff_max_secs=60`, `max_restarts=5`.
    /// Test: used directly in unit tests.
    fn default() -> Self {
        Self {
            startup_timeout_secs: 30,
            backoff_max_secs: 60,
            max_restarts: 5,
        }
    }
}

// ── Binary discovery ─────────────────────────────────────────────────────────

/// Locate the `trusty-embedderd` binary.
///
/// Why: operators may install the binary in a non-standard location or point
/// to a development build; both cases are handled without modifying source.
/// What: delegates to `trusty_common::embedder_client::locate_embedderd_binary`.
/// Search order:
///
///   1. `TRUSTY_EMBEDDERD_BIN` env var — must exist if set.
///   2. Sibling of `current_exe()` — works for both `cargo run` and installs.
///   3. `trusty-embedderd` on `PATH`.
///   4. Otherwise returns `Err` with an actionable install hint.
///
/// Test: `locate_binary_bad_explicit_path_errors` and `locate_binary_via_explicit_env`.
pub fn locate_embedderd_binary() -> anyhow::Result<PathBuf> {
    trusty_common::embedder_client::locate_embedderd_binary()
}

// ── Socket path resolution ───────────────────────────────────────────────────

/// Compute a per-instance UDS socket path that avoids collisions between
/// concurrent trusty-search daemons on the same machine.
///
/// Why: if two daemons share a single socket path, the second spawn would
/// fail with "address already in use". Using the parent PID disambiguates.
/// What:
///   - macOS/Linux: `$TMPDIR/trusty-embedderd-<PID>.sock`
///   - Falls back to `/tmp/trusty-embedderd-<PID>.sock` when `TMPDIR` is
///     empty (common on headless Linux).
///
/// Note: this path is used for the UDS transport
/// (`TRUSTY_EMBEDDER=unix:/path`). The default auto-spawn path uses the
/// stdio transport via `EmbedderSupervisor::spawn_stdio`.
/// Test: `default_socket_path_is_pid_specific`.
pub fn default_socket_path() -> PathBuf {
    let pid = std::process::id();
    let filename = format!("trusty-embedderd-{pid}.sock");

    let dir = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    dir.join(filename)
}

// ── Private utilities ─────────────────────────────────────────────────────────

fn parse_env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_env_u32(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for the supervisor façade.
    //!
    //! Why: we validate the deterministic, pure parts of the supervisor
    //! (config parsing, socket path, binary discovery) without needing a
    //! live ONNX binary. Process-lifecycle tests (spawn/restart/shutdown) are
    //! in `tests/embedder_supervisor_e2e.rs` and marked `#[ignore]`.
    //! Test: `cargo test -p trusty-search -- embedder_supervisor`.

    use super::*;

    // ── SupervisorConfig::from_env ──────────────────────────────────────────

    /// With no env vars set, `from_env()` must return the documented defaults.
    ///
    /// Why: catches accidental changes to the defaults that would silently
    /// break production deployments.
    /// What: remove all three vars, call `from_env()`, assert the fields.
    /// Test: this test.
    #[test]
    fn config_from_env_defaults() {
        let _g1 = EnvGuard::remove("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS");
        let _g2 = EnvGuard::remove("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS");
        let _g3 = EnvGuard::remove("TRUSTY_EMBEDDERD_MAX_RESTARTS");

        let cfg = SupervisorConfig::from_env();
        assert_eq!(cfg.startup_timeout_secs, 30);
        assert_eq!(cfg.backoff_max_secs, 60);
        assert_eq!(cfg.max_restarts, 5);
    }

    /// Env-var overrides must be parsed and applied correctly.
    ///
    /// Why: if `from_env()` ignores set vars, operators can't tune the
    /// supervisor without recompiling.
    /// What: set all three vars, call `from_env()`, assert the fields match.
    /// Test: this test.
    #[test]
    fn config_from_env_overrides() {
        let _g1 = EnvGuard::set("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", "15");
        let _g2 = EnvGuard::set("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", "120");
        let _g3 = EnvGuard::set("TRUSTY_EMBEDDERD_MAX_RESTARTS", "10");

        let cfg = SupervisorConfig::from_env();
        assert_eq!(cfg.startup_timeout_secs, 15);
        assert_eq!(cfg.backoff_max_secs, 120);
        assert_eq!(cfg.max_restarts, 10);
    }

    /// Malformed env var values must fall through to defaults without panicking.
    ///
    /// Why: operators may accidentally set `TRUSTY_EMBEDDERD_MAX_RESTARTS=abc`;
    /// the daemon must not crash on startup.
    /// What: set the vars to non-numeric strings and assert defaults are used.
    /// Test: this test.
    #[test]
    fn config_from_env_ignores_malformed() {
        let _g1 = EnvGuard::set("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", "not_a_number");
        let _g2 = EnvGuard::set("TRUSTY_EMBEDDERD_MAX_RESTARTS", "bad");

        let cfg = SupervisorConfig::from_env();
        assert_eq!(cfg.startup_timeout_secs, 30);
        assert_eq!(cfg.max_restarts, 5);
    }

    /// `into_common()` must map fields correctly to the trusty-common type.
    ///
    /// Why: field mismatch would silently use wrong defaults at runtime.
    /// What: construct a custom config, convert, and assert the common fields.
    /// Test: this test.
    #[test]
    fn into_common_maps_fields() {
        let cfg = SupervisorConfig {
            startup_timeout_secs: 99,
            backoff_max_secs: 77,
            max_restarts: 3,
        };
        let common = cfg.into_common();
        assert_eq!(common.startup_timeout_secs, 99);
        assert_eq!(common.backoff_max_secs, 77);
        assert_eq!(common.max_restarts, 3);
    }

    // ── default_socket_path ─────────────────────────────────────────────────

    /// The default socket path must be unique to the current process.
    ///
    /// Why: two daemons using the same socket would conflict at bind time.
    /// What: call `default_socket_path()` twice (same PID) — the results must
    /// be equal and contain the PID.
    /// Test: this test.
    #[test]
    fn default_socket_path_is_pid_specific() {
        let p = default_socket_path();
        let pid = std::process::id().to_string();
        assert!(
            p.to_string_lossy().contains(&pid),
            "socket path {p:?} must contain PID {pid}"
        );
        assert_eq!(
            p,
            default_socket_path(),
            "must be deterministic for same PID"
        );
    }

    /// The socket path must have a non-empty parent directory.
    ///
    /// Why: the supervisor creates the parent directory before spawning;
    /// an unparseable `TMPDIR` would cause `create_dir_all` to fail.
    /// What: assert the parent is non-None and non-empty.
    /// Test: this test.
    #[test]
    fn default_socket_path_has_parent() {
        let p = default_socket_path();
        assert!(
            p.parent().is_some_and(|pp| !pp.as_os_str().is_empty()),
            "socket path {p:?} must have a non-empty parent"
        );
    }

    // ── locate_embedderd_binary ─────────────────────────────────────────────

    /// When `TRUSTY_EMBEDDERD_BIN` points to a non-existent file, return an error.
    ///
    /// Why: an operator typo in the env var should produce a clear error at
    /// startup, not a confusing fallback.
    /// What: set `TRUSTY_EMBEDDERD_BIN` to a guaranteed non-existent path and
    /// assert the call returns `Err`.
    /// Test: this test.
    #[test]
    fn locate_binary_bad_explicit_path_errors() {
        let _g = EnvGuard::set("TRUSTY_EMBEDDERD_BIN", "/nonexistent/path/trusty-embedderd");
        let result = locate_embedderd_binary();
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    /// When `TRUSTY_EMBEDDERD_BIN` points to an existing file, return that path.
    ///
    /// Why: the explicit-path override is the canonical way to use a dev build.
    /// What: create a temp file, set the env var, and assert `Ok(path)`.
    /// Test: this test.
    #[test]
    fn locate_binary_via_explicit_env() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let _g = EnvGuard::set("TRUSTY_EMBEDDERD_BIN", path.to_str().unwrap());
        let result = locate_embedderd_binary();
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), path);
    }

    // ── Helper ──────────────────────────────────────────────────────────────

    /// RAII guard that restores an env var to its original state on drop.
    ///
    /// Why: env vars are global; leaking changes between tests causes flakiness
    /// in parallel runs.
    /// What: captures the old value on construction; restores or removes it on drop.
    /// Test: used by all env-var-touching tests in this module.
    struct EnvGuard {
        key: String,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: single-threaded tests; no indexing workers running.
            unsafe { std::env::set_var(key, value) }
            Self {
                key: key.to_owned(),
                old,
            }
        }

        fn remove(key: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: same invariant as above.
            unsafe { std::env::remove_var(key) }
            Self {
                key: key.to_owned(),
                old,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test teardown; no workers live past the test body.
            unsafe {
                match &self.old {
                    Some(v) => std::env::set_var(&self.key, v),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }
}

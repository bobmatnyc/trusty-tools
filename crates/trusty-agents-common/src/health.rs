//! The trusty-agents health socket: its path and its one method (#11550).
//!
//! Why: `tm tui` probes every service with one JSON-RPC health frame over a
//! Unix socket (ADR-0032). tagent served only per-project `<id>.sock` and
//! `<id>.ctrl.sock` sockets, so its tile had nothing to dial. The daemon and
//! the TUI both need the same path; computing it in one function here means
//! neither side publishes it to the other.
//! What: [`agents_socket`] resolves the path the way `mpm_rpc::mpm_socket`
//! does — an env override, else `daemon_socket_path("trusty-agents")`.
//! [`METHOD_AGENTS_HEALTH`] names the method and [`AgentsHealth`] its answer.
//! Test: `agents_socket_honours_the_env_override`,
//! `agents_socket_defaults_to_the_daemon_socket_path`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The app name tagent's health socket is derived under.
pub const AGENTS_APP_NAME: &str = "trusty-agents";

/// The health method tagent serves on [`agents_socket`].
pub const METHOD_AGENTS_HEALTH: &str = "agents.health";

/// Override for [`agents_socket`], so a test rig or sandbox points both the
/// daemon and a caller at a temp path.
pub const TRUSTY_AGENTS_SOCKET_ENV: &str = "TRUSTY_AGENTS_SOCKET";

/// The answer to [`METHOD_AGENTS_HEALTH`]: the `{status, version}` envelope
/// every other `<service>.health` method returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsHealth {
    /// `"ok"` while the process answers.
    pub status: String,
    /// The tagent crate version.
    pub version: String,
}

/// Resolve the well-known socket tagent binds for [`METHOD_AGENTS_HEALTH`].
///
/// Why: one function for the daemon and `tm tui`, so the two cannot drift.
/// What: a non-blank [`TRUSTY_AGENTS_SOCKET_ENV`] wins; else
/// `trusty_common::daemon_socket_path(AGENTS_APP_NAME)`.
///
/// # Errors
///
/// The data directory could not be resolved or created.
///
/// Test: `agents_socket_honours_the_env_override`,
/// `agents_socket_defaults_to_the_daemon_socket_path`.
pub fn agents_socket() -> anyhow::Result<PathBuf> {
    if let Ok(raw) = std::env::var(TRUSTY_AGENTS_SOCKET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    trusty_common::daemon_socket_path(AGENTS_APP_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn agents_socket_honours_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: ENV_LOCK serialises this module's env mutation.
        unsafe { std::env::set_var(TRUSTY_AGENTS_SOCKET_ENV, " /tmp/agents-x.sock ") };
        let got = agents_socket();
        unsafe { std::env::remove_var(TRUSTY_AGENTS_SOCKET_ENV) };
        assert_eq!(
            got.expect("override resolves"),
            PathBuf::from("/tmp/agents-x.sock")
        );
    }

    #[test]
    fn agents_socket_defaults_to_the_daemon_socket_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(TRUSTY_AGENTS_SOCKET_ENV) };
        let want = trusty_common::daemon_socket_path(AGENTS_APP_NAME).expect("data dir");
        assert_eq!(agents_socket().expect("resolves"), want);
        assert!(want.ends_with("trusty-agents.sock"), "{}", want.display());
    }
}

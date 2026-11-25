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
    if let Some(socket) = socket_override() {
        return Ok(socket);
    }
    trusty_common::daemon_socket_path(AGENTS_APP_NAME)
}

/// [`agents_socket`] for a dialer such as `tm tui`: same path, no directory.
///
/// Why: a dialer that created the data dir left it 0755, so the dial's 0700
/// trust check refused it and the Agents tile read DOWN (#11667). The daemon
/// keeps [`agents_socket`]; its bind creates the directory at 0700.
/// What: a non-blank [`TRUSTY_AGENTS_SOCKET_ENV`] wins; else
/// `trusty_common::daemon_socket_path_no_create(AGENTS_APP_NAME)`.
///
/// # Errors
///
/// The data directory could not be resolved.
///
/// Test: `agents_dial_socket_matches_and_creates_nothing`.
pub fn agents_dial_socket() -> anyhow::Result<PathBuf> {
    if let Some(socket) = socket_override() {
        return Ok(socket);
    }
    // #11667: resolve only; the bind side owns directory creation.
    trusty_common::daemon_socket_path_no_create(AGENTS_APP_NAME)
}

fn socket_override() -> Option<PathBuf> {
    let raw = std::env::var(TRUSTY_AGENTS_SOCKET_ENV).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
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

    /// #11667: the dial path equals the bind path and creates nothing.
    #[test]
    fn agents_dial_socket_matches_and_creates_nothing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("agents-dial-{}", std::process::id()));
        // SAFETY: ENV_LOCK serialises this module's env mutation.
        unsafe {
            std::env::remove_var(TRUSTY_AGENTS_SOCKET_ENV);
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let dial = agents_dial_socket();
        let created = tmp.join(AGENTS_APP_NAME).exists();
        let bind = agents_socket();
        unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(!created, "the dial lookup created the data dir");
        assert_eq!(dial.expect("dial"), bind.expect("bind"));
    }
}

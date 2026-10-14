//! Which tmux server this process may reach (#9557).
//!
//! Why: [`crate::core::host_state_gate`] refuses tmux under a scratch `$HOME`,
//! because tmux is keyed to the OS user and a scratch daemon would otherwise
//! reach the operator's live server. That left a `tm daemon --sandbox` (#9121)
//! with no tmux at all, so tmux-dependent fixes could not be live-checked in
//! isolation. Lifting the gate alone would put the sandbox on the default
//! socket.
//! What: outside sandbox mode the host-state verdict decides, unchanged. With
//! `TRUSTY_SANDBOX=1`, tmux is allowed only on a server the sandbox owns:
//! `TMUX_TMPDIR` must name a directory inside `TRUSTY_DATA_DIR_OVERRIDE`, owned
//! by this uid and writable by no one else, and `$TMUX` (which names another
//! server's socket and wins over `TMUX_TMPDIR`) must be unset. tmux then puts
//! its socket at `$TMUX_TMPDIR/tmux-<uid>/default`, and every child inherits
//! the same variable. Anything else refuses; the sandbox flag also overrides
//! `TRUSTY_MPM_ALLOW_HOST_STATE`, so a sandbox never reaches the default
//! socket. Only tmux gates use this; the host process table stays on the
//! host-state gate.
//! Test: `tmux_access_tests.rs`; end to end, `tests/sandbox_tmux_9557.rs`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::core::host_state_gate::HostStateAccess;

/// The variable that moves tmux's socket directory.
pub const TMUX_TMPDIR_ENV: &str = "TMUX_TMPDIR";

/// The verdict for a tmux spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxAccess {
    /// Not a sandbox: the host-state gate's verdict.
    Host(HostStateAccess),
    /// A sandbox whose tmux socket directory it owns.
    SandboxServer {
        /// The canonical `TMUX_TMPDIR`.
        tmux_tmpdir: PathBuf,
    },
    /// A sandbox with no socket directory of its own: tmux is refused.
    SandboxRefused {
        /// Why the socket directory does not qualify.
        detail: String,
    },
}

impl TmuxAccess {
    /// True when the caller may spawn tmux.
    pub fn is_allowed(&self) -> bool {
        match self {
            Self::Host(host) => host.is_allowed(),
            Self::SandboxServer { .. } => true,
            Self::SandboxRefused { .. } => false,
        }
    }

    /// The operator-facing refusal, or `None` when allowed.
    ///
    /// Test: `a_refused_sandbox_names_the_variable`.
    pub fn skip_reason(&self) -> Option<String> {
        match self {
            Self::Host(host) => host.skip_reason(),
            Self::SandboxServer { .. } => None,
            Self::SandboxRefused { detail } => Some(format!(
                "this is a sandbox (TRUSTY_SANDBOX=1) without a tmux server of its own: \
                 {detail}. A sandbox reaches tmux only through {TMUX_TMPDIR_ENV} set to a \
                 directory under the sandbox data dir, which the sandbox launcher sets (#9557)"
            )),
        }
    }
}

/// The inputs [`classify_tmux_access`] reads from the environment.
#[derive(Debug, Clone, Default)]
pub struct TmuxEnv<'a> {
    /// `TRUSTY_SANDBOX`.
    pub sandbox_flag: Option<&'a OsStr>,
    /// `$TMUX`.
    pub tmux: Option<&'a OsStr>,
    /// `TMUX_TMPDIR`.
    pub tmux_tmpdir: Option<&'a Path>,
    /// `TRUSTY_DATA_DIR_OVERRIDE`.
    pub data_dir_override: Option<&'a Path>,
}

/// Decide tmux access from `env`, with `host` as the non-sandbox verdict.
///
/// What: not a sandbox → `Host(host())`; `host` is not evaluated otherwise.
/// A sandbox → [`sandbox_socket_dir`]'s answer.
/// Test: `outside_a_sandbox_the_host_verdict_decides`,
/// `a_sandbox_with_its_own_socket_dir_is_allowed`,
/// `the_sandbox_flag_overrides_a_host_allow`.
pub fn classify_tmux_access(
    env: &TmuxEnv<'_>,
    host: impl FnOnce() -> HostStateAccess,
) -> TmuxAccess {
    if !trusty_common::credentials::sandbox_flag_set(env.sandbox_flag) {
        return TmuxAccess::Host(host());
    }
    match sandbox_socket_dir(env) {
        Ok(tmux_tmpdir) => TmuxAccess::SandboxServer { tmux_tmpdir },
        Err(detail) => TmuxAccess::SandboxRefused { detail },
    }
}

/// The sandbox's own tmux socket directory, or why there is none.
///
/// What: `Err` when `$TMUX` is non-empty, when `TMUX_TMPDIR` or the data-dir
/// override is unset or does not resolve, when `TMUX_TMPDIR` is not strictly
/// inside the override, or when it is not a directory owned by this uid with
/// no group or other write bit. Both paths are compared canonical, so a
/// symlink cannot point the socket outside the sandbox.
/// Test: `refuses_when_tmux_is_set`, `refuses_without_tmux_tmpdir`,
/// `refuses_a_tmux_tmpdir_outside_the_data_dir`,
/// `refuses_a_group_writable_tmux_tmpdir`,
/// `refuses_a_symlink_out_of_the_data_dir`.
fn sandbox_socket_dir(env: &TmuxEnv<'_>) -> Result<PathBuf, String> {
    if env.tmux.is_some_and(|v| !v.is_empty()) {
        return Err("$TMUX is set, and it names another server's socket".to_string());
    }
    let tmpdir = env
        .tmux_tmpdir
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| format!("{TMUX_TMPDIR_ENV} is not set"))?;
    let data = env
        .data_dir_override
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| format!("{} is not set", trusty_common::DATA_DIR_OVERRIDE_ENV))?;
    let tmpdir = tmpdir.canonicalize().map_err(|e| {
        format!(
            "{TMUX_TMPDIR_ENV} ({}) does not resolve: {e}",
            tmpdir.display()
        )
    })?;
    let data = data.canonicalize().map_err(|e| {
        format!(
            "the sandbox data dir ({}) does not resolve: {e}",
            data.display()
        )
    })?;
    if tmpdir == data || !tmpdir.starts_with(&data) {
        return Err(format!(
            "{TMUX_TMPDIR_ENV} ({}) is not inside the sandbox data dir ({})",
            tmpdir.display(),
            data.display()
        ));
    }
    owned_private_dir(&tmpdir)?;
    Ok(tmpdir)
}

/// `Ok` when `dir` is a directory owned by this uid with no group/other write.
#[cfg(unix)]
fn owned_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot stat {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != nix::unistd::Uid::current().as_raw() {
        return Err(format!("{} is not owned by this user", dir.display()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!("{} is writable by other users", dir.display()));
    }
    Ok(())
}

/// No tmux off unix, so no sandbox socket either.
#[cfg(not(unix))]
fn owned_private_dir(dir: &Path) -> Result<(), String> {
    Err(format!("{} cannot be checked off unix", dir.display()))
}

/// The tmux verdict for THIS process.
///
/// Test: covered through [`classify_tmux_access`]; end to end by
/// `a_sandbox_daemon_reaches_only_its_own_tmux_server`.
pub fn tmux_access() -> TmuxAccess {
    with_process_env(|env| {
        classify_tmux_access(env, crate::core::host_state_gate::host_state_access)
    })
}

/// [`tmux_access`] for a caller that runs against `data_root` (#6348).
///
/// Test: covered through [`classify_tmux_access`].
pub fn tmux_access_for_root(data_root: &Path) -> TmuxAccess {
    with_process_env(|env| {
        classify_tmux_access(env, || {
            crate::core::host_state_gate::host_state_access_for_root(data_root)
        })
    })
}

/// Read the four inputs from this process's environment and run `f`.
fn with_process_env<T>(f: impl FnOnce(&TmuxEnv<'_>) -> T) -> T {
    let sandbox = std::env::var_os(trusty_common::credentials::SANDBOX_ENV_VAR);
    let tmux = std::env::var_os("TMUX");
    let tmpdir = std::env::var_os(TMUX_TMPDIR_ENV).map(PathBuf::from);
    let data = std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).map(PathBuf::from);
    f(&TmuxEnv {
        sandbox_flag: sandbox.as_deref(),
        tmux: tmux.as_deref(),
        tmux_tmpdir: tmpdir.as_deref(),
        data_dir_override: data.as_deref(),
    })
}

#[cfg(test)]
#[path = "tmux_access_tests.rs"]
mod tests;

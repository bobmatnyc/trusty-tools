//! The sccache unix-socket server, for UNSANDBOXED builds only (#8928 step 3).
//!
//! Why: one long-lived sccache server per machine, started by tm at daemon
//! start, keeps the compile cache warm for unsandboxed cargo builds over a
//! unix socket (`SCCACHE_SERVER_UDS`, sccache 0.9.0+) rather than a TCP port.
//! A sandboxed session must NOT reach it: whoever connects names the compiler,
//! and the server runs that program itself, outside the sandbox — on sccache
//! 0.17.0 a script passed as `rustc` ran under the server, whose parent is
//! launchd, and `SCCACHE_CLIENT_SIDE=1` did not prevent it (#8928 fix round).
//! So no sandboxed role's `allowUnixSockets` names this socket, and sandboxed
//! sessions build with `RUSTC_WRAPPER=""` (no sccache at all; see
//! `core::session_launch::sandbox_settings`).
//! What: [`socket_path_at`] names the socket; [`client_env`] and
//! [`server_env`] are the variables each side needs; [`probe_socket`]
//! classifies the path; [`ensure_server`] checks the sccache version and then
//! starts `sccache --start-server` on it unless a server already answers;
//! [`start_if_requested`] is the daemon's entry, a no-op unless
//! `build.sccache: true`. The server's idle timeout is off
//! (`SCCACHE_IDLE_TIMEOUT=0`): the default 600 s would let it exit. tm never
//! writes `~/.cargo/config.toml`.
//! Test: `sccache_socket_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The socket's file name under `~/.trusty-mpm/`.
pub const SOCKET_FILE: &str = "sccache.sock";

/// The first sccache release that serves `SCCACHE_SERVER_UDS` (PR #2206).
pub const MIN_UDS_VERSION: (u32, u32, u32) = (0, 9, 0);

/// How long `sccache --start-server` may take before it is killed.
const START_BUDGET: Duration = Duration::from_secs(15);

/// The socket path under `home`: `~/.trusty-mpm/sccache.sock`.
///
/// Why: short, so it stays under the 104-byte `sun_path` limit, and under the
/// directory tm already owns.
/// Test: `the_socket_lives_under_trusty_mpm`.
pub fn socket_path_at(home: &Path) -> PathBuf {
    home.join(".trusty-mpm").join(SOCKET_FILE)
}

/// What an UNSANDBOXED cargo client sets to use the server. Never handed to a
/// sandboxed session: the server would run its compiler outside the sandbox.
/// Test: `client_and_server_env_name_the_socket`.
pub fn client_env(socket: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("SCCACHE_SERVER_UDS", socket.display().to_string()),
        ("SCCACHE_CLIENT_SIDE", "1".to_owned()),
    ]
}

/// What the server needs: the socket, and no idle shutdown.
/// Test: `client_and_server_env_name_the_socket`.
pub fn server_env(socket: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("SCCACHE_SERVER_UDS", socket.display().to_string()),
        ("SCCACHE_IDLE_TIMEOUT", "0".to_owned()),
    ]
}

/// `sccache --version` output parsed to `(major, minor, patch)`.
/// Test: `versions_parse_and_compare`.
pub fn parse_version(stdout: &str) -> Option<(u32, u32, u32)> {
    let word = stdout.split_whitespace().nth(1)?;
    let mut parts = word.split(['.', '-']).map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Wall clock `sccache --version` may take.
const VERSION_BUDGET: Duration = Duration::from_secs(3);

/// `<bin> --version`, bounded and parsed; `Err(why)` when it cannot run,
/// fails, or prints no version.
/// Test: `a_too_old_sccache_is_refused_before_starting`.
pub fn read_version(bin: &Path) -> Result<(u32, u32, u32), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    let out =
        crate::core::bounded_proc::run_bounded(cmd, VERSION_BUDGET).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("it exited with {}", out.status));
    }
    parse_version(&out.stdout).ok_or_else(|| format!("unparseable output {:?}", out.stdout.trim()))
}

/// What is at the socket path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SocketState {
    /// Nothing at the path.
    Missing,
    /// A server accepted a connection.
    Listening,
    /// A socket file that refuses connections: the server exited.
    Stale(String),
    /// Something that is not a socket, or a path that cannot be examined.
    Unknown(String),
}

/// Classify `path` by connecting to it.
///
/// What: `Missing` when absent; `Listening` when a connect succeeds; `Stale`
/// when the file is a socket that refuses; `Unknown` for a non-socket or an
/// error reading the path — never `Listening` without a connection.
/// Test: `a_listening_socket_is_listening`, `a_stale_socket_is_stale`,
/// `a_regular_file_is_unknown`.
pub fn probe_socket(path: &Path) -> SocketState {
    use std::os::unix::fs::FileTypeExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketState::Missing,
        Err(e) => return SocketState::Unknown(e.to_string()),
    };
    if !meta.file_type().is_socket() {
        return SocketState::Unknown("the path is not a socket".to_owned());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => SocketState::Listening,
        Err(e) => SocketState::Stale(e.to_string()),
    }
}

/// What [`ensure_server`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StartOutcome {
    /// A server already answered on the socket.
    AlreadyListening,
    /// This call started one, and it answers.
    Started,
}

/// Why the server could not be started.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SccacheSocketError {
    /// `<bin> --version` could not be read, so the start was refused.
    #[error("cannot read `{bin} --version`: {why}; no sccache server was started")]
    Version {
        /// The sccache binary.
        bin: PathBuf,
        /// What went wrong.
        why: String,
    },
    /// The installed sccache predates `SCCACHE_SERVER_UDS`.
    #[error(
        "`{bin}` is sccache {version}, older than 0.9.0, which first serves \
         `SCCACHE_SERVER_UDS`; no sccache server was started"
    )]
    TooOld {
        /// The sccache binary.
        bin: PathBuf,
        /// The version it reported.
        version: String,
    },
    /// The crate config could not be read, so the opt-in is unknown.
    #[error("the sccache opt-in is unknown: {0}")]
    Config(String),
    /// The socket path holds something that is not a usable socket.
    #[error("`{path}` cannot hold the sccache socket: {why}")]
    BadPath {
        /// The socket path.
        path: PathBuf,
        /// What is wrong with it.
        why: String,
    },
    /// Creating the directory or removing a stale socket failed.
    #[error("preparing `{path}` failed: {source}")]
    Io {
        /// The path acted on.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// `sccache --start-server` could not run, failed, or timed out.
    #[error("`{bin} --start-server` failed: {why}")]
    Start {
        /// The sccache binary.
        bin: PathBuf,
        /// What happened.
        why: String,
    },
    /// The start command succeeded but nothing answers on the socket.
    #[error("sccache started but nothing answers on `{path}`: {state:?}")]
    NotListening {
        /// The socket path.
        path: PathBuf,
        /// What the probe found.
        state: SocketState,
    },
}

/// Start an sccache server on `socket` unless one already answers there.
///
/// What: a listening socket is left alone; a stale socket file is removed; a
/// non-socket or unreadable path is an error (never removed). The parent
/// directory is created, then `<bin> --start-server` runs with [`server_env`]
/// and null stdio, killed after [`START_BUDGET`]. The socket is probed again
/// and must answer.
///
/// # Errors
///
/// [`SccacheSocketError`] for each failure above.
///
/// Test: `ensure_server_leaves_a_listening_server_alone`,
/// `ensure_server_starts_and_verifies`, `ensure_server_refuses_a_non_socket`,
/// `ensure_server_reports_a_server_that_never_listens`.
pub fn ensure_server(bin: &Path, socket: &Path) -> Result<StartOutcome, SccacheSocketError> {
    match probe_socket(socket) {
        SocketState::Listening => return Ok(StartOutcome::AlreadyListening),
        SocketState::Missing => {}
        SocketState::Stale(_) => {
            std::fs::remove_file(socket).map_err(|source| SccacheSocketError::Io {
                path: socket.to_path_buf(),
                source,
            })?;
        }
        SocketState::Unknown(why) => {
            return Err(SccacheSocketError::BadPath {
                path: socket.to_path_buf(),
                why,
            });
        }
    }
    check_version(bin)?;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SccacheSocketError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let start = |why: String| SccacheSocketError::Start {
        bin: bin.to_path_buf(),
        why,
    };
    let mut cmd = Command::new(bin);
    cmd.arg("--start-server")
        .envs(server_env(socket))
        .env_remove("SCCACHE_CLIENT_SIDE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| start(e.to_string()))?;
    let deadline = Instant::now() + START_BUDGET;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(start(format!("timed out after {START_BUDGET:?}")));
            }
            Err(e) => return Err(start(e.to_string())),
        }
    };
    if !status.success() {
        return Err(start(format!("exited with {status}")));
    }
    match probe_socket(socket) {
        SocketState::Listening => Ok(StartOutcome::Started),
        state => Err(SccacheSocketError::NotListening {
            path: socket.to_path_buf(),
            state,
        }),
    }
}

/// Whether `home`'s crate config sets `build.sccache: true`.
///
/// What: `Ok(false)` for an absent file, section or key; `Err(why)` for a
/// config that cannot be read or parsed — never a silent `false`.
/// Test: `start_if_requested_is_a_no_op_without_the_opt_in`,
/// `an_unreadable_config_is_an_error_not_a_skip`.
pub fn sccache_requested(home: &Path) -> Result<bool, String> {
    let path = trusty_common::crate_config::crate_config_path_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    );
    let config = trusty_common::crate_config::load_at::<
        crate::core::trusty_tools_config::TrustyToolsConfig,
    >(&path)
    .map_err(|e| format!("`{}` cannot be read: {e}", path.display()))?;
    Ok(config
        .as_ref()
        .and_then(|c| c.build.as_ref())
        .and_then(|b| b.sccache)
        == Some(true))
}

/// Refuse an sccache older than [`MIN_UDS_VERSION`], or one whose version
/// cannot be read (#8928 fix round): it would ignore `SCCACHE_SERVER_UDS` and
/// start a TCP server that never exits.
fn check_version(bin: &Path) -> Result<(), SccacheSocketError> {
    let version = read_version(bin).map_err(|why| SccacheSocketError::Version {
        bin: bin.to_path_buf(),
        why,
    })?;
    if version < MIN_UDS_VERSION {
        let (major, minor, patch) = version;
        return Err(SccacheSocketError::TooOld {
            bin: bin.to_path_buf(),
            version: format!("{major}.{minor}.{patch}"),
        });
    }
    Ok(())
}

/// The daemon's entry: start the server when `home`'s config asks for sccache.
///
/// What: unless [`sccache_requested`] says `true`, does nothing and returns
/// `None`; an unreadable config is `Some(Err(Config))`. Otherwise resolves
/// `sccache` on `PATH` and runs [`ensure_server`] on [`socket_path_at`]. A
/// missing binary is an error, never a silent skip.
/// Test: `start_if_requested_is_a_no_op_without_the_opt_in`,
/// `an_unreadable_config_is_an_error_not_a_skip`.
pub fn start_if_requested(home: &Path) -> Option<Result<StartOutcome, SccacheSocketError>> {
    match sccache_requested(home) {
        Ok(true) => {}
        Ok(false) => return None,
        Err(why) => return Some(Err(SccacheSocketError::Config(why))),
    }
    let socket = socket_path_at(home);
    let Some(bin) = trusty_common::bin_resolve::resolve_binary("sccache") else {
        return Some(Err(SccacheSocketError::Start {
            bin: PathBuf::from("sccache"),
            why: "not found on PATH".to_owned(),
        }));
    };
    Some(ensure_server(&bin, &socket))
}

/// [`start_if_requested`], with the outcome logged: the daemon's call.
/// Test: the outcomes are covered by `start_if_requested`'s tests; this only
/// logs them.
pub fn start_and_log(home: &Path) {
    match start_if_requested(home) {
        None => {}
        Some(Ok(outcome)) => tracing::info!(
            "sccache socket server on {}: {outcome:?}",
            socket_path_at(home).display()
        ),
        Some(Err(e)) => tracing::warn!("sccache socket server not started (#8928): {e}"),
    }
}

#[cfg(test)]
#[path = "sccache_socket_tests.rs"]
mod tests;

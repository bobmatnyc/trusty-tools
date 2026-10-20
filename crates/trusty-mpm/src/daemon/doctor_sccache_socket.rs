//! The `tm doctor` `sccache_socket` row (#8928 step 3).
//!
//! Why: a sandboxed cargo build reaches sccache only through the unix-socket
//! server the tm daemon starts (`core::sccache_socket`). Nothing else says
//! whether that server is up, so a session whose cache silently misses looks
//! like a slow build.
//! What: [`check_sccache_socket`] gathers [`SccacheFacts`] and [`build_check`]
//! folds them. Not applicable (`Ok`) unless `build.sccache: true`. Then: `Ok`
//! when a server answers on `~/.trusty-mpm/sccache.sock`; `Warn` when sccache
//! is not on `PATH`, or the socket is missing or stale; `Fail` when the
//! installed sccache predates `SCCACHE_SERVER_UDS` (0.9.0); `Unknown` — never
//! `Ok` — when the config, the version or the path cannot be read. Read-only:
//! connecting to the socket is the only action.
//! Test: `doctor_sccache_socket_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::sccache_socket::{
    MIN_UDS_VERSION, SocketState, parse_version, probe_socket, sccache_requested, socket_path_at,
};

/// Stable check name.
pub(super) const CHECK_NAME: &str = "sccache_socket";

/// Wall clock `sccache --version` may take.
const VERSION_BUDGET: Duration = Duration::from_secs(3);

/// What the probe learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SccacheFacts {
    /// `build.sccache` is not `true`.
    NotRequested,
    /// The crate config could not be read; the string says why.
    ConfigUnreadable(String),
    /// `build.sccache: true` and no `sccache` on `PATH`.
    NoBinary,
    /// The binary, its version (or why it is unknown), and the socket.
    Probed {
        /// The resolved binary.
        bin: PathBuf,
        /// `(major, minor, patch)`, or why it could not be read.
        version: Result<(u32, u32, u32), String>,
        /// The socket path.
        socket: PathBuf,
        /// What is at the socket path.
        state: SocketState,
    },
}

/// Fold the facts into the row (pure).
/// Test: every test in `doctor_sccache_socket_tests.rs`.
pub(super) fn build_check(facts: &SccacheFacts) -> DoctorCheck {
    let row = |status, message: String| DoctorCheck::new(CHECK_NAME, status, message);
    let (bin, version, socket, state) = match facts {
        SccacheFacts::NotRequested => {
            return row(
                CheckStatus::Ok,
                "`build.sccache` is not true, so no sccache socket server is started — not \
                 applicable (#8928)"
                    .to_owned(),
            );
        }
        SccacheFacts::ConfigUnreadable(why) => {
            return row(
                CheckStatus::Unknown,
                format!("cannot tell whether sccache is requested: {why} (#8928)"),
            );
        }
        SccacheFacts::NoBinary => {
            return row(
                CheckStatus::Warn,
                "`build.sccache` is true but `sccache` is not on PATH, so sandboxed builds get no \
                 cache — install sccache 0.9.0 or later, or set `build.sccache: false` (#8928)"
                    .to_owned(),
            );
        }
        SccacheFacts::Probed {
            bin,
            version,
            socket,
            state,
        } => (bin, version, socket, state),
    };
    let version = match version {
        Ok(v) => *v,
        Err(why) => {
            return row(
                CheckStatus::Unknown,
                format!(
                    "cannot read `{} --version`: {why}, so whether it serves a unix socket is \
                     unknown (#8928)",
                    bin.display()
                ),
            );
        }
    };
    let (major, minor, patch) = version;
    if version < MIN_UDS_VERSION {
        return row(
            CheckStatus::Fail,
            format!(
                "sccache {major}.{minor}.{patch} at {} predates 0.9.0 and cannot serve \
                 `SCCACHE_SERVER_UDS` — upgrade sccache (#8928)",
                bin.display()
            ),
        );
    }
    let at = socket.display();
    let restart = "restart the tm daemon (`tm restart`), which starts the server when \
                   `build.sccache` is true";
    match state {
        SocketState::Listening => row(
            CheckStatus::Ok,
            format!(
                "sccache {major}.{minor}.{patch} serves {at}; sandboxed cargo uses \
                 `SCCACHE_SERVER_UDS={at} SCCACHE_CLIENT_SIDE=1` (#8928)"
            ),
        ),
        SocketState::Missing => row(
            CheckStatus::Warn,
            format!(
                "no sccache server on {at}, so sandboxed builds get no cache — {restart} (#8928)"
            ),
        ),
        SocketState::Stale(why) => row(
            CheckStatus::Warn,
            format!("{at} refuses connections ({why}): the server exited — {restart} (#8928)"),
        ),
        SocketState::Unknown(why) => row(
            CheckStatus::Unknown,
            format!("cannot examine {at}: {why} (#8928)"),
        ),
    }
}

/// Probe this machine and report the sccache socket server.
pub(super) fn check_sccache_socket(home: &Path) -> DoctorCheck {
    build_check(&gather(home))
}

/// [`check_sccache_socket`]'s fact-gathering half.
/// Test: `gather_reports_not_requested_for_an_empty_home`.
pub(super) fn gather(home: &Path) -> SccacheFacts {
    match sccache_requested(home) {
        Ok(true) => {}
        Ok(false) => return SccacheFacts::NotRequested,
        Err(why) => return SccacheFacts::ConfigUnreadable(why),
    }
    let Some(bin) = trusty_common::bin_resolve::resolve_binary("sccache") else {
        return SccacheFacts::NoBinary;
    };
    let socket = socket_path_at(home);
    SccacheFacts::Probed {
        version: read_version(&bin),
        state: probe_socket(&socket),
        socket,
        bin,
    }
}

/// `<bin> --version`, bounded and parsed.
fn read_version(bin: &Path) -> Result<(u32, u32, u32), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    let out =
        crate::core::bounded_proc::run_bounded(cmd, VERSION_BUDGET).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("it exited with {}", out.status));
    }
    parse_version(&out.stdout).ok_or_else(|| format!("unparseable output {:?}", out.stdout.trim()))
}

#[cfg(test)]
#[path = "doctor_sccache_socket_tests.rs"]
mod tests;

//! `trusty-memory stop` under `TRUSTY_DATA_DIR_OVERRIDE` (#9140).
//!
//! Why: a sandbox cleanup ran `trusty-memory stop` with the override set, and
//! `stop` went to the live `com.trusty.memory` launchd unit as it always did.
//! The live daemon was down for about 40 s. An override names one data dir, so
//! `stop` may touch only the daemon that owns that dir.
//! What: [`StopTarget::resolve`] reads the override. With none, the target is
//! the live install. With one, it connects to the override's daemon socket and
//! asks the kernel for the peer pid: the process serving that socket owns the
//! data dir. [`stop_target`] then signals that pid alone, and only when the
//! process table shows it as a `trusty-memory` daemon. Any doubt is an error.
//! `service stop` refuses outright under an override
//! ([`refuse_live_unit_under_override`]).
//! Test: `stop_under_a_data_dir_override_never_reaches_the_live_unit`,
//! `sandbox_stop_without_proof_of_ownership_fails_and_signals_nothing`,
//! `sandbox_stop_signals_only_the_socket_owner`,
//! `stop_target_resolves_the_override_socket_owner_by_peer_pid`,
//! `service_stop_is_refused_under_a_data_dir_override`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use trusty_common::DATA_DIR_OVERRIDE_ENV;

use super::{daemon_pids_in, stop_daemons_in, ProcInfo};

/// How long the ownership probe may take to connect to the override's socket.
const OWNER_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Which daemon `stop` is allowed to signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopTarget {
    /// No override: the live install — launchd unit, then the process table.
    Live,
    /// An override is set: only the process serving `socket`, the override's
    /// daemon socket. `owner` is its peer pid; `None` when nothing answered.
    Sandbox { socket: PathBuf, owner: Option<u32> },
}

impl StopTarget {
    /// Decide the target from `TRUSTY_DATA_DIR_OVERRIDE`.
    ///
    /// Why: see the module doc. A blank override is refused rather than read
    /// as "unset": `resolve_data_dir` maps it to the live data dir, so the
    /// socket owner it finds would be the live daemon.
    /// What: unset gives [`StopTarget::Live`]; set gives
    /// [`StopTarget::Sandbox`] with the peer pid of the override's socket.
    ///
    /// # Errors
    ///
    /// A blank override, and an override the data-dir resolver rejects.
    ///
    /// Test: `stop_under_a_data_dir_override_never_reaches_the_live_unit`,
    /// `stop_target_resolves_the_override_socket_owner_by_peer_pid`.
    pub(crate) async fn resolve() -> Result<Self> {
        let Some(raw) = std::env::var_os(DATA_DIR_OVERRIDE_ENV) else {
            return Ok(Self::Live);
        };
        if raw.to_string_lossy().trim().is_empty() {
            bail!(
                "{DATA_DIR_OVERRIDE_ENV} is set but blank, which resolves the live data \
                 dir; refusing to stop anything. Unset it to stop the live daemon (#9140)"
            );
        }
        let socket = crate::socket_path()
            .with_context(|| format!("resolve the daemon socket under {DATA_DIR_OVERRIDE_ENV}"))?;
        let owner = socket_owner_pid(&socket).await;
        Ok(Self::Sandbox { socket, owner })
    }
}

/// The pid of the process serving `socket`, by the kernel's peer credentials.
///
/// `None` when nothing accepts within [`OWNER_PROBE_TIMEOUT`] or the platform
/// cannot name the peer.
async fn socket_owner_pid(socket: &Path) -> Option<u32> {
    let connect = tokio::net::UnixStream::connect(socket);
    let stream = tokio::time::timeout(OWNER_PROBE_TIMEOUT, connect)
        .await
        .ok()?
        .ok()?;
    trusty_common::uds::peer_pid(&stream)
}

/// Stop the daemon `target` allows, and nothing else.
///
/// Why (#9140): the routing is the fix, so it is a function a test can drive
/// with a `stop_live` that records whether the live path was reached.
/// What: [`StopTarget::Live`] runs `stop_live`. [`StopTarget::Sandbox`] never
/// runs it: the proven owner, if it is a `trusty-memory` daemon in `procs`,
/// goes through [`stop_daemons_in`] alone.
///
/// # Errors
///
/// No proven owner; an owner that is this process or not a `trusty-memory`
/// daemon in `procs`; and every error of the stop that runs.
///
/// Test: `stop_under_a_data_dir_override_never_reaches_the_live_unit`,
/// `sandbox_stop_without_proof_of_ownership_fails_and_signals_nothing`,
/// `sandbox_stop_signals_only_the_socket_owner`.
pub(crate) fn stop_target(
    target: &StopTarget,
    procs: &[ProcInfo],
    me: u32,
    grace: Duration,
    stop_live: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let (socket, owner) = match target {
        StopTarget::Live => return stop_live(),
        StopTarget::Sandbox { socket, owner } => (socket, *owner),
    };
    let Some(pid) = owner else {
        bail!(
            "cannot prove which trusty-memory daemon owns the {DATA_DIR_OVERRIDE_ENV} data \
             dir: nothing answers on {}, or its peer pid is unreadable. Stopped nothing; \
             under an override `stop` never touches the live launchd unit (#9140)",
            socket.display()
        );
    };
    if !daemon_pids_in(procs, me).contains(&pid) {
        bail!(
            "pid {pid} serves {} but is not a trusty-memory daemon in the process table; \
             refusing to signal it (#9140)",
            socket.display()
        );
    }
    let owned: Vec<ProcInfo> = procs.iter().filter(|p| p.pid == pid).cloned().collect();
    stop_daemons_in(&owned, me, grace)
}

/// Refuse a command that acts on the live launchd unit while an override is
/// set (#9140).
///
/// Why: the unit is the live install's, whatever data dir this process
/// resolves; `service stop` boots it out with no data-dir check at all.
/// What: any `TRUSTY_DATA_DIR_OVERRIDE` value, blank included, is a refusal
/// naming `command`.
/// Test: `service_stop_is_refused_under_a_data_dir_override`.
pub(crate) fn refuse_live_unit_under_override(command: &str) -> Result<()> {
    if std::env::var_os(DATA_DIR_OVERRIDE_ENV).is_some() {
        bail!(
            "`trusty-memory {command}` acts on the live launchd unit, and \
             {DATA_DIR_OVERRIDE_ENV} is set; refusing. Use `trusty-memory stop` to stop \
             the override's own daemon (#9140)"
        );
    }
    Ok(())
}

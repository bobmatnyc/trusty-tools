//! Socket-health discovery for the trusty-mpm daemon (#6288 PR 1b).
//!
//! Why: every in-repo client reaches the daemon over its Unix socket since
//! #6288 PR 1a, and the daemon's HTTP bind on 7880 is retired in PR 2. A
//! `tm services` entry that GETs `http://localhost:7880/health` would then
//! report a live daemon DOWN, exactly as #9543 found for trusty-search.
//! What: the [`MpmSocketProber`] seam and its production impl, which calls
//! `mpm.health` on the socket `trusty_common::mpm_rpc::mpm_socket` resolves —
//! the same path, and the same `TRUSTY_MPM_SOCKET` override, every other mpm
//! client uses.
//! Test: `crate::services::uds_mpm_tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use super::HealthState;

/// The daemon's health method (`daemon::rpc::core`).
pub(crate) const METHOD_MPM_HEALTH: &str = "mpm.health";

/// Trait for trusty-mpm socket-health probing.
///
/// Why: lets unit tests replace the socket dial, as
/// [`super::SearchSocketProber`] does for trusty-search.
/// What: `socket()` names the daemon's socket; `mpm_health()` calls
/// `mpm.health` there within `timeout` and maps the outcome to a
/// [`HealthState`].
/// Test: `uds_mpm_is_up_against_a_socket_only_daemon`,
/// `uds_mpm_is_down_on_a_missing_socket_or_an_error_answer`.
pub trait MpmSocketProber: Send + Sync {
    /// The socket the trusty-mpm daemon binds.
    fn socket(&self) -> PathBuf;

    /// Call `mpm.health` on `socket`: `Ok` on a health answer, `Fail` otherwise.
    fn mpm_health(&self, socket: &Path, timeout: Duration) -> HealthState;
}

/// Production prober over `mpm_rpc::mpm_socket` and one framed request.
///
/// What: a dial failure, a timeout, a JSON-RPC error frame, or a result with
/// no string `status` is `Fail`, carrying the reason and the socket path. Only
/// a health envelope is `Ok`: a failed probe never reads as healthy.
/// Test: `uds_mpm_is_up_against_a_socket_only_daemon`,
/// `uds_mpm_is_down_on_a_missing_socket_or_an_error_answer`.
pub struct RealMpmSocketProber;

impl MpmSocketProber for RealMpmSocketProber {
    fn socket(&self) -> PathBuf {
        trusty_common::mpm_rpc::mpm_socket()
    }

    fn mpm_health(&self, socket: &Path, timeout: Duration) -> HealthState {
        match health_blocking(socket, timeout) {
            Ok(()) => HealthState::Ok,
            Err(reason) => HealthState::Fail {
                detail: format!(
                    "trusty-mpm daemon unreachable at {}: {reason}",
                    socket.display()
                ),
            },
        }
    }
}

/// One `mpm.health` call from a synchronous caller.
///
/// Why a joined thread: `tm services` runs inside `tm`'s `#[tokio::main]`, and
/// `Runtime::block_on` on a runtime worker panics; the runtime this builds
/// lives on its own OS thread, as `search_rpc::call_blocking` does (#5965).
fn health_blocking(socket: &Path, timeout: Duration) -> Result<(), String> {
    let socket = socket.to_path_buf();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("build a runtime for {METHOD_MPM_HEALTH}: {e}"))?;
        runtime.block_on(health_at(&socket, timeout))
    })
    .join()
    .map_err(|_| format!("the {METHOD_MPM_HEALTH} worker thread panicked"))?
}

/// Send `mpm.health` to `socket` and accept only a health envelope.
async fn health_at(socket: &Path, timeout: Duration) -> Result<(), String> {
    let request = json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": 1,
        "method": METHOD_MPM_HEALTH,
        "params": {},
    });
    let response: RpcResponse = send_framed_request(socket, &request, timeout)
        .await
        .map_err(|e| e.to_string())?;
    match (response.result, response.error) {
        (_, Some(e)) => Err(format!(
            "{METHOD_MPM_HEALTH} answered error {}: {}",
            e.code, e.message
        )),
        (Some(result), None) if result.get("status").is_some_and(Value::is_string) => Ok(()),
        _ => Err(format!(
            "{METHOD_MPM_HEALTH} answered without a health envelope"
        )),
    }
}

/// The mpm prober a unit test gets unless it injects one: no daemon, so no
/// test dials the operator's real trusty-mpm socket by accident.
#[cfg(test)]
pub(crate) struct NoMpmDaemon;

#[cfg(test)]
impl MpmSocketProber for NoMpmDaemon {
    fn socket(&self) -> PathBuf {
        PathBuf::from("/nonexistent/trusty-mpm/trusty-mpm.sock")
    }

    fn mpm_health(&self, _socket: &Path, _timeout: Duration) -> HealthState {
        HealthState::Fail {
            detail: "no trusty-mpm daemon in this test".into(),
        }
    }
}

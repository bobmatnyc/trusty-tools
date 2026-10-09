//! Socket-health discovery for trusty-search (#9543).
//!
//! Why: trusty-search serves its RPC socket and, from 0.59.0, binds no TCP
//! port (ADR-0032). `tm services` must find it the way the trusty-search
//! client does, by calling `search.health` on that socket, or it reports a live
//! daemon DOWN.
//! What: the [`SearchSocketProber`] seam, its production impl over
//! `trusty_common::search_rpc`, and the `Discoverer` methods that build a
//! socket-probed `ServiceStatus` and answer `port` / `url` for any service.
//! Test: `crate::services::uds_search_tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Discoverer, HEALTH_PROBE_TIMEOUT, HealthState, ServiceStatus};
use crate::daemon::search_rpc;
use crate::services::manifest::{HealthProbe, ServiceDecl};

/// Trait for trusty-search socket-health probing.
///
/// Why: lets unit tests replace the socket dial, as [`super::HttpProber`] does
/// for HTTP. The production impl is [`RealSearchSocketProber`].
/// What: `socket()` resolves the daemon's socket path; `search_health()` calls
/// `search.health` there within `timeout` and maps the outcome to a
/// [`HealthState`].
/// Test: `uds_search_is_up_against_a_socket_only_daemon`,
/// `uds_search_is_down_on_a_missing_stale_or_hung_socket`.
pub trait SearchSocketProber: Send + Sync {
    /// The socket the trusty-search daemon binds.
    fn socket(&self) -> anyhow::Result<PathBuf>;

    /// Call `search.health` on `socket`: `Ok` on an answer, `Fail` otherwise.
    fn search_health(&self, socket: &Path, timeout: Duration) -> HealthState;
}

/// Production prober over `search_rpc::search_socket` and `call_blocking`.
///
/// Why: the trusty-search client's resolver (`TRUSTY_SEARCH_SOCKET`, else the
/// daemon's data directory) is the one place the path is derived; a local copy
/// would drift. `call_blocking` drives its runtime on a joined OS thread, so
/// this is safe inside `tm`'s `#[tokio::main]` (#5965).
/// What: delegates both methods. A refusal, a dead or stale socket, or a
/// timeout is `Fail` carrying the error text and the socket path.
/// Test: `uds_search_is_up_against_a_socket_only_daemon`,
/// `uds_search_is_down_on_a_missing_stale_or_hung_socket`,
/// `real_socket_prober_survives_being_called_inside_a_tokio_runtime`.
pub struct RealSearchSocketProber;

impl SearchSocketProber for RealSearchSocketProber {
    fn socket(&self) -> anyhow::Result<PathBuf> {
        search_rpc::search_socket()
    }

    fn search_health(&self, socket: &Path, timeout: Duration) -> HealthState {
        match search_rpc::call_blocking(
            socket,
            search_rpc::METHOD_HEALTH,
            serde_json::json!({}),
            timeout,
        ) {
            Ok(_) => HealthState::Ok,
            Err(e) => HealthState::Fail {
                detail: format!("trusty-search unreachable at {}: {e:#}", socket.display()),
            },
        }
    }
}

/// The socket prober a unit test gets unless it injects one: no daemon, so no
/// test dials the operator's real trusty-search socket by accident.
#[cfg(test)]
pub(crate) struct NoSearchDaemon;

#[cfg(test)]
impl SearchSocketProber for NoSearchDaemon {
    fn socket(&self) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from(
            "/nonexistent/trusty-search/trusty-search.sock",
        ))
    }

    fn search_health(&self, _socket: &Path, _timeout: Duration) -> HealthState {
        HealthState::Fail {
            detail: "no trusty-search daemon in this test".into(),
        }
    }
}

impl Discoverer {
    /// Probe a `health_probe: uds_search` service.
    ///
    /// Why (#9543): UP must not depend on a TCP port, a port-owner PID or HTTP.
    /// What: health is `search.health` on the resolved socket; the PID is
    /// `pgrep -f` on `process_match`; `running` is true when the socket answers
    /// or a process matches; `port` and `url` are always `None`.
    /// Test: `uds_search_is_up_against_a_socket_only_daemon`,
    /// `uds_search_never_calls_the_http_prober`.
    pub(super) fn probe_uds_search(&self, name: &str, decl: &ServiceDecl) -> ServiceStatus {
        let health = match self.socket_prober.socket() {
            Ok(socket) => self
                .socket_prober
                .search_health(&socket, HEALTH_PROBE_TIMEOUT),
            Err(e) => HealthState::Fail {
                detail: format!("cannot resolve the trusty-search socket: {e:#}"),
            },
        };
        let pid = self.probe_process(decl);
        let running = pid.is_some() || health == HealthState::Ok;
        let version = if running {
            self.probe_version(decl)
        } else {
            None
        };
        ServiceStatus {
            name: name.to_string(),
            declared: true,
            running,
            pid,
            port: None,
            url: None,
            version,
            health,
            log_path: super::existing_log_path(decl),
            uptime_secs: pid.and_then(|p| self.probe_uptime(p)),
        }
    }

    /// The TCP port of service `name`, or the reason there is none.
    ///
    /// Why (#9543): a socket-only service has no port, and `tm services port`
    /// must say so and name the socket rather than print a port nothing binds.
    /// What: `None` for an undeclared service. A `uds_search` service is `Err`
    /// naming its socket, without probing; any other service is its probed port
    /// or `Err` when none was discovered.
    /// Test: `port_and_url_for_trusty_search_name_the_socket`.
    pub fn port(&mut self, name: &str) -> Option<Result<u16, String>> {
        let decl = self.manifest.services.get(name)?.clone();
        if decl.health_probe == HealthProbe::UdsSearch {
            return Some(Err(format!(
                "{name} has no TCP port ({})",
                self.socket_label()
            )));
        }
        let status = self.probe_or_cached(name, &decl);
        Some(
            status
                .port
                .ok_or_else(|| format!("{name}: port unavailable (service down or no port)")),
        )
    }

    /// The base URL of service `name`, or the reason there is none.
    ///
    /// Why/What: as [`Discoverer::port`], for `tm services url` (#9543).
    /// Test: `port_and_url_for_trusty_search_name_the_socket`.
    pub fn url(&mut self, name: &str) -> Option<Result<String, String>> {
        let decl = self.manifest.services.get(name)?.clone();
        if decl.health_probe == HealthProbe::UdsSearch {
            return Some(Err(format!(
                "{name} has no TCP URL ({})",
                self.socket_label()
            )));
        }
        let status = self.probe_or_cached(name, &decl);
        Some(
            status
                .url
                .ok_or_else(|| format!("{name}: URL unavailable (service down or no port)")),
        )
    }

    /// `socket: <path>`, or why the path could not be resolved.
    fn socket_label(&self) -> String {
        match self.socket_prober.socket() {
            Ok(socket) => format!("socket: {}", socket.display()),
            Err(e) => format!("socket unresolved: {e:#}"),
        }
    }
}

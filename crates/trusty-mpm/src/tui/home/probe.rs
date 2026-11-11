//! Health probes and the `console_metrics` fetch behind the home screen.
//!
//! Why (#10491): every tile's state comes from an interface the service
//! already serves — its socket health method, a socket connect, or the stdio
//! `console_metrics` MCP tool — bounded by a deadline so a wedged service
//! reads as `Timeout`, never as a hang. A service with no UDS endpoint is not
//! probed at all (#10899).
//! What: [`probe_service`] (one bounded probe), [`classify`] (the pure verdict
//! mapping), [`spawn_health_poller`] (one detached task per service, off the
//! UI loop) and [`fetch_metrics`] (one bounded stdio MCP call).
//! Test: `classify_maps_each_observation_to_a_tile_state`,
//! `a_probe_past_its_deadline_reads_as_timeout`,
//! `a_hung_probe_does_not_hold_back_the_other_tiles`.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use trusty_common::console_metrics::{CONSOLE_METRICS_METHOD, ConsoleMetricsReport, parse_report};
use trusty_common::stdio_mcp_client::StdioMcpClient;
use trusty_common::uds::probe::{SocketVerdict, probe_socket_verdict};
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::{UdsRpcError, UdsSecurityError};

use super::services::{ProbeKind, ServiceId, binary_on_path};
use super::state::{HealthUpdate, MetricsUpdate, TileHealth};

/// What one probe observed, before the binary-on-`PATH` check folds in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The service answered health; carries its version when it sent one.
    Healthy(Option<String>),
    /// The service answered, but not with health.
    Unhealthy(String),
    /// Nothing answered, or the probe failed (no discovery file, a refused
    /// socket, an I/O or decode failure, a hang-up, a timeout).
    Silent(String),
    /// The kernel proved nothing is listening: the socket file is absent, or
    /// the dial failed with not-found or connection-refused (#10476).
    NotListening(String),
    /// The service has no resident daemon to observe.
    OnDemand,
}

/// Fold an [`Observation`] and the binary check into a [`TileHealth`].
///
/// Why: "stopped" and "not installed" need different words on a tile, and
/// only a silent probe raises the question.
/// Test: `classify_maps_each_observation_to_a_tile_state`.
pub fn classify(observation: Observation, installed: bool) -> TileHealth {
    match observation {
        Observation::Healthy(version) => TileHealth::Up { version },
        Observation::Unhealthy(reason) => TileHealth::Degraded { reason },
        Observation::Silent(_) | Observation::NotListening(_) | Observation::OnDemand
            if !installed =>
        {
            TileHealth::Absent
        }
        Observation::Silent(reason) | Observation::NotListening(reason) => {
            TileHealth::Down { reason }
        }
        Observation::OnDemand => TileHealth::OnDemand,
    }
}

/// Fold the service's lifecycle into an [`Observation`]: for a service whose
/// [`ServiceId::not_listening_is_on_demand`] holds, `NotListening` becomes
/// `OnDemand`. Every other failure, `Silent` included, stays as observed.
///
/// Test: `an_idle_analyze_reads_as_on_demand_not_down`,
/// `an_answering_analyze_still_reads_as_up`,
/// `a_broken_analyze_socket_reads_as_down_not_on_demand`.
pub fn on_demand_when_idle(id: ServiceId, observation: Observation) -> Observation {
    match observation {
        // #10476: only "nothing is listening" is idle; a broken analyze is down
        Observation::NotListening(_) if id.not_listening_is_on_demand() => Observation::OnDemand,
        other => other,
    }
}

/// Run `observe` under `deadline`; an overrun is [`TileHealth::Timeout`].
///
/// Test: `a_probe_past_its_deadline_reads_as_timeout`.
pub async fn with_deadline<F>(observe: F, installed: bool, deadline: Duration) -> TileHealth
where
    F: Future<Output = Observation>,
{
    match tokio::time::timeout(deadline, observe).await {
        Ok(observation) => classify(observation, installed),
        Err(_) => TileHealth::Timeout,
    }
}

/// Probe one service once, bounded by `deadline`.
///
/// Why: the poller's one call per tick; it always yields a renderable state.
/// What: the binary check, then the service's [`ProbeKind`] folded through
/// [`on_demand_when_idle`], under [`with_deadline`].
/// Test: the pieces are covered by `classify_*`, the `*_analyze_*` tests and
/// the deadline test; the live dials are the console's own probes, exercised
/// by the smoke run.
pub async fn probe_service(id: ServiceId, deadline: Duration) -> TileHealth {
    if let Some(health) = no_uds_endpoint(id) {
        return health;
    }
    let installed = binary_on_path(id.binary());
    let observed = async move { on_demand_when_idle(id, observe(id, deadline).await) };
    with_deadline(observed, installed, deadline).await
}

/// The tile state of a service with no UDS endpoint, or `None` for a service
/// the TUI probes.
///
/// Why (#10899): the TUI reaches services over UDS only, so a service without
/// a socket is reported as such, not dialled over HTTP and not shown as down.
/// What: a pure mapping from [`ProbeKind::NoUdsEndpoint`]; it runs before the
/// `PATH` lookup, so these tiles do no file read, socket or spawn.
/// Test: `a_no_uds_endpoint_probe_does_no_io_and_is_not_down`.
pub fn no_uds_endpoint(id: ServiceId) -> Option<TileHealth> {
    match id.probe() {
        ProbeKind::NoUdsEndpoint(detail) => Some(TileHealth::NoUdsEndpoint { detail }),
        _ => None,
    }
}

/// Observe one service through its [`ProbeKind`].
async fn observe(id: ServiceId, deadline: Duration) -> Observation {
    match id.probe() {
        // See #10899: `probe_service` answers these before `observe` runs;
        // this arm keeps the match total without any I/O.
        ProbeKind::NoUdsEndpoint(detail) => Observation::Silent(detail.to_string()),
        ProbeKind::OnDemand => Observation::OnDemand,
        ProbeKind::UdsHealth(method) => match id.socket() {
            Ok(socket) => uds_health(&socket, method, deadline).await,
            Err(reason) => Observation::Silent(reason),
        },
        ProbeKind::SocketConnect => match id.socket() {
            Ok(socket) => match probe_socket_verdict(&socket, deadline).await {
                SocketVerdict::Serving => Observation::Healthy(None),
                _ => Observation::Silent(format!("nothing serves {}", socket.display())),
            },
            Err(reason) => Observation::Silent(reason),
        },
    }
}

/// One `<method>` frame with `"params": {}` over `socket` — the console's
/// `detect/socket_health.rs::probe_health`, ported (#1318 forbids the edge).
///
/// Test: `an_absent_or_refused_analyze_socket_reads_as_on_demand`,
/// `a_broken_analyze_socket_reads_as_down_not_on_demand`.
pub(super) async fn uds_health(socket: &Path, method: &str, deadline: Duration) -> Observation {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
    let sent =
        trusty_common::uds::send_framed_request::<_, RpcResponse>(socket, &request, deadline).await;
    let response = match sent {
        Ok(response) => response,
        Err(e) => return transport_failure(&e),
    };
    if let Some(error) = response.error {
        return Observation::Unhealthy(format!(
            "{method} answered an error (code {}): {}",
            error.code, error.message
        ));
    }
    health_version(response.result.as_ref())
}

/// Map a failed health exchange: [`Observation::NotListening`] when the kernel
/// proved nothing listens, [`Observation::Silent`] for every other failure.
///
/// Why (#10476): an on-demand service reads "not listening" as idle, so a
/// verification, I/O, hang-up, decode or timeout failure must not reach that
/// fold. The split keys off the error's variant and errno, never its message.
/// Test: `transport_failures_split_not_listening_from_broken`.
pub(super) fn transport_failure(err: &UdsRpcError) -> Observation {
    if is_not_listening(err) {
        Observation::NotListening(err.to_string())
    } else {
        Observation::Silent(err.to_string())
    }
}

/// Whether `err` is a dial that found no listener: the socket (or its
/// directory) absent, or `connect` refused / not-found, seen through the
/// bounded connect retry's wrapper.
fn is_not_listening(err: &UdsRpcError) -> bool {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    match err {
        UdsRpcError::Dial { source, .. } => match source {
            UdsSecurityError::StatForConnect { source, .. } => source.kind() == NotFound,
            UdsSecurityError::Connect { source, .. } => {
                matches!(source.kind(), NotFound | ConnectionRefused)
            }
            _ => false,
        },
        UdsRpcError::ConnectRetriesExhausted { source, .. } => is_not_listening(source),
        _ => false,
    }
}

/// Read a health envelope; a body without `status` is not health.
fn health_version(result: Option<&Value>) -> Observation {
    match result {
        Some(body) if body.get("status").is_some() => Observation::Healthy(
            body.get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        _ => Observation::Unhealthy("the answer is not a health envelope".to_string()),
    }
}

/// Spawn one detached poll task per service, each sending to `tx`.
///
/// Why (#10491): probing on the UI loop would freeze key handling behind a
/// slow service; one task per service also keeps a hung one from holding
/// back the others.
/// What: each task runs `probe(id)`, sends a [`HealthUpdate`], sleeps
/// `interval`, and exits once the receiver is dropped.
/// Test: `a_hung_probe_does_not_hold_back_the_other_tiles`.
pub fn spawn_health_poller<P, Fut>(
    ids: &[ServiceId],
    probe: P,
    tx: mpsc::Sender<HealthUpdate>,
    interval: Duration,
) where
    P: Fn(ServiceId) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = TileHealth> + Send + 'static,
{
    for &id in ids {
        let tx = tx.clone();
        let probe = probe.clone();
        tokio::spawn(async move {
            loop {
                let health = probe(id).await;
                if tx.send(HealthUpdate { id, health }).await.is_err() {
                    break; // the home screen has exited
                }
                tokio::time::sleep(interval).await;
            }
        });
    }
}

/// Call `console_metrics` once over the service's stdio MCP command.
///
/// Why (#10491): the first-level service view shows what trusty-console's
/// metrics poller shows, through the same tool and the same report contract.
/// What: spawn, `initialize`, `tools/call console_metrics`, [`parse_report`],
/// all under `deadline`. The child is killed when the client drops.
/// Test: `a_service_without_console_metrics_is_refused_without_a_spawn`.
pub async fn fetch_metrics(id: ServiceId, deadline: Duration) -> MetricsUpdate {
    let result = match id.metrics_command() {
        None => Err(format!("{} offers no console_metrics tool", id.id())),
        Some((binary, args)) => {
            match tokio::time::timeout(deadline, call_metrics(binary, args, deadline)).await {
                Ok(Ok(report)) => Ok(report),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err(format!("timed out after {}s", deadline.as_secs())),
            }
        }
    };
    MetricsUpdate { id, result }
}

/// The spawn → initialize → call → parse sequence.
async fn call_metrics(
    binary: &str,
    args: &[&str],
    deadline: Duration,
) -> anyhow::Result<ConsoleMetricsReport> {
    let mut client = StdioMcpClient::spawn(binary, args, "tm-tui").await?;
    client.initialize().await?;
    let raw = client
        .call_tool_with_timeout(CONSOLE_METRICS_METHOD, json!({}), deadline)
        .await?;
    parse_report(&raw)
}

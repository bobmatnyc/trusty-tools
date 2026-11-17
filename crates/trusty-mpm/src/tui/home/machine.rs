//! The MACHINE STATUS feed: `console.machine_status` and `console.history`
//! read over the console's own socket (#10942, #10943).
//!
//! Why: `tm tui` draws the console's machine cards and their sparklines over
//! UDS alone (#10899); the console serves both methods on its #10902 listener.
//! A console that is absent, slow, erroring or speaking an unknown schema must
//! show as such, never as stale or default numbers.
//! What: [`Feed`] is the last answer of one method — pending, cold, ready or a
//! labelled [`Fault`]; [`fetch_machine_status`] and [`fetch_history`] make one
//! bounded call each; [`spawn_poller`] runs a fetch on a detached task, off
//! the UI loop, as `probe::spawn_health_poller` does. Each poll result
//! replaces the previous one, so a later error clears earlier data.
//! Test: `machine_tests.rs` beside this file.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::mpsc;
use trusty_common::console_metrics::machine_status::{
    MACHINE_STATUS_SCHEMA_VERSION, MachineStatus,
};
use trusty_common::console_rpc::metrics::{
    ConsoleMachineStatus, HistorySnapshot, MACHINE_HISTORY_SCHEMA_VERSION,
};
use trusty_common::console_rpc::{METHOD_HISTORY, METHOD_MACHINE_STATUS};
use trusty_common::uds::UdsRpcError;
use trusty_common::uds::server::RpcResponse;

use super::probe::is_not_listening;
use crate::tui::theme::Tone;

/// How often `console.machine_status` is polled (the console samples at 1 Hz).
pub const STATUS_INTERVAL: Duration = Duration::from_secs(2);
/// How often `console.history` is polled; its 600-sample answer is the bigger one.
pub const HISTORY_INTERVAL: Duration = Duration::from_secs(5);
/// The deadline on one call, connect included.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a call produced no usable answer. Each kind draws its own label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Nothing listens on the console socket: the console is not running.
    Absent(String),
    /// The call did not finish inside its deadline.
    Timeout,
    /// The console answered an RPC error, or the transport broke mid-call.
    Error(String),
    /// The answer decoded to no shape this build knows: a bad body, an
    /// unknown variant, or a schema version this build does not read.
    Unknown(String),
}

impl Fault {
    /// The badge word and tone a fault shows as.
    ///
    /// Test: `every_fault_draws_its_own_label_and_no_values`.
    pub fn label(&self) -> (&'static str, Tone) {
        match self {
            Fault::Absent(_) => ("console absent", Tone::Muted),
            Fault::Timeout => ("timeout", Tone::Warn),
            Fault::Error(_) => ("error", Tone::Err),
            Fault::Unknown(_) => ("unknown", Tone::Degraded),
        }
    }

    /// One line of detail for the card's meta line.
    pub fn detail(&self) -> String {
        match self {
            Fault::Absent(d) | Fault::Error(d) | Fault::Unknown(d) => d.clone(),
            Fault::Timeout => format!("no answer within {}s", CALL_TIMEOUT.as_secs()),
        }
    }
}

/// The last answer of one console method.
#[derive(Debug, Clone, Default)]
pub enum Feed<T> {
    /// Not asked yet.
    #[default]
    Pending,
    /// The console answered but has no sample yet (`ready: false`).
    Cold,
    /// A decoded, schema-checked answer.
    Ready(T),
    /// The call failed; see [`Fault`].
    Failed(Fault),
}

impl<T> Feed<T> {
    /// The ready value, or `None` for every other state.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Feed::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// One poll result, sent from a poll task to the UI loop.
#[derive(Debug, Clone)]
pub enum MachineUpdate {
    /// A `console.machine_status` result.
    Status(Feed<MachineStatus>),
    /// A `console.history` result.
    History(Feed<HistorySnapshot>),
}

/// Both feeds, as the home state holds them.
#[derive(Debug, Clone, Default)]
pub struct MachineFeeds {
    /// The last `console.machine_status` result.
    pub status: Feed<MachineStatus>,
    /// The last `console.history` result.
    pub history: Feed<HistorySnapshot>,
}

impl MachineFeeds {
    /// Record one poll result; it replaces the previous one of its method.
    ///
    /// Test: `a_later_error_replaces_earlier_data`.
    pub fn apply(&mut self, update: MachineUpdate) {
        // #10942: replace, never merge, so a failure never leaves old numbers up.
        match update {
            MachineUpdate::Status(feed) => self.status = feed,
            MachineUpdate::History(feed) => self.history = feed,
        }
    }
}

/// One `console.machine_status` call over `socket`, bounded by `deadline`.
///
/// What: `ready: false` is [`Feed::Cold`]; a machine whose schema version is
/// not [`MACHINE_STATUS_SCHEMA_VERSION`] is [`Fault::Unknown`].
/// Test: `machine_status_over_uds_maps_each_answer`,
/// `a_console_fault_maps_to_its_own_kind`.
pub async fn fetch_machine_status(socket: &Path, deadline: Duration) -> Feed<MachineStatus> {
    let answer = match call::<ConsoleMachineStatus>(socket, METHOD_MACHINE_STATUS, deadline).await {
        Ok(answer) => answer,
        Err(fault) => return Feed::Failed(fault),
    };
    match answer.machine {
        _ if !answer.ready => Feed::Cold,
        None => Feed::Failed(Fault::Unknown(
            "ready, but the answer carries no machine".to_string(),
        )),
        // #10942: an unknown schema shows as unknown, never as read numbers.
        Some(machine) if machine.schema_version != MACHINE_STATUS_SCHEMA_VERSION => {
            Feed::Failed(Fault::Unknown(format!(
                "machine_status schema v{}, this build reads v{MACHINE_STATUS_SCHEMA_VERSION}",
                machine.schema_version
            )))
        }
        Some(machine) => Feed::Ready(machine),
    }
}

/// One `console.history` call over `socket`, bounded by `deadline`.
///
/// What: an empty snapshot is a complete answer (no samples yet), so it is
/// ready. A schema version outside `1..=`[`MACHINE_HISTORY_SCHEMA_VERSION`] is
/// [`Fault::Unknown`]: every field defaults on decode, so a foreign body would
/// otherwise read as an empty but valid history.
/// Test: `history_over_uds_maps_each_answer`.
pub async fn fetch_history(socket: &Path, deadline: Duration) -> Feed<HistorySnapshot> {
    match call::<HistorySnapshot>(socket, METHOD_HISTORY, deadline).await {
        Ok(h) if (1..=MACHINE_HISTORY_SCHEMA_VERSION).contains(&h.schema_version) => Feed::Ready(h),
        Ok(h) => Feed::Failed(Fault::Unknown(format!(
            "history schema v{}, this build reads up to v{MACHINE_HISTORY_SCHEMA_VERSION}",
            h.schema_version
        ))),
        Err(fault) => Feed::Failed(fault),
    }
}

/// One framed `{method, params: {}}` call, decoded as `T`.
async fn call<T: DeserializeOwned>(
    socket: &Path,
    method: &str,
    deadline: Duration,
) -> Result<T, Fault> {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
    let sent = tokio::time::timeout(
        deadline,
        trusty_common::uds::send_framed_request::<_, RpcResponse>(socket, &request, deadline),
    )
    .await;
    let response = match sent {
        Err(_) => return Err(Fault::Timeout),
        Ok(Err(e)) => return Err(transport_fault(&e)),
        Ok(Ok(response)) => response,
    };
    if let Some(error) = response.error {
        return Err(Fault::Error(format!(
            "{method}: code {}: {}",
            error.code, error.message
        )));
    }
    let body = response
        .result
        .ok_or_else(|| Fault::Unknown(format!("{method} answered no result")))?;
    serde_json::from_value(body)
        .map_err(|e| Fault::Unknown(format!("{method} answer did not decode: {e}")))
}

/// Map a failed exchange: no listener is [`Fault::Absent`], an expired
/// transport deadline is [`Fault::Timeout`], a body that is not a response
/// frame is [`Fault::Unknown`], and anything else is [`Fault::Error`].
fn transport_fault(err: &UdsRpcError) -> Fault {
    match err {
        e if is_not_listening(e) => Fault::Absent("nothing listens on the console socket".into()),
        UdsRpcError::Timeout { .. } => Fault::Timeout,
        UdsRpcError::Decode { .. } => Fault::Unknown(err.to_string()),
        e => Fault::Error(e.to_string()),
    }
}

/// The console socket, or the fault to show when it cannot be resolved.
///
/// Test: covered by the smoke run; the fetches take the path as an argument.
pub fn console_socket() -> Result<PathBuf, Fault> {
    super::services::ServiceId::Console
        .socket()
        .map_err(Fault::Error)
}

/// Spawn one detached task that runs `fetch`, sends its result to `tx`,
/// sleeps `interval`, and exits once the receiver is dropped.
///
/// Why: a call can take up to [`CALL_TIMEOUT`]; on the UI loop that would
/// freeze key handling. One task per method keeps a slow history from
/// delaying the status cards.
/// Test: `the_poller_runs_off_the_ui_loop_and_stops_with_the_screen`.
pub fn spawn_poller<F, Fut>(fetch: F, tx: mpsc::Sender<MachineUpdate>, interval: Duration)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = MachineUpdate> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            if tx.send(fetch().await).await.is_err() {
                break; // the home screen has exited
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// The production status fetch: resolve the socket, then one bounded call.
pub async fn poll_status() -> MachineUpdate {
    MachineUpdate::Status(match console_socket() {
        Ok(socket) => fetch_machine_status(&socket, CALL_TIMEOUT).await,
        Err(fault) => Feed::Failed(fault),
    })
}

/// The production history fetch: resolve the socket, then one bounded call.
pub async fn poll_history() -> MachineUpdate {
    MachineUpdate::History(match console_socket() {
        Ok(socket) => fetch_history(&socket, CALL_TIMEOUT).await,
        Err(fault) => Feed::Failed(fault),
    })
}

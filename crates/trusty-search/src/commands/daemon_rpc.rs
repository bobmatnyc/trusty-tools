//! Shared socket-call plumbing for CLI subcommands (#9214).
//!
//! Why: every subcommand moved off HTTP maps a [`DaemonCallError`] to the same
//! two operator messages and reads one index's status the same way. One copy
//! keeps the wording identical across subcommands.
//! What: [`rpc_error`], [`call`] and [`index_status`]; [`connect`] and
//! [`connect_for_indexing`], the guarded client every subcommand opens; and
//! [`registrations`] / [`resident_statuses`], the registry sweep the
//! CWD- and PATH-based target lookups share.
//! Test: `an_unreachable_socket_reads_as_could_not_reach`,
//! `a_refusal_reads_as_daemon_returned`, and `daemon_rpc_tests.rs`.

use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{json, Value};
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::rpc::reads::{METHOD_INDEXES_LIST, METHOD_INDEX_STATUS};

#[cfg(test)]
#[path = "daemon_rpc_tests.rs"]
mod sweep_tests;

/// The operator-facing error for a failed socket call.
///
/// What: nothing serving the socket reads "could not reach daemon: …"; every
/// other failure reads "daemon returned …". Both carry the client's own text,
/// which names the socket or the daemon's refusal.
pub(crate) fn rpc_error(e: DaemonCallError) -> anyhow::Error {
    if e.is_unreachable() {
        anyhow::anyhow!("could not reach daemon: {e}")
    } else {
        anyhow::anyhow!("daemon returned {e}")
    }
}

/// Call `method` and map a failure through [`rpc_error`].
///
/// # Errors
///
/// When the socket is unreachable, or the daemon refuses the call.
pub(crate) async fn call(
    client: &DaemonClient,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    client.call(method, params).await.map_err(rpc_error)
}

/// One index's status — the body `GET /indexes/{id}/status` answered.
///
/// # Errors
///
/// The client's own error, unmapped, so a caller can tell a refusal from an
/// unreachable daemon.
pub(crate) async fn index_status(
    client: &DaemonClient,
    id: &str,
) -> Result<Value, DaemonCallError> {
    client
        .call(METHOD_INDEX_STATUS, json!({ "index_id": id }))
        .await
}

/// The daemon's socket client, with the daemon started if nothing answers.
///
/// # Errors
///
/// When the socket cannot be resolved, or the daemon never comes up; the
/// error names the socket.
pub(crate) async fn connect() -> anyhow::Result<DaemonClient> {
    let client = DaemonClient::resolve()?;
    super::daemon_guard::ensure_daemon_up(&client).await?;
    Ok(client)
}

/// [`connect`], starting a missing daemon on the indexing device (issue #24).
///
/// # Errors
///
/// The same set as [`connect`].
pub(crate) async fn connect_for_indexing() -> anyhow::Result<DaemonClient> {
    let client = DaemonClient::resolve()?;
    let device = super::daemon_guard::indexing_device();
    super::daemon_guard::ensure_daemon_up_with_device(&client, device.as_deref()).await?;
    Ok(client)
}

/// Every registration `search.indexes.list` reports.
#[derive(Debug, Default)]
pub(crate) struct Registrations {
    /// Resident ids — the list's `indexes` array.
    pub(crate) resident: Vec<String>,
    /// Cold-parked `(id, root)` rows — the list's optional `parked` array (#8727).
    pub(crate) parked: Vec<(String, PathBuf)>,
}

/// Read the registry: resident ids plus parked rows.
///
/// Why (#9214): a list answer with no `indexes` array used to read as "nothing
/// registered", and `index remove <PATH>` then cleared PATH's local rows as
/// stale without the daemon ever confirming PATH was unregistered.
/// What: `search.indexes.list`; `indexes` must be an array of ids, `parked`
/// may be absent (the daemon omits it when nothing is parked).
/// Test: `a_list_without_an_indexes_array_is_an_error`.
///
/// # Errors
///
/// A failed call, or an answer whose `indexes` member is not an array.
pub(crate) async fn registrations(client: &DaemonClient) -> anyhow::Result<Registrations> {
    let body = call(client, METHOD_INDEXES_LIST, json!({})).await?;
    // #9214: no `indexes` array is a broken answer, never an empty registry.
    let resident = body
        .get("indexes")
        .and_then(Value::as_array)
        .with_context(|| format!("daemon's index list carries no `indexes` array: {body}"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let parked = body
        .get("parked")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_string();
            Some((id, PathBuf::from(row.get("root_path")?.as_str()?)))
        })
        .collect();
    Ok(Registrations { resident, parked })
}

/// One resident index's status, read for its root.
#[derive(Debug, Clone)]
pub(crate) struct ResidentStatus {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
    pub(crate) body: Value,
}

/// Every resident status that could be read, and every one that could not.
#[derive(Debug, Default)]
pub(crate) struct StatusSweep {
    pub(crate) read: Vec<ResidentStatus>,
    /// `(id, reason)` for each status that failed for a reason other than
    /// "no such index", or answered without a `root_path`.
    pub(crate) unreadable: Vec<(String, String)>,
}

impl StatusSweep {
    /// The read statuses, or an error naming every unreadable one.
    ///
    /// Why (#9214): an unread index could own the root a caller is matching,
    /// so a lookup that skips it can answer "not registered" or pick another
    /// index. `refusal` says what the caller refuses to do instead.
    ///
    /// # Errors
    ///
    /// When any status was unreadable.
    pub(crate) fn require_all(self, refusal: &str) -> anyhow::Result<Vec<ResidentStatus>> {
        if self.unreadable.is_empty() {
            return Ok(self.read);
        }
        let names: Vec<String> = self
            .unreadable
            .iter()
            .map(|(id, why)| format!("\"{id}\" ({why})"))
            .collect();
        anyhow::bail!(
            "could not read the status of index {}; {refusal}",
            names.join(", ")
        )
    }
}

/// Read the status of each id in `ids`.
///
/// Why (#9214, #8737): a lookup by root must read every registration before
/// deciding. A `not found` means the id was deleted since the list and is
/// skipped; every other failure — refusal, broken exchange, unreachable
/// socket, or a body with no `root_path` — is recorded as unreadable for the
/// caller to refuse on via [`StatusSweep::require_all`].
/// What: one `search.index.status` per id, in order.
/// Test: `a_failed_status_is_unreadable_and_a_missing_one_is_skipped`.
pub(crate) async fn resident_statuses(client: &DaemonClient, ids: Vec<String>) -> StatusSweep {
    let mut sweep = StatusSweep::default();
    for id in ids {
        match index_status(client, &id).await {
            Ok(body) => match body.get("root_path").and_then(Value::as_str) {
                Some(root) => sweep.read.push(ResidentStatus {
                    root: PathBuf::from(root),
                    id,
                    body,
                }),
                None => sweep
                    .unreadable
                    .push((id, "status carries no root_path".to_string())),
            },
            Err(e) if e.is_not_found() => continue,
            Err(e) => sweep.unreadable.push((id, e.to_string())),
        }
    }
    sweep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;
    use trusty_common::uds::server::RpcError;
    use trusty_search::service::rpc::error::CODE_NOT_FOUND;

    #[tokio::test]
    async fn an_unreachable_socket_reads_as_could_not_reach() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let err = call(&DaemonClient::at(&socket), "search.config.get", json!({}))
            .await
            .expect_err("nothing serves the socket")
            .to_string();
        assert!(err.starts_with("could not reach daemon"), "{err}");
        assert!(err.contains(&socket.display().to_string()), "{err}");
        assert!(!err.contains("http://"), "{err}");
    }

    #[tokio::test]
    async fn a_refusal_reads_as_daemon_returned() {
        let daemon =
            mock_daemon(|_, _| Err(RpcError::new(CODE_NOT_FOUND, "index not found"))).await;
        let err = call(&daemon.client, "search.index.status", json!({}))
            .await
            .expect_err("refused")
            .to_string();
        assert!(err.starts_with("daemon returned"), "{err}");
        assert!(err.contains("index not found"), "{err}");
    }
}

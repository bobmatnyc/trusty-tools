//! Shared socket-call plumbing for CLI subcommands (#9214).
//!
//! Why: every subcommand moved off HTTP maps a [`DaemonCallError`] to the same
//! two operator messages and reads one index's status the same way. One copy
//! keeps the wording identical across subcommands.
//! What: [`rpc_error`], [`call`] and [`index_status`].
//! Test: `an_unreachable_socket_reads_as_could_not_reach`,
//! `a_refusal_reads_as_daemon_returned`.

use serde_json::{json, Value};
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::rpc::reads::METHOD_INDEX_STATUS;

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

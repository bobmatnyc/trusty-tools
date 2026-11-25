//! `agents.health` on the assistant core's well-known socket (#11550).
//!
//! Why: `tm tui` probes each service over a Unix socket (ADR-0032); `ta`'s
//! (`tagent` before #11632) only sockets were per-project, so its tile could
//! not be probed. The socket keeps its `trusty-agents.sock` name: `tm tui`
//! dials it by that name.
//! What: [`router`] serves [`METHOD_AGENTS_HEALTH`]; [`serve_at`] binds the
//! socket with the singleton bind, serves it until shut down, and removes it.
//! #11650: each per-instance ask daemon calls [`serve_at`] on
//! `trusty_agents_common::health::agents_socket()`; the first one serves it
//! and the others find it served and leave it alone.
//! Test: `agents_health_answers_status_and_version`,
//! `agents_health_round_trips_over_a_real_socket`.

use std::path::Path;
use std::sync::Arc;

use trusty_agents_common::health::{AgentsHealth, METHOD_AGENTS_HEALTH};
use trusty_common::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};

/// The `agents.health` answer for this binary.
pub(crate) fn agents_health() -> AgentsHealth {
    AgentsHealth {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// The method table of the health socket. The method is pure liveness, so
/// answering it never counts as activity (#6621).
pub(crate) fn router() -> RpcRouter {
    RpcRouter::new().typed_liveness(
        METHOD_AGENTS_HEALTH,
        |_params: serde_json::Value| async move { Ok::<_, RpcError>(agents_health()) },
    )
}

/// Bind `socket` and serve [`router`] until `shutdown` resolves, then remove
/// the socket.
///
/// What: #11650 — the socket is unlinked before the listener drops, and only
/// while it still names the socket this call bound.
///
/// # Errors
///
/// The bind failed for a reason other than a live owner.
/// Test: `agents_health_round_trips_over_a_real_socket`.
pub(crate) async fn serve_at(
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    let listener = match trusty_common::uds::bind_singleton_hardened(socket).await {
        Ok(listener) => listener,
        Err(trusty_common::uds::UdsSecurityError::AlreadyServing { .. }) => {
            tracing::info!(socket = %socket.display(), "agents.health already served");
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("bind {}: {e}", socket.display())),
    };
    let owned = crate::ask::daemon::OwnedSocketPath::capture(socket)
        .map_err(|e| anyhow::anyhow!("stat {}: {e}", socket.display()))?;
    serve_until(
        &listener,
        Arc::new(router()),
        RpcServeOptions::default(),
        shutdown,
    )
    .await;
    owned.remove();
    drop(listener);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::time::Duration;

    #[test]
    fn agents_health_answers_status_and_version() {
        let got = agents_health();
        assert_eq!(got.status, "ok");
        assert_eq!(got.version, env!("CARGO_PKG_VERSION"));
        assert!(router().method_names().any(|m| m == METHOD_AGENTS_HEALTH));
        assert!(router().liveness_names().any(|m| m == METHOD_AGENTS_HEALTH));
    }

    #[tokio::test]
    async fn agents_health_round_trips_over_a_real_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sockets").join("trusty-agents.sock");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let served = socket.clone();
        let server = tokio::spawn(async move {
            serve_at(&served, async {
                let _ = stop_rx.await;
            })
            .await
        });
        let request =
            json!({"jsonrpc": "2.0", "id": 1, "method": METHOD_AGENTS_HEALTH, "params": {}});
        let mut answer: Option<Value> = None;
        for _ in 0..50 {
            match trusty_common::uds::send_framed_request::<_, Value>(
                &socket,
                &request,
                Duration::from_secs(2),
            )
            .await
            {
                Ok(v) => {
                    answer = Some(v);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let _ = stop_tx.send(());
        server.await.expect("join").expect("served");
        // #11650: the socket is gone once the server stops.
        assert!(!socket.exists(), "health socket left behind");
        let answer = answer.expect("agents.health answered");
        assert_eq!(answer["result"]["status"], "ok", "{answer}");
        assert_eq!(answer["result"]["version"], env!("CARGO_PKG_VERSION"));
    }
}

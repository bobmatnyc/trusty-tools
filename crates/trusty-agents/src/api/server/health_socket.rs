//! `agents.health` on tagent's well-known socket (#11550).
//!
//! Why: `tm tui` probes each service over a Unix socket (ADR-0032); tagent's
//! only sockets were per-project, so its tile could not be probed.
//! What: [`router`] serves [`METHOD_AGENTS_HEALTH`]; [`spawn`] binds
//! [`agents_socket`] with the singleton bind and serves it for the life of the
//! `--api` server process. A second `--api` process finds the socket served
//! and leaves it to the first.
//! Test: `agents_health_answers_status_and_version`,
//! `agents_health_round_trips_over_a_real_socket`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_agents_common::health::{AgentsHealth, METHOD_AGENTS_HEALTH, agents_socket};
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

/// Bind `socket` and serve [`router`] until `shutdown` resolves.
///
/// # Errors
///
/// The bind failed for a reason other than a live owner.
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
    serve_until(
        &listener,
        Arc::new(router()),
        RpcServeOptions::default(),
        shutdown,
    )
    .await;
    Ok(())
}

/// Serve the health socket in the background for this process's lifetime.
/// A resolve or bind failure is logged; it never stops the API server.
pub(crate) fn spawn() {
    let socket: PathBuf = match agents_socket() {
        Ok(socket) => socket,
        Err(e) => {
            tracing::warn!(error = %e, "agents.health socket unresolved; not served");
            return;
        }
    };
    tokio::spawn(async move {
        if let Err(e) = serve_at(&socket, std::future::pending()).await {
            tracing::warn!(error = %e, "agents.health socket not served");
        }
    });
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
        let answer = answer.expect("agents.health answered");
        assert_eq!(answer["result"]["status"], "ok", "{answer}");
        assert_eq!(answer["result"]["version"], env!("CARGO_PKG_VERSION"));
    }
}

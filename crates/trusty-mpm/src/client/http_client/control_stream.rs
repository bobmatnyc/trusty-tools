//! The one streamed daemon route a client reads: `mpm.control.connect`
//! (#6288 client cutover).
//!
//! Why: `tm sessctl connect` read the control session's events from the SSE
//! route `POST /api/v1/control/sessions/{id}/connect`. The daemon serves the
//! same events as a JSON-RPC frame stream on its socket, but
//! [`DaemonRequest::send`](super::DaemonRequest::send) reads exactly one
//! frame, so the stream needs its own entry point.
//! What: [`DaemonClient::connect_control_session`] dials the socket, asks for
//! the stream, and hands back the `FramedStream`. An HTTP client is refused —
//! there is no TCP form of this call any more.
//! Test: `connect_control_session_streams_frames`,
//! `connect_control_session_over_an_absent_socket_is_unreachable`.

use std::time::Duration;

use serde_json::Value;
use trusty_common::uds::FramedStream;
use trusty_common::uds::server::JSONRPC_VERSION;

use super::{DaemonCallError, DaemonClient};

/// The per-frame read bound on a control stream.
///
/// A control session may be quiet for a long time between events, and the
/// stream reader applies its timeout to each frame, so this bound is long; the
/// dial itself fails at once on a local socket.
const CONTROL_FRAME_TIMEOUT: Duration = Duration::from_secs(3600);

impl DaemonClient {
    /// Open the event stream of control session `id`.
    ///
    /// # Errors
    ///
    /// [`DaemonCallError::Unreachable`] when the socket is absent or refuses
    /// the dial; [`DaemonCallError::Failed`] for an HTTP client or a failed
    /// request write. A refusal the daemon sends (an unknown id) arrives as the
    /// stream's one error frame.
    pub async fn connect_control_session(
        &self,
        id: &str,
    ) -> Result<FramedStream<Value>, DaemonCallError> {
        let Some(socket) = self.socket_path() else {
            return Err(DaemonCallError::Failed {
                target: self.base_url().to_string(),
                source: anyhow::anyhow!(
                    "the control-session stream is served over the daemon socket only (#6288)"
                ),
            });
        };
        let frame = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": 1,
            "stream": true,
            "method": "mpm.control.connect",
            "params": {"id": id},
        });
        let target = socket.display().to_string();
        trusty_common::uds::send_framed_stream_request(socket, &frame, CONTROL_FRAME_TIMEOUT)
            .await
            .map_err(|e| {
                if e.is_dial_failure() {
                    DaemonCallError::Unreachable {
                        target,
                        source: e.into(),
                    }
                } else {
                    DaemonCallError::Failed {
                        target,
                        source: e.into(),
                    }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

    use super::DaemonClient;

    /// #6288: the stream method delivers every item frame, in order, to the end.
    #[tokio::test]
    async fn connect_control_session_streams_frames() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("s").join("trusty-mpm.sock");
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("dir");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let router =
            RpcRouter::new().typed_stream("mpm.control.connect", |params: Value| async move {
                let (tx, rx) = tokio::sync::mpsc::channel(4);
                let id = params["id"].as_str().unwrap_or_default().to_string();
                tokio::spawn(async move {
                    for n in 0..3 {
                        let _ = tx.send(Ok(json!({"id": id, "n": n}))).await;
                    }
                });
                Ok(rx)
            });
        let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_until(
                &listener,
                Arc::new(router),
                RpcServeOptions::default(),
                async {
                    let _ = shutdown.await;
                },
            )
            .await;
        });

        let mut stream = DaemonClient::over_socket(&socket)
            .connect_control_session("s-1")
            .await
            .expect("stream opens");
        let mut got = Vec::new();
        while let Some(frame) = stream.next_frame().await {
            got.push(frame.expect("an item"));
        }
        let _ = stop.send(());
        assert_eq!(
            got,
            (0..3)
                .map(|n| json!({"id": "s-1", "n": n}))
                .collect::<Vec<_>>()
        );
    }

    /// #6288: an absent socket is `Unreachable`, naming it; an HTTP client
    /// is refused without any dial.
    #[tokio::test]
    async fn connect_control_session_over_an_absent_socket_is_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let err = DaemonClient::over_socket(&socket)
            .connect_control_session("s-1")
            .await
            .expect_err("nothing listens");
        assert!(err.is_connect(), "{err}");
        assert!(err.to_string().contains("absent.sock"), "{err}");
        let http = DaemonClient::new("http://127.0.0.1:1")
            .connect_control_session("s-1")
            .await
            .expect_err("no TCP form");
        assert!(!http.is_connect());
        assert!(http.to_string().contains("#6288"), "{http}");
    }
}

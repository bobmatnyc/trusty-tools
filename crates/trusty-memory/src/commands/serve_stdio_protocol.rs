//! The protocol check in front of the MCP stdio bridge (#9288).
//!
//! Why: `serve --stdio` is this same binary. When `cargo install` replaces it
//! while the old daemon keeps running, the bridge and the daemon can speak
//! different protocols, and a forwarded call would be misparsed by one end.
//! What: [`protocol_refusal`] runs the shared check
//! (`trusty_common::memory_rpc::ensure_memory_protocol_at`) before a forward,
//! and answers a refused check with an error that names it. Notifications and
//! the methods [`super::serve_stdio_local`] answers in process never reach the
//! daemon, so they are never checked — the MCP handshake still survives a down
//! daemon (#8351). A daemon that predates the handshake is forwarded to, by
//! the rule `check_memory_protocol_at` documents.
//! Test: `a_tool_call_to_an_unsupported_daemon_is_refused_by_name`,
//! `local_and_notification_methods_are_never_checked`.

use std::path::Path;
use std::time::Duration;

use trusty_mcp::{error_codes, Request, Response};

/// Budget for one protocol check; a callable verdict is then reused briefly.
const PROTOCOL_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Check the daemon at `socket` before `req` is forwarded to it.
///
/// What: `None` lets the request through; `Some` is the answer to send
/// instead — an error carrying the request's id and the check's own message.
/// A failed check never reads as callable.
pub(crate) async fn protocol_refusal(req: &Request, socket: &Path) -> Option<Response> {
    let is_notification = req.id.is_none() || req.method.starts_with("notifications/");
    let is_local = super::serve_stdio_local::LOCAL_METHODS.contains(&req.method.as_str());
    if is_notification || is_local {
        return None;
    }
    match trusty_common::memory_rpc::ensure_memory_protocol_at(socket, PROTOCOL_CHECK_TIMEOUT).await
    {
        Ok(_) => None,
        Err(refused) => {
            // Stderr only: stdout is the JSON-RPC channel.
            eprintln!("trusty-memory: {refused}");
            Some(Response::err(
                req.id.clone(),
                error_codes::INTERNAL_ERROR,
                refused.to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use trusty_common::memory_rpc::{METHOD_PROTOCOL, SUPPORTED_MEMORY_PROTOCOLS};
    use trusty_common::uds::server::{serve_until, RpcRouter, RpcServeOptions};

    fn request(method: &str, id: Option<Value>) -> Request {
        Request {
            jsonrpc: Some("2.0".to_string()),
            id,
            method: method.to_string(),
            params: None,
        }
    }

    /// A daemon reporting a protocol past this build's range.
    fn serve_newer_daemon() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let newer = SUPPORTED_MEMORY_PROTOCOLS.end() + 1;
        let router = RpcRouter::new()
            .typed::<Value, Value, _, _>(METHOD_PROTOCOL, move |_| async move {
                Ok(json!({ "protocol_version": newer }))
            });
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("newer-memory.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_until(
                &listener,
                std::sync::Arc::new(router),
                RpcServeOptions::default(),
                async {
                    let _ = shutdown.await;
                },
            )
            .await;
        });
        (dir, socket, stop)
    }

    /// Why (#9288): an MCP client must see why its tool call failed, not a
    /// misparsed reply from a daemon of another release.
    /// Test: itself.
    #[tokio::test]
    async fn a_tool_call_to_an_unsupported_daemon_is_refused_by_name() {
        let (_dir, socket, _stop) = serve_newer_daemon();

        let refusal = protocol_refusal(&request("tools/call", Some(json!(7))), &socket)
            .await
            .expect("an unsupported daemon is refused");

        assert_eq!(refusal.id, Some(json!(7)));
        let error = refusal.error.expect("an error answer");
        assert!(
            error
                .message
                .starts_with("unsupported trusty-memory protocol"),
            "the refusal names itself: {}",
            error.message
        );
    }

    /// Why (#8351, #9288): the handshake and notifications never reach the
    /// daemon, so checking them would only make a down or newer daemon cost
    /// the client its session.
    /// Test: itself.
    #[tokio::test]
    async fn local_and_notification_methods_are_never_checked() {
        let (_dir, socket, _stop) = serve_newer_daemon();
        let unchecked = [
            request("initialize", Some(json!(1))),
            request("tools/list", Some(json!(2))),
            request("notifications/initialized", None),
            request("tools/call", None),
        ];
        for req in unchecked {
            assert!(
                protocol_refusal(&req, &socket).await.is_none(),
                "{} must not be checked",
                req.method
            );
        }
    }
}

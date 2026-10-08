//! A stand-in trusty-search daemon on a scratch Unix socket, for CLI tests
//! (#6285).
//!
//! Why: CLI subcommands now reach the daemon over its socket, so their tests
//! need a socket peer rather than a mock HTTP listener. The library carries a
//! twin in `service::daemon_client_tests`; a `#[cfg(test)]` item in the library
//! is not visible to this binary's tests, so the ~30 lines are repeated here
//! rather than shipped in the library.
//! What: [`mock_daemon`] binds `<tempdir>/ts.sock`, answers every method
//! through the test's closure, and hands back a `DaemonClient` pointed at it.
//! [`mock_daemon_streaming`] also serves one streaming method (#9214: the
//! reindex progress stream). Nothing here resolves or dials the live daemon's
//! socket.
//! Test: every CLI test that talks to a daemon.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use trusty_common::uds::server::{
    serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions, RpcStreamItems,
};
use trusty_search::service::daemon_client::DaemonClient;

type Handler = Arc<dyn Fn(&str, Value) -> Result<Value, RpcError> + Send + Sync>;

struct Fallback(Handler);

#[async_trait]
impl RpcFallback for Fallback {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        (self.0)(method, params)
    }
}

/// A mock daemon on a scratch socket. Dropping it stops the accept loop.
pub(crate) struct MockDaemon {
    pub(crate) client: DaemonClient,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// Bind a scratch socket and answer every call through `handler`.
pub(crate) async fn mock_daemon(
    handler: impl Fn(&str, Value) -> Result<Value, RpcError> + Send + Sync + 'static,
) -> MockDaemon {
    serve(RpcRouter::new().fallback(Fallback(Arc::new(handler)))).await
}

/// One streaming method's canned answer: the items it sends, in order, or a
/// refusal before the first item.
pub(crate) type StreamAnswer = Result<Vec<Result<Value, RpcError>>, RpcError>;

/// [`mock_daemon`], plus `stream_method` answered by `stream` per call. The
/// stream ends cleanly after its last item, or with that item's error.
pub(crate) async fn mock_daemon_streaming(
    handler: impl Fn(&str, Value) -> Result<Value, RpcError> + Send + Sync + 'static,
    stream_method: &str,
    stream: impl Fn(Value) -> StreamAnswer + Send + Sync + 'static,
) -> MockDaemon {
    let stream = Arc::new(stream);
    let router = RpcRouter::new()
        .typed_stream::<Value, _, _>(stream_method.to_string(), move |params| {
            let answer = stream(params);
            async move {
                let items = answer?;
                let (tx, rx) = tokio::sync::mpsc::channel(items.len().max(1));
                for item in items {
                    let _ = tx.send(item).await;
                }
                Ok::<RpcStreamItems, RpcError>(rx)
            }
        })
        .fallback(Fallback(Arc::new(handler)));
    serve(router).await
}

/// Bind a scratch socket and serve `router` on it.
async fn serve(router: RpcRouter) -> MockDaemon {
    let dir = tempfile::tempdir().expect("scratch socket dir");
    let socket = dir.path().join("ts.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the scratch socket");
    let router = Arc::new(router);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });
    MockDaemon {
        client: DaemonClient::at(socket),
        _dir: dir,
        _stop: stop,
    }
}

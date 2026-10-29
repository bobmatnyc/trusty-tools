//! Tests for the `mpm.architect.dashboard.*` socket methods (#10063), over a
//! real served socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use crate::core::paths::FrameworkPaths;
use crate::daemon::error::{CODE_CONFLICT, CODE_UNPROCESSABLE};
use crate::daemon::state::DaemonState;

/// A daemon serving only its socket, and a temp dashboard root.
struct Fixture {
    socket: PathBuf,
    root: PathBuf,
    _dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(dir.path()).expect("canonical");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(
        base.join("fw"),
    )));
    let socket = base.join("sock").join("trusty-mpm.sock");
    let bound = crate::daemon::socket::bind(&socket).await.expect("bind");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(crate::daemon::socket::serve_until_shutdown(
        bound,
        state,
        async {
            let _ = shutdown.await;
        },
    ));
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Fixture {
        socket,
        root: base.join("store"),
        _dir: dir,
        stop: Some(stop),
    }
}

async fn call(socket: &Path, method: &str, params: Value) -> RpcResponse {
    let frame = json!({"jsonrpc": JSONRPC_VERSION, "id": 1, "method": method, "params": params});
    send_framed_request(socket, &frame, Duration::from_secs(10))
        .await
        .expect("exchange")
}

/// Set, conflict, get, list, ids, tally and fsck answer over the socket;
/// a stale `if_version` is a conflict frame with exit 1 (owner ruling D34),
/// an unknown key a refusal with exit 2.
#[tokio::test]
async fn architect_dashboard_methods_round_trip_over_the_daemon_socket() {
    let f = fixture().await;
    let store = json!({"root": f.root, "format": "dir"});
    let set = json!({"store": store, "collection": "items", "id": "D01",
        "data": {"kind": "question", "status": "open", "title": "t"}, "if_version": 0});
    let ok = call(&f.socket, "mpm.architect.dashboard.set", set.clone()).await;
    assert_eq!(
        ok.result.as_ref().map(|r| r["version"].clone()),
        Some(json!(1)),
        "{ok:?}"
    );
    let stale = call(&f.socket, "mpm.architect.dashboard.set", set).await;
    let err = stale.error.expect("conflict");
    assert_eq!(err.code, CODE_CONFLICT, "{}", err.message);
    assert_eq!(err.data.as_ref().map(|d| d["exit"].clone()), Some(json!(1)));
    assert!(f.root.join("items").join("D01.json").exists());

    let get = call(
        &f.socket,
        "mpm.architect.dashboard.get",
        json!({"store": store, "collection": "items", "id": "D01"}),
    )
    .await;
    assert_eq!(get.result.expect("doc")["data"]["title"], "t");
    let list = call(
        &f.socket,
        "mpm.architect.dashboard.list",
        json!({"store": store, "collection": "items"}),
    )
    .await;
    assert_eq!(list.result.expect("docs")[0]["id"], "D01");
    let ids = call(
        &f.socket,
        "mpm.architect.dashboard.ids",
        json!({"store": store, "prefix": "D"}),
    )
    .await;
    assert_eq!(ids.result.expect("ids")["next"], "D02");
    let tally = call(
        &f.socket,
        "mpm.architect.dashboard.tally",
        json!({"store": store}),
    )
    .await;
    assert_eq!(tally.result.expect("tally")["questions"], 1);
    let fsck = call(
        &f.socket,
        "mpm.architect.dashboard.fsck",
        json!({"store": store}),
    )
    .await;
    assert_eq!(fsck.result.expect("fsck")["problems"], json!([]));

    let unknown = call(
        &f.socket,
        "mpm.architect.dashboard.fsck",
        json!({"store": store, "bogus": 1}),
    )
    .await;
    let err = unknown.error.expect("refused");
    assert_eq!(err.code, CODE_UNPROCESSABLE, "{}", err.message);
}

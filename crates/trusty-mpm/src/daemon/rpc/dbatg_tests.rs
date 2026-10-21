//! Tests for the `mpm.dbatg.*` socket methods, over a real served socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_architect::ledger::init_store;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use crate::core::paths::FrameworkPaths;
use crate::daemon::error::{CODE_CONFLICT, CODE_UNPROCESSABLE};
use crate::daemon::state::DaemonState;

/// A daemon serving only its socket, and a temp project with store `pm-a`.
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
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    init_store(
        &root.join("codes.json"),
        &root,
        "pm-a",
        "th",
        "2026-10-21T09:00Z",
        &|_| true,
    )
    .expect("init");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(
        &root.join("fw"),
    )));
    let socket = root.join("sock").join("trusty-mpm.sock");
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
        root,
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

fn add(root: &Path, request_id: &str, ops: Value) -> Value {
    json!({"store": {"project": root, "session": "pm-a"}, "request_id": request_id, "ops": ops})
}

/// `batch` writes and replays, `show` reads, and a refusal keeps its exit
/// class in the error frame; `init` refuses a bad code before the registry.
#[tokio::test(flavor = "multi_thread")]
async fn dbatg_methods_round_trip_over_the_daemon_socket() {
    let f = fixture().await;
    let goal = json!([{"op": "add", "kind": "G", "title": "lorem"}]);
    let first = call(
        &f.socket,
        "mpm.dbatg.batch",
        add(&f.root, "r1", goal.clone()),
    )
    .await;
    let first = first.result.expect("batch result");
    assert_eq!(first["events"][0]["id"], "th-G01");
    let again = call(&f.socket, "mpm.dbatg.batch", add(&f.root, "r1", goal)).await;
    assert_eq!(
        again.result,
        Some(first.clone()),
        "a retried request_id is applied once"
    );
    let show = call(
        &f.socket,
        "mpm.dbatg.show",
        json!({"store": {"project": f.root, "session": "pm-a"}, "brief": true}),
    )
    .await;
    assert_eq!(show.result.expect("show")["text"], "TOP th-G01 lorem");
    let bad = call(
        &f.socket,
        "mpm.dbatg.batch",
        add(
            &f.root,
            "r2",
            json!([{"op": "add", "kind": "T", "title": "t", "goal": "th-G09"}]),
        ),
    )
    .await;
    let err = bad.error.expect("refused");
    assert_eq!(
        (err.code, err.data.as_ref().map(|d| d["exit"].clone())),
        (CODE_UNPROCESSABLE, Some(json!(2)))
    );
    let mut stale = add(
        &f.root,
        "r3",
        json!([{"op": "add", "kind": "T", "title": "t", "goal": "th-G01"}]),
    );
    stale["if_v"] = json!(0);
    let conflict = call(&f.socket, "mpm.dbatg.batch", stale)
        .await
        .error
        .expect("conflict");
    assert_eq!(
        (conflict.code, conflict.data.map(|d| d["exit"].clone())),
        (CODE_CONFLICT, Some(json!(1)))
    );
    let init = call(
        &f.socket,
        "mpm.dbatg.init",
        json!({"project": f.root, "session": "pm-a", "code": "BAD"}),
    )
    .await;
    assert_eq!(init.error.map(|e| e.code), Some(CODE_UNPROCESSABLE));
}

/// The single-writer rule across the two write paths: the daemon method and
/// the in-process body (the CLI fallback) race on one store, and no add is
/// lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dbatg_daemon_and_in_process_writers_lose_no_update() {
    let f = fixture().await;
    let goal = json!([{"op": "add", "kind": "G", "title": "lorem"}]);
    call(&f.socket, "mpm.dbatg.batch", add(&f.root, "", goal))
        .await
        .result
        .expect("goal");
    let task = |w: &str, i: usize| json!([{"op": "add", "kind": "T", "title": format!("{w} {i}"), "goal": "th-G01"}]);
    let (socket, root) = (f.socket.clone(), f.root.clone());
    let daemon = tokio::spawn(async move {
        for i in 0..20 {
            let resp = call(
                &socket,
                "mpm.dbatg.batch",
                add(&root, "", task("daemon", i)),
            )
            .await;
            assert!(resp.result.is_some(), "{:?}", resp.error);
        }
    });
    let root = f.root.clone();
    let local = tokio::task::spawn_blocking(move || {
        for i in 0..20 {
            let req = serde_json::from_value(add(&root, "", task("local", i))).expect("request");
            crate::core::dbatg::batch(&req).expect("in-process write");
        }
    });
    daemon.await.expect("daemon writer");
    local.await.expect("local writer");
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(f.root.join(".trusty-mpm/dbatg/pm-a/state.json")).expect("state"),
    )
    .expect("json");
    assert_eq!(state["tasks"].as_array().map(Vec::len), Some(40));
    assert_eq!(state["v"], 41);
}

//! Tests for the `mpm.architect.dashboard.*` socket methods (#10063), over a
//! real served socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use crate::core::architect_store::{STORE_SUBDIR, user_config_path};
use crate::core::paths::FrameworkPaths;
use crate::daemon::error::{CODE_CONFLICT, CODE_UNAVAILABLE, CODE_UNPROCESSABLE};
use crate::daemon::state::DaemonState;

/// A daemon serving only its socket, and a temp dashboard root.
struct Fixture {
    socket: PathBuf,
    root: PathBuf,
    /// The daemon's home: the parent of its framework root (#10944).
    home: PathBuf,
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
        home: base.join("fw"),
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

/// Record an Architect at `<home>/<rel>` in `home`'s user config, with its
/// store directory; returns the store root.
fn record_architect(home: &Path, rel: &str) -> PathBuf {
    let dir = home.join(rel);
    std::fs::create_dir_all(dir.join(STORE_SUBDIR)).expect("store");
    std::fs::write(dir.join(".trusty-mpm.toml"), "profile = \"supervisor\"\n").expect("profile");
    let config = user_config_path(home);
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    let text = format!(
        "[architect]\nprojects = [{:?}]\n",
        dir.display().to_string()
    );
    std::fs::write(config, text).expect("config");
    dir.join(STORE_SUBDIR)
}

fn set_params(store: Option<Value>, id: &str, title: &str) -> Value {
    let mut p = json!({"collection": "items", "id": id,
        "data": {"kind": "question", "status": "open", "title": title}, "if_version": 0});
    if let Some(store) = store {
        p["store"] = store;
    }
    p
}

/// #10944: with no `store`, `store` without `root`, or no params at all, the
/// reads answer from the recorded Architect's store, as an explicit root does.
#[tokio::test]
async fn omitted_store_lists_and_tallies_the_resolved_store() {
    let f = fixture().await;
    let root = record_architect(&f.home, "arch");
    let explicit = json!({"root": root, "format": "dir"});
    let set = set_params(Some(explicit.clone()), "D01", "t");
    let ok = call(&f.socket, "mpm.architect.dashboard.set", set).await;
    assert!(ok.error.is_none(), "{ok:?}");

    let list = |params: Value| call(&f.socket, "mpm.architect.dashboard.list", params);
    let with_root = list(json!({"store": explicit, "collection": "items"})).await;
    let omitted = list(json!({"collection": "items"})).await;
    let no_root = list(json!({"store": {"format": "dir"}, "collection": "items"})).await;
    assert_eq!(omitted.result, with_root.result, "{omitted:?}");
    assert_eq!(no_root.result, with_root.result, "{no_root:?}");
    assert_eq!(omitted.result.expect("docs")[0]["id"], "D01");

    let tally = call(&f.socket, "mpm.architect.dashboard.tally", Value::Null).await;
    assert_eq!(tally.result.expect("tally")["questions"], 1);
    let get = call(
        &f.socket,
        "mpm.architect.dashboard.get",
        json!({"collection": "items", "id": "D01"}),
    )
    .await;
    assert_eq!(get.result.expect("doc")["data"]["title"], "t");
    let ids = call(
        &f.socket,
        "mpm.architect.dashboard.ids",
        json!({"prefix": "D"}),
    )
    .await;
    assert_eq!(ids.result.expect("ids")["next"], "D02");
}

/// An explicit `store.root` wins over the resolved one.
#[tokio::test]
async fn explicit_root_overrides_the_resolved_one() {
    let f = fixture().await;
    let resolved = record_architect(&f.home, "arch");
    let other = json!({"root": f.root});
    for (store, title) in [
        (json!({"root": resolved}), "resolved"),
        (other.clone(), "explicit"),
    ] {
        let ok = call(
            &f.socket,
            "mpm.architect.dashboard.set",
            set_params(Some(store), "D01", title),
        )
        .await;
        assert!(ok.error.is_none(), "{ok:?}");
    }
    let get = call(
        &f.socket,
        "mpm.architect.dashboard.get",
        json!({"store": other, "collection": "items", "id": "D01"}),
    )
    .await;
    assert_eq!(get.result.expect("doc")["data"]["title"], "explicit");
}

/// With nothing recorded and no default store, a call gets an exit-3 frame
/// naming the path, its source and the fix, never an empty list or tally.
#[tokio::test]
async fn unresolvable_root_returns_an_error_frame_with_exit_3() {
    let f = fixture().await;
    for (method, params) in [
        (
            "mpm.architect.dashboard.list",
            json!({"collection": "items"}),
        ),
        ("mpm.architect.dashboard.tally", json!({})),
    ] {
        let reply = call(&f.socket, method, params).await;
        assert!(reply.result.is_none(), "{method}: {reply:?}");
        let err = reply.error.expect("error frame");
        assert_eq!(err.code, CODE_UNAVAILABLE, "{}", err.message);
        assert_eq!(err.data.as_ref().map(|d| d["exit"].clone()), Some(json!(3)));
        let tried = f
            .home
            .join("trusty-mpm-projects/architect")
            .join(STORE_SUBDIR);
        assert!(
            err.message.contains(&tried.display().to_string()),
            "{}",
            err.message
        );
        assert!(err.message.contains("the default"), "{}", err.message);
        assert!(err.message.contains("tm fleet init"), "{}", err.message);
    }
}

/// `deny_unknown_fields` still refuses a bad key when the root was resolved.
#[tokio::test]
async fn omitted_store_with_unknown_key_is_still_refused_exit_2() {
    let f = fixture().await;
    record_architect(&f.home, "arch");
    let reply = call(
        &f.socket,
        "mpm.architect.dashboard.fsck",
        json!({"bogus": 1}),
    )
    .await;
    let err = reply.error.expect("refused");
    assert_eq!(err.code, CODE_UNPROCESSABLE, "{}", err.message);
    assert_eq!(err.data.as_ref().map(|d| d["exit"].clone()), Some(json!(2)));
    let bad_store = json!({"store": {"bogus": 1}});
    let reply = call(&f.socket, "mpm.architect.dashboard.fsck", bad_store).await;
    assert_eq!(reply.error.expect("refused").code, CODE_UNPROCESSABLE);
}

/// A write verb with no `store` writes and deletes in the resolved store.
#[tokio::test]
async fn omitted_store_write_goes_to_the_resolved_store() {
    let f = fixture().await;
    let root = record_architect(&f.home, "arch");
    let ok = call(
        &f.socket,
        "mpm.architect.dashboard.set",
        set_params(None, "D07", "w"),
    )
    .await;
    assert_eq!(
        ok.result.as_ref().map(|r| r["version"].clone()),
        Some(json!(1)),
        "{ok:?}"
    );
    let doc = root.join("items").join("D07.json");
    assert!(doc.exists(), "{}", doc.display());
    let del = call(
        &f.socket,
        "mpm.architect.dashboard.delete",
        json!({"collection": "items", "id": "D07", "if_version": 1}),
    )
    .await;
    assert!(del.error.is_none(), "{del:?}");
    assert!(!doc.exists(), "{}", doc.display());
}

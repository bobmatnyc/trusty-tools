//! `tm memory recall|remember|note` against a fake daemon on a socket (#8352).
//!
//! Why: the unit tests drive the library function; this target drives the
//! BINARY, which is what a PM with a dead MCP connection actually runs. Four
//! facts decide whether that fallback works, and none of them is observable
//! from inside the library: the process honours `TRUSTY_MEMORY_SOCKET`, it
//! carries the palace the session's environment injected, `--palace` outranks
//! that environment, and `--json` prints something an agent can parse.
//!
//! The fifth is the failure arm — nothing listening gives a prompt non-zero
//! exit naming the socket, never an empty success.
//!
//! What: a stub daemon built from the same `trusty_common::uds::server` pieces
//! the real one uses, recording every `(method, params)` it is sent, plus a
//! spawned `tm` confined by `common::tm_command`.
//! Test: this file IS the test module.

mod common;

use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::uds::server::{RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until};

/// Every `(method, params)` frame the stub was sent.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A stub daemon answering one canned body to every method.
struct Canned {
    body: Value,
    calls: Calls,
}

#[async_trait]
impl RpcFallback for Canned {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), params));
        }
        Ok(self.body.clone())
    }
}

/// A recall body with `hits` results, as trusty-memory's `serialize_recall` shapes it.
fn recall_body(palace: &str, hits: usize) -> Value {
    let results: Vec<Value> = (0..hits)
        .map(|i| {
            json!({
                "drawer_id": format!("d{i}"),
                "content": format!("remembered fact {i}"),
                "score": 0.9,
                "layer": "L2",
                "tags": [],
                "importance": 0.5,
                "drawer_type": "Insight",
            })
        })
        .collect();
    json!({ "palace": palace, "query": "q", "results": results, "dropped_below_floor": 0 })
}

/// Bind a stub at `socket`, serving until the returned guard is dropped.
async fn serve(socket: &Path, body: Value) -> (tokio::sync::oneshot::Sender<()>, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the stub socket");
    let router = Arc::new(RpcRouter::new().fallback(Canned {
        body,
        calls: Arc::clone(&calls),
    }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = rx.await;
        })
        .await;
    });
    (tx, calls)
}

/// Run `tm memory <args>` with `socket` and `palace` in the child's environment.
///
/// The palace arrives the way a managed session delivers it — as
/// `TRUSTY_MEMORY_PALACE` in the spawned process's environment, which is what
/// `core::mcp_session_env` exports and what trusty-memory's own MCP server
/// reads.
async fn run_tm(socket: &Path, palace: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = common::tm_command();
    cmd.arg("memory");
    cmd.args(args);
    cmd.env(trusty_common::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV, socket);
    match palace {
        Some(palace) => cmd.env("TRUSTY_MEMORY_PALACE", palace),
        None => cmd.env_remove("TRUSTY_MEMORY_PALACE"),
    };
    tokio::process::Command::from(cmd)
        .output()
        .await
        .expect("run tm")
}

/// The single call the stub recorded.
fn only_call(calls: &Calls) -> (String, Value) {
    let seen = calls.lock().expect("recorded calls");
    assert_eq!(seen.len(), 1, "expected exactly one RPC: {seen:?}");
    seen[0].clone()
}

/// Why (#8352 acceptance 1, 2): the whole point is a shell with no MCP
/// connection reaching the daemon over its socket, in the palace the session
/// pinned, with output an agent can parse.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn recall_uses_the_socket_and_the_environment_palace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, recall_body("session-palace", 2)).await;

    let out = run_tm(&socket, Some("session-palace"), &["recall", "q", "--json"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (method, params) = only_call(&calls);
    assert_eq!(method, "memory_recall");
    assert_eq!(
        params["palace"],
        json!("session-palace"),
        "the request must carry the palace the environment injected"
    );
    assert_eq!(params["query"], json!("q"));

    let envelope: Value =
        serde_json::from_slice(&out.stdout).expect("`--json` must print parseable JSON");
    assert_eq!(envelope["verb"], json!("recall"));
    assert_eq!(envelope["palace"], json!("session-palace"));
    assert_eq!(envelope["count"], json!(2));
    assert_eq!(envelope["socket"], json!(socket.display().to_string()));
    assert_eq!(
        envelope["result"]["results"].as_array().map(Vec::len),
        Some(2)
    );
}

/// Why (#8352 acceptance 2): `--palace` is the operator's explicit instruction,
/// so it has to beat the session variable rather than merely fill in for it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_palace_outranks_the_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, recall_body("other-palace", 0)).await;

    let out = run_tm(
        &socket,
        Some("session-palace"),
        &["recall", "q", "--palace", "other-palace", "--json"],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (_method, params) = only_call(&calls);
    assert_eq!(params["palace"], json!("other-palace"));

    let envelope: Value = serde_json::from_slice(&out.stdout).expect("parseable JSON");
    assert_eq!(envelope["palace"], json!("other-palace"));
    assert_eq!(
        envelope["count"],
        json!(0),
        "an empty recall is still a count"
    );
}

/// Why (#8352 acceptance 3): a write must leave the process as a WRITE RPC on
/// the socket. If it were ever served in-process it would be a second writer
/// against a palace the daemon holds under redb's exclusive lock (#1078).
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_verb_sends_a_write_tool_call_over_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(
        &socket,
        json!({ "palace": "session-palace", "status": "stored", "drawer_id": "d7" }),
    )
    .await;

    let out = run_tm(
        &socket,
        Some("session-palace"),
        &[
            "note",
            "deploy target is prod-east",
            "--tag",
            "ops",
            "--json",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (method, params) = only_call(&calls);
    assert_eq!(method, "memory_note");
    assert_eq!(params["palace"], json!("session-palace"));
    assert_eq!(params["content"], json!("deploy target is prod-east"));
    assert_eq!(params["tags"], json!(["ops"]));

    let envelope: Value = serde_json::from_slice(&out.stdout).expect("parseable JSON");
    assert_eq!(envelope["verb"], json!("note"));
    assert_eq!(envelope["count"], Value::Null);
    assert_eq!(envelope["result"]["drawer_id"], json!("d7"));
}

/// Why (#8352 acceptance 5): daemon down is the condition these verbs exist
/// for. It must exit non-zero, name the socket it dialled, print no JSON
/// envelope, and come back well inside the client's own budget — an empty
/// success here would be a PM told its palace holds nothing.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_daemon_exits_non_zero_naming_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let started = std::time::Instant::now();

    let out = run_tm(&socket, Some("session-palace"), &["recall", "q", "--json"]).await;

    assert!(!out.status.success(), "a dead daemon must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&socket.display().to_string()),
        "the error must name the socket: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "no envelope may be printed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the wait must be bounded: {:?}",
        started.elapsed()
    );
}

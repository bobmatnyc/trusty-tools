//! `ta handoff` against the built binary (#11650, slice 3c).
//!
//! Why: the production clients — `tm serve --stdio` over MCP and `task.run`
//! on the tcode socket — are only proven by running the binary. No test may
//! reach a real tm daemon or tcode, so tm is a fake `tm` script on `PATH`
//! that speaks MCP over stdio, and tc is a mock server on the socket path
//! `TRUSTY_DATA_DIR_OVERRIDE` resolves. Every run is under a temporary
//! `$HOME` with a short `/tmp` data directory.
//! Test: this file IS the test.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use trusty_common::uds::server::{RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until};

/// A fake `tm serve --stdio`: answers `initialize`, `session_new`,
/// `session_activity` and `session_send`, and logs every request line.
const FAKE_TM: &str = r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$FAKE_TM_LOG"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*) body='{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake-tm","version":"0"}}' ;;
    *'"name":"session_new"'*) body='{"content":[{"type":"text","text":"{\"id\":\"tm-fake-1\",\"state\":\"active\"}"}],"isError":false}' ;;
    *'"name":"session_activity"'*) body='{"content":[{"type":"text","text":"{\"id\":\"tm-fake-1\",\"state\":\"active\",\"runtime_active\":true}"}],"isError":false}' ;;
    *'"name":"session_send"'*) body='{"content":[{"type":"text","text":"{\"id\":\"tm-fake-1\",\"sent\":true,\"submit_state\":\"submitted\"}"}],"isError":false}' ;;
    *) body='{}' ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$body"
done
"#;

struct Env {
    home: TempDir,
    data: TempDir,
    bin: TempDir,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join("trusty-agents").join("e2e")).expect("instance");
        let data = tempfile::Builder::new()
            .prefix("ta-")
            .tempdir_in("/tmp")
            .expect("data");
        let bin = tempfile::tempdir().expect("bin");
        let tm = bin.path().join("tm");
        std::fs::write(&tm, FAKE_TM).expect("fake tm");
        std::fs::set_permissions(&tm, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { home, data, bin }
    }

    fn handoff(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        common::ta(&self.home)
            .env("TRUSTY_DATA_DIR_OVERRIDE", self.data.path())
            .env("PATH", path)
            .env("FAKE_TM_LOG", self.tm_log())
            .arg("handoff")
            .args(args)
            .output()
            .expect("run ta handoff")
    }

    fn tm_log(&self) -> PathBuf {
        self.bin.path().join("tm.log")
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(
            self.home
                .path()
                .join("trusty-agents/e2e/stores/handoffs.jsonl"),
        )
        .unwrap_or_default()
    }

    fn tc_socket(&self) -> PathBuf {
        self.data
            .path()
            .join("trusty-code")
            .join("trusty-code.sock")
    }
}

fn report(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("one JSON report")
}

/// Why: item 1 — no default target, and an unknown one is a clear error.
/// Test: itself.
#[test]
fn handoff_without_a_target_is_refused() {
    let env = Env::new();
    let out = env.handoff(&["e2e", "do", "it"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("there is no default target"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = env.handoff(&["e2e", "--to", "tx", "do", "it"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown handoff target `tx`"));
    assert!(env.ledger().is_empty());
}

/// Why: item 2 through the real MCP client — the first tm handoff creates
/// the session, the second sends into it; the task text arrives unchanged.
/// Test: itself.
#[test]
fn handoff_to_tm_creates_then_reuses_the_session() {
    let env = Env::new();
    let first = report(&env.handoff(&["e2e", "--to", "tm", "--", "plan the \"release\""]));
    assert_eq!(first["handle"], "tm-fake-1");
    assert_eq!(first["tm_session"], json!({"kind": "created"}));
    let second = report(&env.handoff(&["e2e", "--to", "tm", "then ship"]));
    assert_eq!(second["handle"], "tm-fake-1");
    assert_eq!(second["tm_session"], json!({"kind": "reused"}));

    let log = std::fs::read_to_string(env.tm_log()).expect("tm log");
    let calls: Vec<Value> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["method"] == "tools/call")
        .map(|v| v["params"].clone())
        .collect();
    assert_eq!(calls.len(), 3, "{log}");
    assert_eq!(calls[0]["name"], "session_new");
    assert_eq!(calls[0]["arguments"]["task"], "plan the \"release\"");
    assert_eq!(calls[0]["arguments"]["ephemeral"], false);
    assert_eq!(calls[1]["name"], "session_activity");
    assert_eq!(calls[2]["name"], "session_send");
    assert_eq!(calls[2]["arguments"]["text"], "then ship");
    assert_eq!(env.ledger().lines().count(), 4);
}

/// Records every `task.run` and answers a fixed session id.
struct MockTcode(Arc<Mutex<Vec<Value>>>);

#[async_trait]
impl RpcFallback for MockTcode {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if method != "task.run" {
            return Err(RpcError::new(-32601, "method not found"));
        }
        self.0.lock().expect("seen").push(params);
        Ok(json!({"session_id": "tc-sess-7", "status": "running"}))
    }
}

/// Why: items 3 and 4 through the real tc client — `task.run`'s session id
/// is printed, and a retry with the same key dispatches nothing.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_to_tc_prints_the_handle_once_per_key() {
    let env = Env::new();
    let socket = env.tc_socket();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind mock tcode");
    let router = Arc::new(RpcRouter::new().fallback(MockTcode(Arc::clone(&seen))));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });

    let run = |env: &Env| env.handoff(&["e2e", "--to", "tc", "--key", "k1", "build", "it"]);
    let first = tokio::task::block_in_place(|| report(&run(&env)));
    assert_eq!(
        (&first["handle"], &first["replayed"]),
        (&json!("tc-sess-7"), &json!(false))
    );
    let again = tokio::task::block_in_place(|| report(&run(&env)));
    assert_eq!(again["replayed"], true);
    assert_eq!(
        *seen.lock().expect("seen"),
        vec![json!({"task_description": "build it"})]
    );
    let _ = stop.send(());
    server.await.expect("server");
}

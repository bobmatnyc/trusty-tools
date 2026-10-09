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

use crate::common;

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

/// Why (#9142): a refused slot still stores the drawer (`tier: "E"`), so a
/// silent `stored` and exit 0 would hide that the fact supersedes nothing. An
/// admitted slot (`tier: "C"`) stays a plain success.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_slot_warns_and_exits_with_the_refusal_code() {
    for (tier, refused, want_code) in [
        ("E", Some("fact_key \"bad\" has no `<domain>:` prefix"), 3),
        ("C", None, 0),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("memory.sock");
        let mut body =
            json!({ "palace": "p", "status": "stored", "drawer_id": "d9", "tier": tier });
        if let Some(reason) = refused {
            body["tier_c_refused"] = json!(reason);
        }
        let (_stop, _calls) = serve(&socket, body).await;

        let out = run_tm(
            &socket,
            Some("p"),
            &[
                "remember",
                "session s1 resumes at review",
                "--fact-key",
                "bad",
            ],
        )
        .await;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(want_code), "tier {tier}: {stdout}");
        assert!(stdout.contains("d9"), "the drawer id is printed: {stdout}");
        match refused {
            Some(reason) => assert!(
                stdout.contains(&format!("WARNING: slot refused: {reason}"))
                    && stdout.contains("stored unslotted"),
                "{stdout}"
            ),
            None => assert!(!stdout.contains("WARNING"), "{stdout}"),
        }
    }
}

/// A drawer id in the shape trusty-memory mints (#9340).
const DRAWER: &str = "0b6f3c1e-8a52-4c1d-9e0f-2a7b5c4d3e21";

/// Why (#9340): `forget` must leave as `memory_forget` on the same socket, in
/// the same palace, as the other verbs — and a `deleted` answer exits 0.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_sends_memory_forget_over_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(
        &socket,
        json!({ "palace": "session-palace", "status": "deleted", "drawer_id": DRAWER }),
    )
    .await;

    let out = run_tm(&socket, Some("session-palace"), &["forget", DRAWER]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (method, params) = only_call(&calls);
    assert_eq!(method, "memory_forget");
    assert_eq!(
        params,
        json!({ "palace": "session-palace", "drawer_id": DRAWER })
    );
    assert!(
        stdout.contains("deleted") && stdout.contains(DRAWER),
        "{stdout}"
    );
}

/// Why (#9340): `memory_forget` answers an unknown id with a SUCCESSFUL body,
/// `status: "not_found"`. The CLI must exit non-zero and say nothing was
/// deleted; under `--json` it still prints the daemon's envelope.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_drawer_exits_non_zero_and_says_nothing_was_deleted() {
    for json_flag in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("memory.sock");
        let (_stop, calls) = serve(
            &socket,
            json!({ "palace": "p", "status": "not_found", "drawer_id": DRAWER }),
        )
        .await;

        let mut args = vec!["forget", DRAWER];
        if json_flag {
            args.push("--json");
        }
        let out = run_tm(&socket, Some("p"), &args).await;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "json={json_flag}: an unknown id must not exit 0"
        );
        assert!(
            stderr.contains(&format!("drawer {DRAWER} not found in palace p"))
                && stderr.contains("nothing was deleted"),
            "json={json_flag}: {stderr}"
        );
        assert_eq!(only_call(&calls).0, "memory_forget");
        if json_flag {
            let envelope: Value = serde_json::from_slice(&out.stdout).expect("parseable JSON");
            assert_eq!(envelope["verb"], json!("forget"));
            assert_eq!(envelope["result"]["status"], json!("not_found"));
        } else {
            assert!(
                out.stdout.is_empty(),
                "no success line may be printed: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
}

/// Why (#9340): with the daemon down, `forget` must fail and name the socket
/// and the method — never print a deletion.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_with_a_dead_daemon_exits_non_zero_naming_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");

    let out = run_tm(&socket, Some("session-palace"), &["forget", DRAWER]).await;

    assert!(!out.status.success(), "a dead daemon must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&socket.display().to_string()) && stderr.contains("memory_forget"),
        "the error must name the socket and the method: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may be printed as deleted: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A stub daemon answering each method with its own canned body (#9340).
///
/// Why: `forget --fact-key` makes two calls — `memory_list`, then
/// `memory_forget` — so one body for every method cannot drive it. A method
/// with no body is refused, which a test sees as a non-zero exit.
struct ByMethod {
    bodies: Vec<(&'static str, Value)>,
    calls: Calls,
}

#[async_trait]
impl RpcFallback for ByMethod {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), params));
        }
        self.bodies
            .iter()
            .find(|(m, _)| *m == method)
            .map(|(_, body)| body.clone())
            .ok_or_else(|| RpcError::new(-32601, format!("stub has no body for {method}")))
    }
}

/// Bind a [`ByMethod`] stub at `socket`, serving until the guard is dropped.
async fn serve_by_method(
    socket: &Path,
    bodies: Vec<(&'static str, Value)>,
) -> (tokio::sync::oneshot::Sender<()>, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the stub socket");
    let router = Arc::new(RpcRouter::new().fallback(ByMethod {
        bodies,
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

/// The `limit` `forget --fact-key` must ask `memory_list` for.
///
/// Mirrors `trusty_mpm::core::memory_forget::FACT_KEY_LIST_LIMIT`; spelled out
/// here so the red-first run compiles against the pre-#9340 library.
const LIST_LIMIT: usize = 100_000;

/// The slot key the fact-key tests forget.
const KEY: &str = "pr:9340/state";
/// A second drawer id, for the ambiguous and expired arms.
const OTHER: &str = "6d2e1f0a-3b4c-4d5e-8f60-718293a4b5c6";
/// An `expires_at` that never passes inside a test run.
const FUTURE: &str = "2999-01-01T00:00:00+00:00";
/// An `expires_at` long gone.
const PAST: &str = "2000-01-01T00:00:00+00:00";

/// One listed drawer, as `handle_memory_list` shapes it after PR1 of #9340.
fn listed(id: &str, fact_key: Option<&str>, expires_at: Option<&str>) -> Value {
    json!({
        "drawer_id": id,
        "content": format!("drawer {id}"),
        "importance": 0.5,
        "tags": [],
        "created_at": "2026-10-09T00:00:00+00:00",
        "drawer_type": "Insight",
        "expires_at": expires_at,
        "fact_key": fact_key,
    })
}

/// A `memory_list` body for palace `p`.
fn list_body(drawers: Vec<Value>) -> Value {
    json!({ "palace": "p", "drawers": drawers })
}

/// The methods the stub was sent, in order.
fn methods(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .expect("recorded calls")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

/// Run `tm memory forget --fact-key KEY` against a stub listing `drawers`.
async fn forget_by_key(drawers: Vec<Value>) -> (Output, Calls) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve_by_method(
        &socket,
        vec![
            ("memory_list", list_body(drawers)),
            (
                "memory_forget",
                json!({ "palace": "p", "status": "deleted", "drawer_id": DRAWER }),
            ),
        ],
    )
    .await;
    let out = run_tm(&socket, Some("p"), &["forget", "--fact-key", KEY]).await;
    (out, calls)
}

/// Assert a fact-key run failed closed: non-zero, KEY named, only a listing sent.
fn assert_refused(out: &Output, calls: &Calls) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must not exit 0: {stderr}");
    assert!(
        stderr.contains(KEY),
        "the error must name the key: {stderr}"
    );
    assert!(
        stderr.contains("nothing was deleted"),
        "the error must say nothing was deleted: {stderr}"
    );
    assert_eq!(
        methods(calls),
        vec!["memory_list".to_string()],
        "a refused fact-key forget sends the listing and no memory_forget"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may be printed as deleted: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// Why (#9340 arm 1): `--fact-key` resolves the slot's one live occupant from
/// the whole-palace listing and forgets it through the id form's own
/// `memory_forget` call, in the same palace.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_forgets_the_one_listed_occupant() {
    let (out, calls) = forget_by_key(vec![
        listed(OTHER, None, None),
        listed(DRAWER, Some(KEY), Some(FUTURE)),
        listed(
            "1a2b3c4d-0000-4000-8000-000000000001",
            Some("pr:1/state"),
            None,
        ),
    ])
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = calls.lock().expect("recorded calls").clone();
    assert_eq!(seen.len(), 2, "one listing, one forget: {seen:?}");
    assert_eq!(seen[0].0, "memory_list");
    assert_eq!(
        seen[0].1,
        json!({ "palace": "p", "limit": LIST_LIMIT, "full": true }),
        "the listing must ask for the whole palace, unfolded"
    );
    assert_eq!(seen[1].0, "memory_forget");
    assert_eq!(seen[1].1, json!({ "palace": "p", "drawer_id": DRAWER }));
    assert!(
        stdout.contains("deleted") && stdout.contains(DRAWER),
        "{stdout}"
    );
}

/// Why (#9340 arm 2): a key no drawer holds must fail closed and name the key.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_with_no_match_fails_closed() {
    let (out, calls) = forget_by_key(vec![
        listed(DRAWER, None, None),
        listed(OTHER, Some("pr:1/state"), Some(FUTURE)),
    ])
    .await;
    let stderr = assert_refused(&out, &calls);
    assert!(stderr.contains("no live drawer"), "{stderr}");
}

/// Why (#9340 arm 3): two live drawers on one key is a palace this cannot
/// resolve; it must list every candidate and forget neither.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_with_two_matches_names_every_candidate() {
    let (out, calls) = forget_by_key(vec![
        listed(DRAWER, Some(KEY), Some(FUTURE)),
        listed(OTHER, Some(KEY), None),
    ])
    .await;
    let stderr = assert_refused(&out, &calls);
    assert!(
        stderr.contains(DRAWER) && stderr.contains(OTHER),
        "every candidate id must be named: {stderr}"
    );
}

/// Why (#9340 arm 4): `memory_list` has no cursor, so a page as long as the
/// requested `limit` cannot prove the palace was seen whole, and a body the
/// daemon's byte ceiling folded (`truncated: true`) dropped drawers from its
/// tail. The occupant on either page must NOT be forgotten.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_on_an_incomplete_listing_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let mut folded = list_body(vec![listed(DRAWER, Some(KEY), Some(FUTURE))]);
    folded["truncated"] = json!(true);
    folded["withheld"] = json!(7);
    let (_stop, calls) = serve_by_method(&socket, vec![("memory_list", folded)]).await;
    let out = run_tm(&socket, Some("p"), &["forget", "--fact-key", KEY]).await;
    let stderr = assert_refused(&out, &calls);
    assert!(stderr.contains("incomplete"), "{stderr}");

    // Slim entries keep the page well inside the client's 32 MiB frame cap.
    let mut drawers: Vec<Value> = (1..LIST_LIMIT)
        .map(|i| {
            json!({
                "drawer_id": format!("00000000-0000-4000-8000-{i:012}"),
                "expires_at": null,
                "fact_key": null,
            })
        })
        .collect();
    drawers.push(listed(DRAWER, Some(KEY), Some(FUTURE)));
    let (out, calls) = forget_by_key(drawers).await;
    let stderr = assert_refused(&out, &calls);
    assert!(
        stderr.contains("incomplete") && stderr.contains(&LIST_LIMIT.to_string()),
        "{stderr}"
    );
}

/// Why (#9340 arm 5): a drawer past its `expires_at` is no live occupant. The
/// daemon does not sweep an expired Tier C drawer, so it stays listed with its
/// key. It is skipped beside a live one; an only-expired match is zero matches
/// and the error names its id, for the id form.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_skips_an_expired_occupant() {
    let (out, calls) = forget_by_key(vec![
        listed(OTHER, Some(KEY), Some(PAST)),
        listed(DRAWER, Some(KEY), Some(FUTURE)),
    ])
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = calls.lock().expect("recorded calls").clone();
    assert_eq!(
        seen.last()
            .map(|(m, p)| (m.as_str(), p["drawer_id"].clone())),
        Some(("memory_forget", json!(DRAWER))),
        "the live occupant is the one forgotten: {seen:?}"
    );

    let (out, calls) = forget_by_key(vec![listed(OTHER, Some(KEY), Some(PAST))]).await;
    let stderr = assert_refused(&out, &calls);
    assert!(
        stderr.contains("no live drawer") && stderr.contains("expired") && stderr.contains(OTHER),
        "{stderr}"
    );
}

/// Why (#9340 arm 6): a daemon that predates the listed `fact_key` reports no
/// such field at all. Read as zero matches, a typo and an old daemon would
/// look the same; it must say the daemon is too old.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn forget_by_fact_key_against_a_pre_fact_key_daemon_fails_closed() {
    let mut old = listed(DRAWER, None, None);
    if let Some(fields) = old.as_object_mut() {
        fields.remove("fact_key");
    }
    let (out, calls) = forget_by_key(vec![old]).await;
    let stderr = assert_refused(&out, &calls);
    assert!(stderr.contains("too old"), "{stderr}");
}

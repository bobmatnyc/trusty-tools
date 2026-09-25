//! Coverage for the no-MCP palace verbs (#8352).
//!
//! Why: these verbs are what a PM falls back to when its memory MCP connection
//! is dead, so the three facts that decide whether the fallback works have to be
//! pinned: the request names the palace the session resolved, a write leaves as
//! a WRITE method over the socket (never a local store open, #1078), and a dead
//! socket is an error naming that socket rather than an empty success.
//! What: drives [`super::run_verb`] against the shared stub JSON-RPC daemon,
//! recording every method and params it is sent.
//! Test: this file.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{MemoryVerb, MemoryVerbError, MemoryVerbOptions};

/// Every `(method, params)` a stub daemon was sent.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A stub answering `body` to everything, recording what it was asked.
async fn recording_daemon(body: Value) -> (crate::uds_mock::MockUdsDaemon, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let daemon = crate::uds_mock::spawn(move |method: &str, params: Value| {
        let body = body.clone();
        if let Ok(mut seen) = seen.lock() {
            seen.push((method.to_string(), params));
        }
        Box::pin(async move { Ok(body) })
    })
    .await;
    (daemon, calls)
}

/// The one call the stub recorded.
fn only_call(calls: &Calls) -> (String, Value) {
    let seen = calls.lock().expect("recorded calls");
    assert_eq!(seen.len(), 1, "expected exactly one RPC: {seen:?}");
    seen[0].clone()
}

/// A recall of `query` with no optional arguments.
fn recall(query: &str) -> MemoryVerb {
    MemoryVerb::Recall {
        query: query.to_string(),
        top_k: None,
        room: None,
        wing: None,
        min_score: None,
    }
}

/// Options naming `palace` explicitly, rooted at a throwaway directory.
fn opts_at(
    palace: Option<&str>,
    socket: &std::path::Path,
    cwd: &std::path::Path,
) -> MemoryVerbOptions {
    MemoryVerbOptions {
        palace: palace.map(str::to_string),
        socket: Some(socket.to_path_buf()),
        cwd: Some(cwd.to_path_buf()),
    }
}

/// Why: an omitted flag must leave the daemon's own default in force. Pinning
/// `top_k: 10` from here would make a later change to trusty-memory's default
/// invisible to this CLI, and sending `"room": null` would be a key the schema
/// does not describe.
/// Test: itself.
#[test]
fn arguments_carry_only_the_schema_keys_supplied() {
    let bare = recall("who owns the ledger").arguments();
    assert_eq!(bare.keys().collect::<Vec<_>>(), vec!["query"]);

    let full = MemoryVerb::Recall {
        query: "q".to_string(),
        top_k: Some(3),
        room: Some("Planning".to_string()),
        wing: None,
        min_score: Some(0.4),
    }
    .arguments();
    assert_eq!(full["top_k"], json!(3));
    assert_eq!(full["room"], json!("Planning"));
    assert_eq!(full["min_score"], json!(0.4));
    assert!(
        !full.contains_key("wing"),
        "an omitted wing is absent: {full:?}"
    );

    let note = MemoryVerb::Note {
        content: "prefers snake_case".to_string(),
        room: None,
        tags: vec!["style".to_string()],
    }
    .arguments();
    assert_eq!(note["content"], json!("prefers snake_case"));
    assert_eq!(note["tags"], json!(["style"]));
}

/// Why (#8352): the request must name the palace the CLI resolved, or a recall
/// silently searches whatever the daemon defaults to.
/// Test: itself.
#[tokio::test]
async fn recall_sends_the_resolved_palace() {
    let (daemon, calls) = recording_daemon(json!({
        "palace": "chosen-palace",
        "query": "q",
        "results": [{ "drawer_id": "d1", "content": "hit", "score": 0.9 }],
    }))
    .await;
    let cwd = tempfile::tempdir().expect("tempdir");
    let outcome = super::run_verb(
        &recall("q"),
        &opts_at(Some("chosen-palace"), daemon.socket(), cwd.path()),
    )
    .await
    .expect("the stub answers");

    let (method, params) = only_call(&calls);
    assert_eq!(method, super::RECALL_METHOD);
    assert_eq!(params["palace"], json!("chosen-palace"));
    assert_eq!(params["query"], json!("q"));
    assert_eq!(outcome.count, Some(1));
    assert_eq!(outcome.palace.as_deref(), Some("chosen-palace"));
}

/// Why (#1078): a write must leave this process as a WRITE RPC over the socket.
/// The hazard this verb group could reintroduce is a second in-process writer
/// opening a palace the daemon already holds under redb's exclusive lock, so the
/// test asserts the method name that proves the write went out.
/// Test: itself.
#[tokio::test]
async fn a_write_sends_a_write_method_over_the_socket() {
    for (verb, expected_method, text_key) in [
        (
            MemoryVerb::Remember {
                text: "the ledger is owned by ops".to_string(),
                room: None,
                tags: vec![],
            },
            super::REMEMBER_METHOD,
            "text",
        ),
        (
            MemoryVerb::Note {
                content: "deploy target is prod-east".to_string(),
                room: None,
                tags: vec![],
            },
            super::NOTE_METHOD,
            "content",
        ),
    ] {
        let (daemon, calls) =
            recording_daemon(json!({ "palace": "p", "status": "stored", "drawer_id": "d1" })).await;
        let cwd = tempfile::tempdir().expect("tempdir");
        let outcome = super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
            .await
            .expect("the stub answers");

        let (method, params) = only_call(&calls);
        assert_eq!(method, expected_method);
        assert_eq!(params["palace"], json!("p"));
        assert!(params.get(text_key).is_some(), "{params:?}");
        assert_eq!(outcome.count, None, "a write reports no hit count");
        assert_eq!(outcome.result["status"], json!("stored"));
    }
}

/// Why: `--palace` is the caller's explicit instruction, so it outranks every
/// derived level — including the `TRUSTY_MEMORY_PALACE` the session injected.
/// What: resolves against a directory carrying a committed pin, and asserts the
/// explicit value wins without the pin ever being consulted.
/// Test: itself.
#[test]
fn explicit_palace_outranks_the_derived_levels() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".trusty-tools")).expect("pin dir");
    std::fs::write(
        dir.path().join(".trusty-tools/trusty-memory.yaml"),
        "schema_version: 1\npalace: pinned-palace\n",
    )
    .expect("pin file");

    let opts = MemoryVerbOptions {
        palace: Some("explicit-palace".to_string()),
        socket: None,
        cwd: Some(dir.path().to_path_buf()),
    };
    let resolved = super::resolve_verb_palace(&recall("q"), &opts).expect("explicit always wins");
    assert_eq!(resolved.as_deref(), Some("explicit-palace"));
}

/// Why: a write has nowhere to land without a palace, and answering "stored"
/// against the daemon's own default would put the PM's fact in a palace nobody
/// chose. A READ in the same state is fine — the daemon answers it with a
/// palace index (#6318) naming the palaces to choose from.
/// What: an unparseable pin file makes resolution fail at level 2, which
/// `palace_resolve` reports as an error regardless of the environment.
/// Test: itself.
#[test]
fn a_write_without_a_resolvable_palace_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".trusty-tools")).expect("pin dir");
    std::fs::write(
        dir.path().join(".trusty-tools/trusty-memory.yaml"),
        "this: [is not: a pin\n",
    )
    .expect("pin file");

    let opts = MemoryVerbOptions {
        palace: None,
        socket: None,
        cwd: Some(dir.path().to_path_buf()),
    };
    let write = MemoryVerb::Note {
        content: "x".to_string(),
        room: None,
        tags: vec![],
    };
    let err = super::resolve_verb_palace(&write, &opts).expect_err("a write needs a palace");
    assert!(matches!(err, MemoryVerbError::Palace { .. }), "{err:?}");
    assert!(err.to_string().contains("--palace"), "{err}");

    let read = super::resolve_verb_palace(&recall("q"), &opts)
        .expect("a read falls through to the daemon's palace index");
    assert_eq!(read, None);
}

/// Why (#8352 acceptance 5): the daemon being down is the exact condition these
/// verbs exist for, so the failure has to be prompt, non-zero, and name the
/// socket that was dialled. It must never be downgraded to an empty result.
/// Test: itself.
#[tokio::test]
async fn a_dead_socket_is_an_error_naming_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let started = std::time::Instant::now();
    let err = super::run_verb(&recall("q"), &opts_at(Some("p"), &socket, dir.path()))
        .await
        .expect_err("nothing is serving that socket");

    assert!(matches!(err, MemoryVerbError::Call { .. }), "{err:?}");
    assert!(
        err.to_string().contains(&socket.display().to_string()),
        "the error must name the socket: {err}"
    );
    assert!(
        started.elapsed() < trusty_common::memory_rpc::DEFAULT_TIMEOUT,
        "a refused dial must not wait out the budget: {:?}",
        started.elapsed()
    );
}

/// Why: an agent parsing `--json` must not branch on key existence. Every key is
/// present on every verb, `null` where it does not apply.
/// Test: itself.
#[tokio::test]
async fn json_envelope_keys_are_always_present() {
    let (daemon, _calls) =
        recording_daemon(json!({ "palace": "p", "status": "stored", "drawer_id": "d1" })).await;
    let cwd = tempfile::tempdir().expect("tempdir");
    let outcome = super::run_verb(
        &MemoryVerb::Remember {
            text: "a fact worth keeping around".to_string(),
            room: None,
            tags: vec![],
        },
        &opts_at(Some("p"), daemon.socket(), cwd.path()),
    )
    .await
    .expect("the stub answers");

    let encoded = serde_json::to_value(&outcome).expect("serialise");
    for key in ["verb", "palace", "socket", "count", "result"] {
        assert!(encoded.get(key).is_some(), "missing `{key}` in {encoded}");
    }
    assert_eq!(encoded["verb"], json!("remember"));
    assert_eq!(encoded["count"], Value::Null);
    assert_eq!(
        encoded["socket"],
        json!(daemon.socket().display().to_string())
    );
}

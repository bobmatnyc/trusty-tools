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

use super::{FactSlot, MemoryVerb, MemoryVerbError, MemoryVerbOptions};

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
        slot: FactSlot::default(),
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
                slot: FactSlot::default(),
            },
            super::REMEMBER_METHOD,
            "text",
        ),
        (
            MemoryVerb::Note {
                content: "deploy target is prod-east".to_string(),
                room: None,
                tags: vec![],
                slot: FactSlot::default(),
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
        slot: FactSlot::default(),
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
            slot: FactSlot::default(),
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

/// A `remember` and a `note` carrying `slot`, with their text keys.
fn writes(slot: &FactSlot) -> [(MemoryVerb, &'static str); 2] {
    [
        (
            MemoryVerb::Remember {
                text: "session s1 resumes at the review gate".to_string(),
                room: None,
                tags: vec![],
                slot: slot.clone(),
            },
            "text",
        ),
        (
            MemoryVerb::Note {
                content: "PR 9254 is green and awaiting merge".to_string(),
                room: None,
                tags: vec![],
                slot: slot.clone(),
            },
            "content",
        ),
    ]
}

/// Why (#9142): a slot only supersedes the prior fact if the key and expiry
/// reach trusty-memory under its own schema keys, `fact_key` and `expires_at`.
/// Test: itself.
#[tokio::test]
async fn a_fact_key_and_expiry_reach_the_request() {
    let slot = FactSlot {
        fact_key: Some("ws:s1/resume".to_string()),
        expires_at: Some("2099-01-01T00:00:00Z".to_string()),
    };
    for (verb, _) in writes(&slot) {
        let (daemon, calls) = recording_daemon(json!({ "status": "stored" })).await;
        let cwd = tempfile::tempdir().expect("tempdir");
        super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
            .await
            .expect("the stub answers");

        let (_, params) = only_call(&calls);
        assert_eq!(params["fact_key"], json!("ws:s1/resume"), "{params}");
        assert_eq!(
            params["expires_at"],
            json!("2099-01-01T00:00:00Z"),
            "{params}"
        );
    }
}

/// Why (#9142): omitting both flags must leave the request byte-identical to
/// the pre-#9142 one — no `fact_key`/`expires_at` key, not even `null`.
/// Test: itself.
#[tokio::test]
async fn a_write_without_slot_flags_sends_the_old_request() {
    for (verb, text_key) in writes(&FactSlot::default()) {
        let (daemon, calls) = recording_daemon(json!({ "status": "stored" })).await;
        let cwd = tempfile::tempdir().expect("tempdir");
        super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
            .await
            .expect("the stub answers");

        let (_, params) = only_call(&calls);
        let text = params[text_key].clone();
        assert_eq!(params, json!({ text_key: text, "palace": "p" }));
    }
}

/// Why (#9142): an unparseable `--expires-at` is a caller typo. It must fail
/// before any RPC, never be dropped and never reach the daemon.
/// Test: itself.
#[tokio::test]
async fn a_bad_expires_at_is_refused_before_any_rpc() {
    let slot = FactSlot {
        fact_key: Some("pr:9254/state".to_string()),
        expires_at: Some("tomorrow".to_string()),
    };
    for (verb, _) in writes(&slot) {
        let (daemon, calls) = recording_daemon(json!({ "status": "stored" })).await;
        let cwd = tempfile::tempdir().expect("tempdir");
        let err = super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
            .await
            .expect_err("a bad timestamp is refused");

        assert!(matches!(err, MemoryVerbError::ExpiresAt { .. }), "{err:?}");
        assert!(err.to_string().contains("\"tomorrow\""), "{err}");
        assert!(
            calls.lock().expect("calls").is_empty(),
            "no RPC may be sent"
        );
    }
}

/// Why (#9142): the daemon stores a past `expires_at` as an unslotted drawer, so
/// it must be refused here, before any RPC.
/// Test: itself.
#[tokio::test]
async fn a_past_expires_at_is_refused_before_any_rpc() {
    let slot = FactSlot {
        fact_key: Some("pr:9254/state".to_string()),
        expires_at: Some("2020-01-01T00:00:00Z".to_string()),
    };
    for (verb, _) in writes(&slot) {
        let (daemon, calls) = recording_daemon(json!({ "status": "stored" })).await;
        let cwd = tempfile::tempdir().expect("tempdir");
        let err = super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
            .await
            .expect_err("a past expiry is refused");

        assert!(err.to_string().contains("in the past"), "{err}");
        assert!(
            calls.lock().expect("calls").is_empty(),
            "no RPC may be sent"
        );
    }
}

/// A drawer id in the shape trusty-memory mints (a v4 UUID).
const DRAWER: &str = "0b6f3c1e-8a52-4c1d-9e0f-2a7b5c4d3e21";

/// Why (#9340): `forget` is a write, so it reaches `memory_forget` with the
/// resolved palace and the id under the schema's own `drawer_id` key.
/// Test: itself.
#[tokio::test]
async fn forget_sends_memory_forget_with_the_drawer_id() {
    let (daemon, calls) =
        recording_daemon(json!({ "palace": "p", "status": "deleted", "drawer_id": DRAWER })).await;
    let cwd = tempfile::tempdir().expect("tempdir");
    let verb = MemoryVerb::Forget {
        drawer_id: DRAWER.to_string(),
    };
    let outcome = super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
        .await
        .expect("the stub answers");

    let (method, params) = only_call(&calls);
    assert_eq!(method, crate::core::memory_forget::FORGET_METHOD);
    assert_eq!(params, json!({ "palace": "p", "drawer_id": DRAWER }));
    assert_eq!(outcome.verb, "forget");
    assert_eq!(outcome.count, None);
}

/// Why (#9340): the daemon would refuse a non-UUID too, but its refusal comes
/// back as "did not answer", which reads as a transport fault.
/// Test: itself.
#[tokio::test]
async fn a_malformed_drawer_id_is_refused_before_any_rpc() {
    let (daemon, calls) = recording_daemon(json!({ "status": "deleted" })).await;
    let cwd = tempfile::tempdir().expect("tempdir");
    let verb = MemoryVerb::Forget {
        drawer_id: "d1".to_string(),
    };
    let err = super::run_verb(&verb, &opts_at(Some("p"), daemon.socket(), cwd.path()))
        .await
        .expect_err("a non-UUID id must be refused");

    assert!(matches!(err, MemoryVerbError::DrawerId { .. }), "{err:?}");
    assert!(err.to_string().contains("\"d1\""), "{err}");
    assert!(
        calls.lock().expect("recorded calls").is_empty(),
        "nothing may be sent"
    );
}

/// Why (#9340): `memory_forget` answers an unknown id with a successful
/// `status: "not_found"` body, so only `deleted` may count as success.
/// Test: itself.
#[test]
fn forget_failure_accepts_only_deleted() {
    use crate::core::memory_forget::forget_failure;

    assert_eq!(
        forget_failure(DRAWER, Some("p"), &json!({ "status": "deleted" })),
        None
    );
    let not_found = forget_failure(DRAWER, Some("p"), &json!({ "status": "not_found" }))
        .expect("not_found is a failure");
    assert!(
        not_found.contains(DRAWER) && not_found.contains("palace p"),
        "{not_found}"
    );
    assert!(not_found.contains("nothing was deleted"), "{not_found}");
    for body in [json!({ "status": "stored" }), json!({})] {
        let reason = forget_failure(DRAWER, None, &body).expect("only `deleted` succeeds");
        assert!(
            reason.contains("did not report the drawer deleted"),
            "{reason}"
        );
    }
}

/// A fixed clock for [`crate::core::memory_forget::match_fact_key`].
fn pinned_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-10-09T12:00:00+00:00")
        .expect("pinned clock parses")
        .with_timezone(&chrono::Utc)
}

/// One listed drawer holding `key`, with a raw `expires_at`.
fn slot_drawer(id: &str, key: &str, expires_at: Option<&str>) -> Value {
    json!({ "drawer_id": id, "fact_key": key, "expires_at": expires_at })
}

/// Run `match_fact_key` for slot `k` in palace `p`, returning the refusal detail.
fn fact_key_refusal(listing: &Value) -> String {
    match crate::core::memory_forget::match_fact_key("k", "p", listing, 100, pinned_now()) {
        Err(MemoryVerbError::FactKey { key, detail }) => {
            assert_eq!(key, "k");
            detail
        }
        other => panic!("expected a FactKey refusal, got {other:?}"),
    }
}

/// Why (#9340): an unparseable `expires_at` cannot be called live or expired,
/// so a match carrying one refuses.
/// Test: itself.
#[test]
fn match_fact_key_refuses_an_unreadable_expires_at() {
    let alone = json!({ "palace": "p", "drawers": [slot_drawer("bad", "k", Some("soon"))] });
    let detail = fact_key_refusal(&alone);
    assert!(
        detail.contains("drawer bad has an unreadable expires_at"),
        "{detail}"
    );
}

/// Why (#9340): beside a live match, skipping the unreadable co-claimant would
/// forget the live drawer, so the whole resolution refuses and none is chosen.
/// Test: itself.
#[test]
fn match_fact_key_refuses_an_unreadable_expires_at_beside_a_live_match() {
    let beside_live = json!({ "palace": "p", "drawers": [
        slot_drawer("live", "k", None),
        slot_drawer("bad", "k", Some("soon")),
    ] });
    let detail = fact_key_refusal(&beside_live);
    assert!(
        detail.contains("drawer bad has an unreadable expires_at"),
        "{detail}"
    );
}

/// Why (#9340): an answer with no `drawers` array is not a listing, so it must
/// not read as an empty palace.
/// Test: itself.
#[test]
fn match_fact_key_refuses_an_answer_without_a_drawers_array() {
    let detail = fact_key_refusal(&json!({ "palace": "p" }));
    assert!(detail.contains("without a drawers array"), "{detail}");
}

//! `GET`/`PUT /api/assistants/:id/memory` handler tests (#7428).
//!
//! Why: the route decides what another assistant may read, so its validation is
//! the security-relevant half — a fan-out entry that is not a real instance id,
//! or one naming the assistant itself, must be refused at the moment it is
//! written rather than tolerated until a chat turn tries to use it.
//! What: the resolved-palace read, the selectable list, a round-trip write, the
//! self and malformed-id refusals, and the refusal of a `palace` the
//! `[[stores]]` binding already pins. Driven against a `tempfile::TempDir`
//! (the `agent_stores.rs` pattern, so nothing races on `$HOME`), plus one
//! full-router test proving the route is wired.
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::assistant_memory::{MemoryBody, read_at, write_at};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

const ASSISTANT: &str = r#"[agent]
name = "izzie"
role = "assistant"
extends = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

const BOUND_ASSISTANT: &str = r#"[agent]
name = "cto-assistant"
role = "assistant"
extends = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "cto-kb"
palace = "cto"
"#;

/// An agents dir holding both fixtures, and an empty assistants root.
fn fixture() -> (
    tempfile::TempDir,
    Vec<std::path::PathBuf>,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    for (name, body) in [("izzie", ASSISTANT), ("cto-assistant", BOUND_ASSISTANT)] {
        let package = agents.join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("agent.toml"), body).unwrap();
    }
    let root = tmp.path().join("homes");
    (tmp, vec![agents], root)
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn put_body(palace: Option<&str>, fan_out: &[&str]) -> MemoryBody {
    serde_json::from_value(serde_json::json!({
        "palace": palace,
        "fan_out": fan_out,
    }))
    .unwrap()
}

/// Why: an assistant's palace is DERIVED, so the stored value is routinely
/// empty for one that does in fact have a palace. A pane rendering only the
/// stored value would show an empty field and read as "no memory".
#[tokio::test]
async fn get_reports_the_resolved_palace_and_selectable_assistants() {
    let (_tmp, dirs, root) = fixture();
    let body = body_json(read_at(&dirs, &root, "izzie")).await;

    assert_eq!(
        body["palace"],
        serde_json::Value::Null,
        "nothing stored yet"
    );
    assert_eq!(body["resolved"]["own"], "izzie");
    assert_eq!(body["resolved"]["source"], "instance-id");
    assert_eq!(
        body["available"],
        serde_json::json!(["cto-assistant"]),
        "the other assistants are selectable; this one is not"
    );

    // A binding-pinned palace still wins, and says so.
    let bound = body_json(read_at(&dirs, &root, "cto-assistant")).await;
    assert_eq!(bound["resolved"]["own"], "cto");
    assert_eq!(bound["resolved"]["source"], "binding");
}

#[tokio::test]
async fn put_replaces_the_whole_table() {
    let (_tmp, dirs, root) = fixture();
    let written = body_json(write_at(
        &dirs,
        &root,
        "izzie",
        put_body(None, &["cto-assistant", "cto-assistant"]),
    ))
    .await;
    assert_eq!(
        written["fan_out"],
        serde_json::json!(["cto-assistant"]),
        "a repeated selection is stored once"
    );
    assert_eq!(
        written["resolved"]["fan_out_palaces"],
        serde_json::json!(["cto"]),
        "the selected assistant resolves to ITS palace, not its id"
    );

    // Durable: a fresh read sees it, and clearing it is equally durable.
    let reread = body_json(read_at(&dirs, &root, "izzie")).await;
    assert_eq!(reread["fan_out"], serde_json::json!(["cto-assistant"]));
    let cleared = body_json(write_at(&dirs, &root, "izzie", put_body(None, &[]))).await;
    assert_eq!(cleared["fan_out"], serde_json::json!([]));
    assert_eq!(
        cleared["resolved"]["fan_out_palaces"],
        serde_json::json!([])
    );
}

/// Why: the recall path DROPS a bad entry so a stale setting never breaks a
/// chat turn, which means the write is the only place a user learns their
/// selection was wrong.
#[tokio::test]
async fn put_rejects_self_and_bad_ids() {
    let (_tmp, dirs, root) = fixture();

    let mine = write_at(&dirs, &root, "izzie", put_body(None, &["izzie"]));
    assert_eq!(mine.status(), StatusCode::BAD_REQUEST);
    let reason = body_json(mine).await;
    assert!(
        reason["error"]
            .as_str()
            .unwrap()
            .contains("OTHER assistants only"),
        "got {reason}"
    );

    let bad = write_at(&dirs, &root, "izzie", put_body(None, &["../escape"]));
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Nothing was stored by either refusal.
    assert_eq!(
        body_json(read_at(&dirs, &root, "izzie")).await["fan_out"],
        serde_json::json!([])
    );
}

/// Why: the binding wins by rule, so accepting a contradicting `palace` would
/// leave the user looking at a value nothing reads.
#[tokio::test]
async fn put_refuses_a_palace_the_binding_pins() {
    let (_tmp, dirs, root) = fixture();
    let refused = write_at(
        &dirs,
        &root,
        "cto-assistant",
        put_body(Some("somewhere-else"), &[]),
    );
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let reason = body_json(refused).await;
    assert!(
        reason["error"].as_str().unwrap().contains("agent.toml"),
        "the refusal names where the value actually lives: {reason}"
    );
}

/// Why: this is the containment boundary, not a usability nicety. `izzie` binds
/// no palace, so a `PUT {"palace": "cto"}` used to be accepted: `own_palace`
/// resolved it as `PalaceSource::Config`, `own_is_bound()` answered false, and
/// the chat path then treated `cto-assistant`'s palace as a derived one of
/// izzie's — recalling from it, persisting izzie's turns into it, and issuing a
/// `palace_create` that trusty-memory does not refuse for an existing name
/// (`handle_palace_create` rewrites `palace.json` with a fresh `created_at`).
#[tokio::test]
async fn put_refuses_a_palace_another_assistant_already_owns() {
    let (_tmp, dirs, root) = fixture();

    // `cto-assistant` binds `cto`; izzie may not claim it.
    let bound = write_at(&dirs, &root, "izzie", put_body(Some("cto"), &[]));
    assert_eq!(bound.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let reason = body_json(bound).await;
    assert!(
        reason["error"]
            .as_str()
            .unwrap()
            .contains("already belongs to assistant `cto-assistant`"),
        "the refusal names the owner: {reason}"
    );

    // And the DERIVED case: an unbound assistant's palace is its instance id,
    // which is equally taken.
    let derived = write_at(&dirs, &root, "cto-assistant", put_body(Some("izzie"), &[]));
    assert_eq!(derived.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Neither refusal wrote anything.
    let mine = body_json(read_at(&dirs, &root, "izzie")).await;
    assert_eq!(mine["palace"], serde_json::Value::Null);
    assert_eq!(mine["resolved"]["own"], "izzie");
    assert_eq!(mine["resolved"]["source"], "instance-id");

    // An unclaimed name is still accepted, so the guard is a collision check and
    // not a blanket ban on setting a palace.
    let free = write_at(&dirs, &root, "izzie", put_body(Some("izzie-notes"), &[]));
    assert_eq!(free.status(), StatusCode::OK);
    assert_eq!(
        body_json(free).await["resolved"]["own"],
        "izzie-notes",
        "a free name is still settable"
    );
}

#[tokio::test]
async fn get_rejects_an_unknown_assistant() {
    let (_tmp, dirs, root) = fixture();
    assert_eq!(
        read_at(&dirs, &root, "not-an-assistant").status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        read_at(&dirs, &root, "../escape").status(),
        StatusCode::BAD_REQUEST
    );
}

/// Why: every other test drives the core directly, so one router test is what
/// proves the path is reachable at all. It asks with a method the route does
/// NOT serve: an unmatched path answers `404` and a matched one answers `405`,
/// which separates "route absent" from "assistant absent" — both of which the
/// handler itself would report as `404`.
#[tokio::test]
async fn memory_route_is_wired_into_the_router() {
    let app = build_router(AppState::default());
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/assistants/izzie/memory")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "the path matched, so GET and PUT are wired"
    );
}

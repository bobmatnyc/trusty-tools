//! Integration tests for the wing MCP surface (ADR-0027 T9, issue #4809).
//!
//! Why: ADR-0027's finding was that `Wing` had zero construction sites — "a
//! level nobody reads is the defect". The acceptance bar for T9 is therefore
//! not that a `WINGS` table exists but that an MCP caller can actually USE
//! wings: list them, create one, rename it, write into it, and recall from it.
//! These tests drive every one of those through `dispatch_tool`, i.e. the exact
//! path an agent takes.
//! What: exercises `wing_list` / `wing_create` / `wing_rename`, the `wing`
//! argument on `memory_remember` / `memory_recall` / `memory_list`, the
//! "a caller who never names a wing is unaffected" guarantee, and the
//! fail-loud unknown-wing contract.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

fn ready_state(tmp: &TempDir) -> AppState {
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    state
}

/// Create a palace and return `(state, tmp)` ready for wing calls.
async fn palace(name: &str) -> (AppState, TempDir) {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": name, "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");
    (state, tmp)
}

/// Store `text` in `room`, optionally scoped to `wing`.
async fn remember(state: &AppState, p: &str, text: &str, room: &str, wing: Option<&str>) {
    let mut args = json!({
        "palace": p, "text": text, "room": room, "force": true
    });
    if let Some(w) = wing {
        args["wing"] = json!(w);
    }
    dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
}

#[tokio::test]
async fn wing_list_shows_the_default_wing() {
    // Every palace has a default wing from the moment it opens — that is what
    // makes "wing" never required of a caller.
    let (state, _t) = palace("wingtest").await;
    let out = dispatch_tool(&state, "wing_list", json!({ "palace": "wingtest" }))
        .await
        .expect("wing_list");
    let wings = out["wings"].as_array().expect("wings array");
    assert_eq!(wings.len(), 1, "exactly the default wing: {wings:?}");
    assert_eq!(wings[0]["label"], "default");
    assert_eq!(wings[0]["is_default"], true);
    assert!(wings[0]["wing_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn wing_create_then_list() {
    let (state, _t) = palace("wingtest").await;
    let created = dispatch_tool(
        &state,
        "wing_create",
        json!({ "palace": "wingtest", "label": "engineer" }),
    )
    .await
    .expect("wing_create");
    assert_eq!(created["created"], true);
    assert_eq!(created["label"], "engineer");

    let out = dispatch_tool(&state, "wing_list", json!({ "palace": "wingtest" }))
        .await
        .expect("wing_list");
    let labels: Vec<&str> = out["wings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"default"), "{labels:?}");
    assert!(labels.contains(&"engineer"), "{labels:?}");
}

#[tokio::test]
async fn wing_create_is_idempotent_over_mcp() {
    let (state, _t) = palace("wingtest").await;
    let a = dispatch_tool(
        &state,
        "wing_create",
        json!({ "palace": "wingtest", "label": "engineer" }),
    )
    .await
    .expect("first");
    // Case variant must resolve to the SAME wing, not a second one.
    let b = dispatch_tool(
        &state,
        "wing_create",
        json!({ "palace": "wingtest", "label": "ENGINEER" }),
    )
    .await
    .expect("second");
    assert_eq!(a["wing_id"], b["wing_id"]);
    assert_eq!(b["created"], false);
    assert_eq!(
        dispatch_tool(&state, "wing_list", json!({ "palace": "wingtest" }))
            .await
            .unwrap()["wings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn wing_create_rejects_blank() {
    let (state, _t) = palace("wingtest").await;
    assert!(
        dispatch_tool(
            &state,
            "wing_create",
            json!({ "palace": "wingtest", "label": "   " })
        )
        .await
        .is_err(),
        "a blank wing label must be rejected, not silently aliased"
    );
}

#[tokio::test]
async fn wing_rename_over_mcp() {
    let (state, _t) = palace("wingtest").await;
    dispatch_tool(
        &state,
        "wing_create",
        json!({ "palace": "wingtest", "label": "engineer" }),
    )
    .await
    .expect("create");
    let renamed = dispatch_tool(
        &state,
        "wing_rename",
        json!({ "palace": "wingtest", "wing": "engineer", "new_label": "platform" }),
    )
    .await
    .expect("rename");
    assert_eq!(renamed["wing"]["label"], "platform");

    // The old label is retired — a rename, not an alias.
    assert!(
        dispatch_tool(
            &state,
            "memory_list",
            json!({ "palace": "wingtest", "wing": "engineer" })
        )
        .await
        .is_err(),
        "the retired label must no longer resolve"
    );
}

#[tokio::test]
async fn wing_rename_rejects_taken_label_over_mcp() {
    let (state, _t) = palace("wingtest").await;
    for label in ["engineer", "pm"] {
        dispatch_tool(
            &state,
            "wing_create",
            json!({ "palace": "wingtest", "label": label }),
        )
        .await
        .expect("create");
    }
    assert!(
        dispatch_tool(
            &state,
            "wing_rename",
            json!({ "palace": "wingtest", "wing": "engineer", "new_label": "pm" }),
        )
        .await
        .is_err(),
        "renaming onto another wing's label must fail"
    );
}

#[tokio::test]
async fn wing_scoped_write_then_recall_over_mcp() {
    // The end-to-end acceptance case: two agent types hold same-named rooms,
    // write into them, and each recalls only its own.
    let (state, _t) = palace("wingtest").await;
    for label in ["engineer", "pm"] {
        dispatch_tool(
            &state,
            "wing_create",
            json!({ "palace": "wingtest", "label": label }),
        )
        .await
        .expect("create wing");
    }
    remember(
        &state,
        "wingtest",
        "The retry budget for the ingest worker is three attempts",
        "Planning",
        Some("engineer"),
    )
    .await;
    remember(
        &state,
        "wingtest",
        "The launch review is scheduled for the last Thursday of the quarter",
        "Planning",
        Some("pm"),
    )
    .await;

    let eng = dispatch_tool(
        &state,
        "memory_list",
        json!({ "palace": "wingtest", "wing": "engineer" }),
    )
    .await
    .expect("list engineer");
    let eng_rows = eng["drawers"].as_array().expect("drawers");
    assert_eq!(eng_rows.len(), 1, "engineer wing holds one drawer");
    assert!(
        eng_rows[0]["content"]
            .as_str()
            .unwrap()
            .contains("retry budget"),
        "got {:?}",
        eng_rows[0]["content"]
    );

    let pm = dispatch_tool(
        &state,
        "memory_list",
        json!({ "palace": "wingtest", "wing": "pm" }),
    )
    .await
    .expect("list pm");
    let pm_rows = pm["drawers"].as_array().expect("drawers");
    assert_eq!(pm_rows.len(), 1, "pm wing holds one drawer");
    assert!(pm_rows[0]["content"]
        .as_str()
        .unwrap()
        .contains("launch review"));

    // And wing-scoped RECALL returns only that wing's drawer.
    let recalled = dispatch_tool(
        &state,
        "memory_recall",
        json!({ "palace": "wingtest", "query": "retry budget ingest worker", "wing": "engineer" }),
    )
    .await
    .expect("wing-scoped recall");
    let contents: Vec<String> = recalled["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["content"].as_str().map(str::to_string))
        .collect();
    // Both halves are required. Asserting only the ABSENCE of the other wing's
    // content would let an implementation that returns nothing at all pass —
    // and this is the test guarding the headline claim that wings are
    // reachable AND scoped, so a silent zero-result regression is exactly the
    // failure it must catch.
    assert!(
        contents.iter().any(|c| c.contains("retry budget")),
        "engineer-wing recall returned no matching drawer: {contents:?}"
    );
    assert!(
        !contents.iter().any(|c| c.contains("launch review")),
        "a pm-wing drawer leaked into an engineer-wing recall: {contents:?}"
    );
}

#[tokio::test]
async fn a_caller_that_never_names_a_wing_is_unaffected() {
    // The guarantee most likely to regress: every pre-wing call shape must
    // behave exactly as it did — no wing argument anywhere, all drawers
    // visible, default wing used implicitly.
    let (state, _t) = palace("wingtest").await;
    remember(
        &state,
        "wingtest",
        "The ingest worker retries three times before giving up",
        "Planning",
        None,
    )
    .await;
    remember(
        &state,
        "wingtest",
        "Documentation lives under the docs directory in this repository",
        "Documentation",
        None,
    )
    .await;

    let all = dispatch_tool(&state, "memory_list", json!({ "palace": "wingtest" }))
        .await
        .expect("unscoped list");
    assert_eq!(
        all["drawers"].as_array().expect("drawers").len(),
        2,
        "an unscoped list must return every drawer"
    );

    // The pre-existing room filter still works, untouched by wings.
    let planning = dispatch_tool(
        &state,
        "memory_list",
        json!({ "palace": "wingtest", "room": "Planning" }),
    )
    .await
    .expect("room list");
    assert_eq!(planning["drawers"].as_array().unwrap().len(), 1);

    // And those wing-less writes landed in the default wing, which now
    // reports a non-zero room population.
    let wings = dispatch_tool(&state, "wing_list", json!({ "palace": "wingtest" }))
        .await
        .expect("wing_list");
    let default = &wings["wings"].as_array().unwrap()[0];
    assert_eq!(default["is_default"], true);
    assert_eq!(
        default["room_count"], 2,
        "both wing-less rooms belong to the default wing: {default:?}"
    );
}

#[tokio::test]
async fn recall_rejects_an_unknown_wing() {
    // Fail LOUD at the tool boundary: a typo'd wing that returned zero results
    // would read as "no memories" rather than "you misspelled it".
    let (state, _t) = palace("wingtest").await;
    let err = dispatch_tool(
        &state,
        "memory_recall",
        json!({ "palace": "wingtest", "query": "anything", "wing": "nosuchwing" }),
    )
    .await
    .expect_err("unknown wing must error");
    let msg = format!("{err:#}");
    assert!(msg.contains("unknown wing"), "{msg}");
    assert!(
        msg.contains("wing_list"),
        "the error must name the remedy: {msg}"
    );
}

#[tokio::test]
async fn memory_list_rejects_wing_and_room_together() {
    // A filter the caller supplied and the server ignored is exactly the
    // invisible failure ADR-0027 exists to remove.
    let (state, _t) = palace("wingtest").await;
    assert!(
        dispatch_tool(
            &state,
            "memory_list",
            json!({ "palace": "wingtest", "wing": "default", "room": "Planning" }),
        )
        .await
        .is_err(),
        "wing+room must be rejected, not silently half-honoured"
    );
}

#[tokio::test]
async fn wing_rename_rejects_blank_new_label() {
    // Same boundary guard `wing_create` has — the two must fail identically.
    let (state, _t) = palace("wingtest").await;
    dispatch_tool(
        &state,
        "wing_create",
        json!({ "palace": "wingtest", "label": "engineer" }),
    )
    .await
    .expect("create");
    assert!(
        dispatch_tool(
            &state,
            "wing_rename",
            json!({ "palace": "wingtest", "wing": "engineer", "new_label": "   " }),
        )
        .await
        .is_err(),
        "a blank new_label must be rejected"
    );
}

#[tokio::test]
async fn palace_info_reports_real_wings() {
    // ADR-0027 T9 replaces T8's hardcoded `1`. `wing_count` must track the
    // WINGS registry and agree with what `wing_list` returns.
    let (state, _t) = palace("wingtest").await;
    let info = dispatch_tool(&state, "palace_info", json!({ "palace": "wingtest" }))
        .await
        .expect("palace_info");
    assert_eq!(info["wing_count"], 1, "a fresh palace has its default wing");

    for label in ["engineer", "pm"] {
        dispatch_tool(
            &state,
            "wing_create",
            json!({ "palace": "wingtest", "label": label }),
        )
        .await
        .expect("create wing");
    }
    let info = dispatch_tool(&state, "palace_info", json!({ "palace": "wingtest" }))
        .await
        .expect("palace_info");
    assert_eq!(
        info["wing_count"], 3,
        "wing_count must be a real count, not a constant: {info:?}"
    );

    let listed = dispatch_tool(&state, "wing_list", json!({ "palace": "wingtest" }))
        .await
        .expect("wing_list")["wings"]
        .as_array()
        .expect("wings")
        .len();
    assert_eq!(
        info["wing_count"].as_u64().unwrap() as usize,
        listed,
        "palace_info and wing_list must never disagree"
    );
}

#[tokio::test]
async fn deep_recall_honours_a_wing_scope() {
    // ADR-0027 T7 gave L3 a room filter; T9 must not leave `memory_recall_deep`
    // silently ignoring `wing` — that is the invisible-failure class the ADR
    // exists to remove.
    let (state, _t) = palace("wingtest").await;
    for label in ["engineer", "pm"] {
        dispatch_tool(
            &state,
            "wing_create",
            json!({ "palace": "wingtest", "label": label }),
        )
        .await
        .expect("create wing");
    }
    remember(
        &state,
        "wingtest",
        "The retry budget for the ingest worker is three attempts",
        "Planning",
        Some("engineer"),
    )
    .await;
    remember(
        &state,
        "wingtest",
        "The launch review is scheduled for the last Thursday of the quarter",
        "Planning",
        Some("pm"),
    )
    .await;

    let out = dispatch_tool(
        &state,
        "memory_recall_deep",
        json!({ "palace": "wingtest", "query": "retry budget ingest worker", "wing": "engineer" }),
    )
    .await
    .expect("deep recall");
    let contents: Vec<String> = out["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        !contents.iter().any(|c| c.contains("launch review")),
        "a pm-wing drawer leaked into an engineer-wing deep recall: {contents:?}"
    );
}

#[tokio::test]
async fn recall_rejects_wing_and_room_together() {
    // The shared scope resolver must reject the combination on every read
    // path, not just memory_list.
    let (state, _t) = palace("wingtest").await;
    for tool in ["memory_recall", "memory_recall_deep"] {
        assert!(
            dispatch_tool(
                &state,
                tool,
                json!({ "palace": "wingtest", "query": "x", "wing": "default", "room": "Planning" }),
            )
            .await
            .is_err(),
            "{tool} must reject wing+room together"
        );
    }
}

#[tokio::test]
async fn wing_tools_are_listed() {
    // The acceptance bar itself: the tools are advertised on the MCP surface,
    // so an agent can discover them without being told they exist.
    let defs = trusty_memory::tools::tool_definitions();
    let names: Vec<&str> = defs["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for tool in ["wing_list", "wing_create", "wing_rename"] {
        assert!(names.contains(&tool), "{tool} missing from tools/list");
    }
}

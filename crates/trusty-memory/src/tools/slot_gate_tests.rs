//! The near-duplicate window gate against Tier C `fact_key` slot writes (#10643).
//!
//! Why: a slot rewrite whose new value differs from the occupant by a few
//! characters (a step number) was dropped as a near-duplicate, so recall kept
//! the stale occupant. Ordinary near-duplicate suppression must still hold.
//! What: dispatch-level tests over `memory_note` and `memory_remember`.
//! Test: this module.

use serde_json::{Value, json};

use super::tests::test_state;
use super::{dispatch_tool, open_palace_handle};

const V1: &str = "workstream live-10609 checkpoint v1: resume the migration run from step 3";
const V2: &str = "workstream live-10609 checkpoint v2: resume the migration run from step 7";
const KEY: &str = "ws:live-10609/checkpoint";

async fn write(state: &crate::AppState, tool: &str, args: Value) -> Value {
    dispatch_tool(state, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} dispatch failed: {e:#}"))
}

/// A near-duplicate rewrite of an occupied slot replaces the occupant, on both
/// write tools.
#[tokio::test]
async fn slot_rewrite_that_near_duplicates_the_occupant_replaces_it() {
    for tool in ["memory_note", "memory_remember"] {
        let (state, _tmp) = test_state();
        let palace = "slotgate";
        write(&state, "palace_create", json!({"name": palace})).await;

        let first = write(
            &state,
            tool,
            json!({"palace": palace, "content": V1, "fact_key": KEY}),
        )
        .await;
        assert_eq!(first["status"], "stored", "{tool}: {first}");

        let second = write(
            &state,
            tool,
            json!({"palace": palace, "content": V2, "fact_key": KEY}),
        )
        .await;
        assert_eq!(
            second["status"], "stored",
            "{tool}: a slot rewrite must not be skipped as a near-duplicate: {second}"
        );
        assert_eq!(second["tier"], "C", "{tool}: {second}");

        let handle = open_palace_handle(&state, palace).expect("open palace");
        let winner: uuid::Uuid = second["drawer_id"]
            .as_str()
            .expect("drawer_id")
            .parse()
            .expect("uuid");
        assert_eq!(
            handle.kg.drawer_id_for_fact_key(KEY).expect("slot lookup"),
            Some(winner),
            "{tool}: the rewrite must hold the slot"
        );
    }
}

/// Content that claims no slot — none asked for, or a key the admission gate
/// refused — is still suppressed as a near-duplicate, including a
/// near-duplicate of a slot occupant.
#[tokio::test]
async fn near_duplicate_without_an_admitted_slot_is_still_skipped() {
    let cases = [
        ("no fact_key, prior plain drawer", None, json!({})),
        ("refused fact_key", None, json!({"fact_key": "not a key"})),
        ("no fact_key, prior slot occupant", Some(KEY), json!({})),
    ];
    for (label, first_key, extra) in cases {
        let (state, _tmp) = test_state();
        let palace = "plaingate";
        write(&state, "palace_create", json!({"name": palace})).await;

        let mut first_args = json!({"palace": palace, "content": V1});
        if let Some(key) = first_key {
            first_args["fact_key"] = json!(key);
        }
        let first = write(&state, "memory_note", first_args).await;
        assert_eq!(first["status"], "stored", "{label}: {first}");

        let mut second_args = json!({"palace": palace, "content": V2});
        if let (Some(obj), Some(more)) = (second_args.as_object_mut(), extra.as_object()) {
            obj.extend(more.clone());
        }
        let second = write(&state, "memory_note", second_args).await;
        assert_eq!(second["status"], "skipped", "{label}: {second}");
        assert!(
            second["reason"]
                .as_str()
                .unwrap_or("")
                .contains("duplicate within window"),
            "{label}: {second}"
        );

        let handle = open_palace_handle(&state, palace).expect("open palace");
        assert_eq!(
            handle.kg.load_drawers().expect("load drawers").len(),
            1,
            "{label}: the skipped write must leave no drawer"
        );
    }
}

/// Fail-open check: an unparseable `expires_at` is a caller error even when
/// the content would also trip the near-duplicate gate. A "skipped" success
/// envelope there would hide the typo, and the write never happens.
#[tokio::test]
async fn unparseable_expires_at_errors_even_when_the_content_is_a_near_duplicate() {
    for tool in ["memory_note", "memory_remember"] {
        let (state, _tmp) = test_state();
        let palace = "badexpiry";
        write(&state, "palace_create", json!({"name": palace})).await;
        write(&state, tool, json!({"palace": palace, "content": V1})).await;

        let out = dispatch_tool(
            &state,
            tool,
            json!({
                "palace": palace,
                "content": V2,
                "fact_key": KEY,
                "expires_at": "tomorrow-ish",
            }),
        )
        .await;
        let err = match out {
            Ok(v) => panic!("{tool}: a bad expires_at must be an error, got {v}"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("expires_at"), "{tool}: {err}");
    }
}

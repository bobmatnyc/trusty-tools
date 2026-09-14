//! Serialized-response byte ceiling tests for the MCP result tools (#7493).
//!
//! Why: the guarantee is about the bytes a caller receives and about what the
//! response says is missing from them. The fold has to drop WHOLE entries,
//! never drop a recall's L0/L1 grounding, never answer a non-empty result set
//! with an empty one, and never claim to be under a ceiling it is over.
//! What: unit coverage over [`super::byte_cap::apply`] with synthetic tool
//! bodies — the fold is a pure function of (tool, args, body), so a mock daemon
//! would add cost without adding signal — plus the descriptor and router
//! consistency arms and the measurement-failure arm.
//! Test: this module.

use serde_json::{json, Value};

use super::byte_cap::{apply, capped_tool_names, measure, DEFAULT_MAX_BYTES, HARD_MAX_BYTES};
use super::tests::test_state;
use super::{dispatch_tool, tool_definitions};

/// Ceiling that keeps exactly four of [`hit`]'s ten uniform entries.
///
/// The fixture's entries are uniform and roughly 1 KB each once serialized, so
/// four plus the envelope and the truncation notice fit under this and five do
/// not. Change the fixture and this constant moves with it —
/// `over_cap_recall_keeps_whole_hits_and_reports_the_withheld_count` prints the
/// counts and sizes it measured on failure.
const FOUR_HIT_CEILING: usize = 4_600;

/// One recall hit, large enough that ten of them clear any sane ceiling.
///
/// `layer` decides whether the fold may drop it: below
/// [`super::recall_projection::FIRST_FILTERABLE_LAYER`] is L0 identity / L1
/// essential grounding and is protected.
fn hit(n: usize, layer: u64) -> Value {
    json!({
        "drawer_id": format!("00000000-0000-4000-8000-{n:012}"),
        "content": format!(
            "hit {n}: {}",
            "a memory body wide enough to matter to a byte ceiling. ".repeat(16)
        ),
        "score": 0.42_f64,
        "layer": layer,
        "tags": ["wildlife", "notes"],
        "importance": 0.7_f64,
        "drawer_type": "Fact",
    })
}

/// One hit whose body alone is about 30 KB, so two of them clear the default
/// 48 KiB ceiling between them.
fn big_hit(n: usize, layer: u64) -> Value {
    let mut entry = hit(n, layer);
    entry["content"] = Value::String("x".repeat(30_000));
    entry
}

/// A `memory_recall` body whose every hit is a droppable L2 one.
fn recall_body(hits: usize) -> Value {
    json!({
        "palace": "demo",
        "query": "quokkas",
        "results": (0..hits).map(|n| hit(n, 2)).collect::<Vec<_>>(),
        "dropped_below_floor": 0,
    })
}

/// The drawer ids a response actually returned, in order.
fn returned_ids(resp: &Value, key: &str) -> Vec<String> {
    resp[key]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|e| e["drawer_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// An under-cap body for each capped tool, so
/// `an_under_cap_call_is_unchanged_except_for_the_truncated_flag` can iterate
/// the table instead of a hardcoded list. A new capped tool with no body here
/// fails loudly rather than being skipped.
fn under_cap_body(tool: &str) -> Value {
    match tool {
        "memory_recall" | "memory_recall_deep" => recall_body(1),
        "memory_recall_all" => json!({
            "query": "quokkas",
            "results": [{"palace_id": "demo", "drawer_id": "d1", "content": "one",
                         "importance": 0.5, "tags": [], "score": 0.4, "layer": 2,
                         "drawer_type": "Fact"}],
        }),
        "memory_list" => json!({
            "palace": "demo",
            "drawers": [{"drawer_id": "d1", "content": "one", "importance": 0.5,
                         "tags": [], "created_at": "2026-09-14T00:00:00+00:00",
                         "drawer_type": "Fact", "expires_at": Value::Null}],
        }),
        "kg_query" => json!({
            "subject": "quokka",
            "triples": [{"subject": "quokka", "predicate": "is-a", "object": "marsupial",
                         "valid_from": "2026-09-14T00:00:00+00:00", "valid_to": Value::Null,
                         "confidence": 1.0, "provenance": "test"}],
            "kg_triple_count": 1,
        }),
        "chat_session_recall" => json!({
            "id": "s1",
            "title": "a session",
            "created_at": "2026-09-14T00:00:00Z",
            "updated_at": "2026-09-14T00:00:00Z",
            "history": [{"role": "user", "content": "hello", "attachments": []}],
        }),
        "list_prompt_facts" => json!({
            "facts": [{"subject": "user", "predicate": "prefers", "object": "snake_case",
                       "affirmed_at": "2026-09-14T00:00:00+00:00"}],
        }),
        other => panic!("add an under-cap body for the newly capped tool {other}"),
    }
}

/// Over the ceiling, whole entries are dropped from the tail and the response
/// says how many.
#[test]
fn over_cap_recall_keeps_whole_hits_and_reports_the_withheld_count() {
    let mut resp = recall_body(10);
    apply(
        "memory_recall",
        &json!({"max_bytes": FOUR_HIT_CEILING}),
        &mut resp,
    )
    .expect("fold");

    let size = measure(&resp).expect("measure");
    assert_eq!(
        resp["returned"],
        json!(4),
        "expected 4 of 10 hits under a {FOUR_HIT_CEILING}-byte ceiling; \
         got {} at {size} bytes",
        resp["returned"]
    );
    assert_eq!(resp["withheld"], json!(6));
    assert_eq!(resp["truncated"], json!(true));
    assert!(size <= FOUR_HIT_CEILING, "{size} bytes is over the ceiling");
    // Whole entries, never one cut mid-object: each kept hit still carries
    // every field the handler put on it.
    let kept = resp["results"].as_array().expect("results");
    assert_eq!(kept.len(), 4);
    for entry in kept {
        assert!(entry["drawer_id"].is_string(), "partial entry: {entry}");
        assert!(entry["content"].is_string(), "partial entry: {entry}");
        assert!(entry["drawer_type"].is_string(), "partial entry: {entry}");
    }
    assert_eq!(
        returned_ids(&resp, "results"),
        returned_ids(&recall_body(4), "results")
    );
    let notice = resp["truncation_notice"].as_str().expect("notice");
    assert!(notice.contains("6 of 10 hits were withheld"), "{notice}");
    assert!(notice.contains("`top_k`"), "{notice}");
    assert!(notice.contains("`min_score`"), "{notice}");
}

/// The fold drops L2 hits first; identity and essential grounding survives.
///
/// The L1 hit sits LAST in the array, so a fold that merely truncated the tail
/// would drop it. Protection is by layer, not by position.
#[test]
fn identity_and_essential_hits_survive_a_cap_that_drops_l2() {
    let mut results: Vec<Value> = (0..9).map(|n| hit(n, 2)).collect();
    results[0] = hit(0, 0);
    results.push(hit(9, 1));
    let mut resp = json!({
        "palace": "demo",
        "query": "quokkas",
        "results": results,
        "dropped_below_floor": 0,
    });

    apply(
        "memory_recall",
        &json!({"max_bytes": FOUR_HIT_CEILING}),
        &mut resp,
    )
    .expect("fold");

    assert_eq!(resp["truncated"], json!(true));
    let ids = returned_ids(&resp, "results");
    let l0 = hit(0, 0)["drawer_id"].as_str().expect("id").to_string();
    let l1 = hit(9, 1)["drawer_id"].as_str().expect("id").to_string();
    assert!(ids.contains(&l0), "L0 identity hit was dropped: {ids:?}");
    assert!(ids.contains(&l1), "L1 essential hit was dropped: {ids:?}");
    assert!(
        resp["withheld"].as_u64().unwrap_or(0) > 0,
        "nothing was withheld, so this proves nothing"
    );
    let layers: Vec<u64> = resp["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|e| e["layer"].as_u64())
        .collect();
    assert!(
        layers.iter().filter(|l| **l >= 2).count() < 8,
        "no L2 hit was dropped: {layers:?}"
    );
}

/// Protected entries are the floor the fold cannot go below, even when they
/// alone exceed the ceiling.
///
/// Two 30 KB L0/L1 hits are already over the default 48 KiB, so the fold drops
/// every droppable hit, keeps both protected ones, and says the response is
/// OVER the ceiling rather than under it. Without the floor there would be
/// nothing to stop the fold from answering identity-less.
#[test]
fn protected_hits_ship_over_the_ceiling_when_they_alone_exceed_it() {
    let mut results = vec![big_hit(0, 0), big_hit(1, 1)];
    results.extend((2..7).map(|n| hit(n, 2)));
    let mut resp = json!({
        "palace": "demo",
        "query": "quokkas",
        "results": results,
        "dropped_below_floor": 0,
    });

    apply("memory_recall", &json!({}), &mut resp).expect("fold");

    assert_eq!(resp["truncated"], json!(true));
    assert_eq!(resp["returned"], json!(2));
    assert_eq!(resp["withheld"], json!(5));
    let ids = returned_ids(&resp, "results");
    for protected in [big_hit(0, 0), big_hit(1, 1)] {
        let id = protected["drawer_id"].as_str().expect("id").to_string();
        assert!(ids.contains(&id), "protected hit was dropped: {ids:?}");
    }
    let size = measure(&resp).expect("measure");
    assert!(
        size > DEFAULT_MAX_BYTES,
        "the fixture no longer exceeds the ceiling at {size} bytes"
    );
    let notice = resp["truncation_notice"].as_str().expect("notice");
    assert!(
        notice.contains(&format!("exceeds the {DEFAULT_MAX_BYTES}-byte ceiling")),
        "{notice}"
    );
    assert!(
        !notice.contains("to keep this response under"),
        "an over-ceiling response must not claim to be under it: {notice}"
    );
}

/// `full: true` is the escape hatch — every entry, no ceiling, no notice.
#[test]
fn full_returns_every_entry_with_no_ceiling() {
    let mut resp = recall_body(10);
    apply(
        "memory_recall",
        &json!({"full": true, "max_bytes": 10}),
        &mut resp,
    )
    .expect("fold");

    assert_eq!(resp["results"].as_array().expect("results").len(), 10);
    assert_eq!(resp["truncated"], json!(false));
    assert!(resp.get("truncation_notice").is_none());
    assert!(resp.get("withheld").is_none());
    // `full` removes the ceiling, so a `max_bytes` alongside it is moot rather
    // than clamped.
    assert!(resp.get("max_bytes_clamped").is_none());
}

/// A `max_bytes` above the hard cap is clamped, and the clamp is reported.
#[test]
fn a_max_bytes_over_the_hard_cap_is_clamped_and_reported() {
    let mut resp = recall_body(2);
    apply(
        "memory_recall",
        &json!({"max_bytes": HARD_MAX_BYTES * 4}),
        &mut resp,
    )
    .expect("fold");

    assert_eq!(resp["max_bytes_clamped"], json!(true));
    assert_eq!(resp["max_bytes"], json!(HARD_MAX_BYTES));
    // The clamp is reported whether or not anything was withheld — the caller
    // asked for something that was changed.
    assert_eq!(resp["truncated"], json!(false));
}

/// When even one entry exceeds the ceiling, that entry ships with a notice
/// that says the response is OVER the ceiling — never one claiming it fits.
#[test]
fn an_oversized_first_entry_ships_alone_under_an_honest_notice() {
    let mut resp = json!({
        "palace": "demo",
        "drawers": (0..3).map(|n| hit(n, 2)).collect::<Vec<_>>(),
    });
    apply("memory_list", &json!({"max_bytes": 200}), &mut resp).expect("fold");

    assert_eq!(resp["returned"], json!(1));
    assert_eq!(resp["withheld"], json!(2));
    assert_eq!(resp["truncated"], json!(true));
    assert_eq!(resp["drawers"].as_array().expect("drawers").len(), 1);
    let notice = resp["truncation_notice"].as_str().expect("notice");
    assert!(notice.contains("exceeds the 200-byte ceiling"), "{notice}");
    assert!(
        !notice.contains("to keep this response under"),
        "an over-ceiling response must not claim to be under it: {notice}"
    );
    assert!(notice.contains("the other 2 were dropped"), "{notice}");
}

/// A present but non-boolean `full` is an error, never coerced.
#[test]
fn a_non_boolean_full_is_rejected() {
    let mut resp = recall_body(1);
    let err = apply("memory_recall", &json!({"full": "true"}), &mut resp)
        .expect_err("a quoted boolean must not be coerced");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("memory_recall: 'full' must be a boolean"),
        "{msg}"
    );
}

/// A non-integer or out-of-range `max_bytes` is an error, never ignored.
#[test]
fn a_non_integer_or_out_of_range_max_bytes_is_rejected() {
    for bad in [
        json!("48000"),
        json!(1.5),
        json!(-1),
        json!(u64::from(u32::MAX) + 1),
    ] {
        let mut resp = recall_body(1);
        let err = apply("memory_recall", &json!({"max_bytes": bad}), &mut resp)
            .expect_err("a non-integer or out-of-range max_bytes must not be coerced");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("memory_recall: 'max_bytes' must be an integer"),
            "{msg}"
        );
    }
}

/// An under-cap call on every capped tool is what it always was, plus the
/// `truncated: false` verdict.
#[test]
fn an_under_cap_call_is_unchanged_except_for_the_truncated_flag() {
    for tool in capped_tool_names() {
        let body = under_cap_body(tool);
        let mut got = body.clone();
        apply(tool, &json!({}), &mut got).expect("fold");
        assert_eq!(got["truncated"], json!(false), "{tool}");
        let mut stripped = got.clone();
        stripped
            .as_object_mut()
            .expect("object")
            .remove("truncated");
        assert_eq!(stripped, body, "{tool} changed an under-cap response");
    }
}

/// An uncapped tool's response is returned byte-for-byte, with no verdict.
#[test]
fn an_uncapped_tool_is_returned_untouched() {
    let body = json!({"drawer_id": "d1", "palace": "demo", "status": "stored"});
    let mut got = body.clone();
    apply("memory_remember", &json!({"max_bytes": 1}), &mut got).expect("fold");
    assert_eq!(got, body);
}

/// The #6318 palace-index fallback carries no entries array, so the fold
/// leaves it exactly as the handler built it.
#[test]
fn a_palace_index_fallback_is_left_untouched() {
    let body = json!({"palaces": [{"id": "demo", "drawers": 3}], "hint": "name one"});
    let mut got = body.clone();
    apply("memory_recall", &json!({}), &mut got).expect("fold");
    assert_eq!(got, body);
}

/// Every capped tool advertises the two knobs it honours.
///
/// Iterates the enforcement table itself, so a tool added there but missing
/// from `tools/list` fails here rather than shipping an unadvertised knob.
#[test]
fn every_capped_tool_advertises_max_bytes_and_full() {
    let defs = tool_definitions();
    let tools = defs["tools"].as_array().expect("descriptor array");
    assert_eq!(capped_tool_names().len(), 7, "the capped roster changed");
    for name in capped_tool_names() {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing from tools/list"));
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["max_bytes"]["type"], "integer", "{name}");
        assert_eq!(
            props["max_bytes"]["default"],
            json!(DEFAULT_MAX_BYTES),
            "{name}"
        );
        assert_eq!(props["full"]["type"], "boolean", "{name}");
    }
    // An uncapped tool gains neither knob.
    let remember = tools
        .iter()
        .find(|t| t["name"] == "memory_remember")
        .expect("memory_remember");
    assert!(remember["inputSchema"]["properties"]
        .get("max_bytes")
        .is_none());
}

/// Every capped tool is a tool the dispatcher actually routes.
///
/// A name that drifts out of the router — a rename, a removed arm — would
/// otherwise leave the table silently capping nothing.
#[tokio::test]
async fn every_capped_tool_is_routed_by_the_dispatcher() {
    let (state, _tmp) = test_state();
    for name in capped_tool_names() {
        // An empty argument object is enough: what matters is that the router
        // claims the name, not that the call succeeds.
        if let Err(e) = dispatch_tool(&state, name, json!({})).await {
            let msg = format!("{e:#}");
            assert!(!msg.contains("unknown tool"), "{name} is not routed: {msg}");
        }
    }
}

/// A measurement that cannot be taken is an error, never a licence to return
/// the body unmeasured.
///
/// `serde_json::Value` itself always serializes, so the reachable failure point
/// is the measurement helper the fold calls with `?`; this pins its error arm
/// on a value serde_json genuinely rejects — a map keyed by a tuple, which has
/// no JSON object-key form.
#[test]
fn a_measurement_failure_returns_an_error_not_an_unmeasured_body() {
    use std::collections::HashMap;

    let unserializable: HashMap<(u8, u8), &str> = HashMap::from([((1u8, 2u8), "entry")]);
    let err = measure(&unserializable).expect_err("a tuple map key cannot serialize");
    let msg = format!("{err:#}");
    assert!(msg.contains("measurement failed"), "{msg}");
    assert!(msg.contains("unmeasured"), "{msg}");
}

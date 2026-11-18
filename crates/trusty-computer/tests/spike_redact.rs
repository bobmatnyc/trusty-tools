//! Port of the spike's `tests/test_redact.py` (3 cases), plus the redaction
//! half of `test_broker.py::test_snapshot_strips_menu_bar_from_structured_and_text`,
//! run on the same fake-daemon snapshot.

mod common;

use std::collections::BTreeSet;

use common::window_state;
use serde_json::Value;
use trusty_computer::redact::{fingerprint, redact_reply, strip_markdown};

#[test]
fn redact_keeps_window_and_drops_menu_subtree() {
    let r = redact_reply(&window_state(0));
    let want: BTreeSet<String> = ["tok-window", "tok-field", "tok-button", "tok-inert"]
        .map(String::from)
        .into();
    assert_eq!(r.tokens, want);
    assert_eq!(r.removed, 4);
    assert_eq!(r.result["structuredContent"]["element_count"], 4);
    let text = r.result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(!text.contains("Recent Items"));
}

#[test]
fn strip_markdown_resumes_after_subtree() {
    let text =
        "- [0] AXWindow\n- [1] AXMenuBar\n  - [2] AXMenuItem \"x\"\n- [3] AXWindow \"after\"";
    assert_eq!(
        strip_markdown(text),
        "- [0] AXWindow\n- [3] AXWindow \"after\""
    );
}

fn markdown_mut(state: &mut Value) -> &mut String {
    match &mut state["structuredContent"]["tree_markdown"] {
        Value::String(s) => s,
        other => panic!("tree_markdown is {other}"),
    }
}

#[test]
fn fingerprint_ignores_menu_changes_but_sees_window_changes() {
    let base = redact_reply(&window_state(0)).fingerprint;
    let mut s = window_state(0);
    markdown_mut(&mut s).push_str("\n    - [8] AXMenuItem \"new recent\"");
    assert_eq!(redact_reply(&s).fingerprint, base);
    let changed = markdown_mut(&mut s).replace("clicks=0", "clicks=1");
    *markdown_mut(&mut s) = changed;
    let redacted = redact_reply(&s).result;
    let structured = redacted["structuredContent"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert_ne!(fingerprint(&structured), base);
}

#[test]
fn snapshot_strips_menu_bar_from_structured_and_text() {
    let result = redact_reply(&window_state(0)).result;
    let roles: BTreeSet<&str> = result["structuredContent"]["elements"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|e| e["role"].as_str())
        .collect();
    assert!(!roles.contains("AXMenuBar") && !roles.contains("AXMenuItem"));
    let blob = result.to_string();
    for word in ["Recent Items", "Shut Down", "AXMenuBar"] {
        assert!(!blob.contains(word), "{word} leaked: {blob}");
    }
    assert_eq!(result["structuredContent"]["redacted_menu_bar_elements"], 4);
}

#[test]
fn non_snapshot_result_passes_through() {
    let plain = serde_json::json!({"tool": "click", "ok": true});
    let r = redact_reply(&plain);
    assert_eq!(
        (r.result, r.removed, r.fingerprint.as_str()),
        (plain, 0, "")
    );
}

#[test]
fn parent_cycle_ends_the_walk() {
    let state = serde_json::json!({"structuredContent": {"elements": [
        {"element_index": 1, "role": "AXGroup", "parent_index": 2, "element_token": "a"},
        {"element_index": 2, "role": "AXGroup", "parent_index": 1, "element_token": "b"},
    ]}});
    assert_eq!(redact_reply(&state).removed, 0);
}

/// #11460 fix round: a reply with only text parts still loses its menu bar.
#[test]
fn text_parts_are_stripped_without_a_structured_part() {
    let mut state = window_state(0);
    state
        .as_object_mut()
        .expect("object")
        .remove("structuredContent");
    let r = redact_reply(&state);
    let text = r.result["content"][0]["text"].as_str().expect("text");
    assert!(!text.contains("AXMenuBar"), "{text}");
    assert!(!text.contains("Shut Down"), "{text}");
    assert!(text.contains("Press Me"), "{text}");
    assert!(r.tokens.is_empty());
}

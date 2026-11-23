//! The bounded window screenshot (issue #11694): the reply filter, the policy
//! row, and the broker pipeline against the fake daemon.

mod common;

use std::collections::BTreeSet;

use common::rig::{Rig, WINDOW, pid};
use common::{FAKE_PNG, window_state, with_capture};
use serde_json::{Value, json};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::mcp::image_result;
use trusty_computer::policy::{Scope, Verdict, check_call, rule};
use trusty_computer::screenshot::{
    MAX_IMAGE_BASE64, SCREENSHOT_TOOL, bound, driver_args, strip_images,
};

/// A daemon reply to a screenshot of window 7 of pid 100.
fn capture() -> Value {
    with_capture(window_state(0), &json!({"pid": 100, "window_id": 7}), false)
}

fn refused(result: &Value) -> &'static str {
    match bound(result, 7) {
        Ok(v) => panic!("admitted {v}"),
        Err((code, _)) => code,
    }
}

#[test]
fn bound_returns_image_only() {
    let out = bound(&capture(), 7).expect("admitted");
    assert_eq!(out["image"], FAKE_PNG);
    assert_eq!(out["mime_type"], "image/png");
    assert_eq!(
        (out["window_id"].as_u64(), out["pid"].as_u64()),
        (Some(7), Some(100))
    );
    let keys: BTreeSet<&str> = out
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["height", "image", "mime_type", "pid", "width", "window_id"])
    );
    let text = out.to_string();
    for leak in ["AXMenuBar", "Shut Down", "tok-", "tree_markdown"] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }
}

#[test]
fn bound_refuses_secure_field() {
    // #11694: the last row is how cua-driver 0.23.2 reports an NSSecureTextField.
    let fields = [
        json!({"element_index": 9, "role": "AXSecureTextField", "parent_index": 0}),
        json!({"element_index": 9, "subrole": "AXSecureTextField", "parent_index": 0}),
        json!({"element_index": 9, "role": "AXTextField", "parent_index": 0,
            "value": "", "value_description": "contains secure text"}),
    ];
    for field in fields {
        let key = field.to_string();
        let mut snap = counted(json!(false), json!(70), json!(70));
        snap["structuredContent"]["elements"]
            .as_array_mut()
            .expect("elements")
            .push(field);
        assert_eq!(refused(&snap), "sensitive_content", "{key}");
    }
}

/// `capture()` with the 0.23.2 completeness fields set to the given values,
/// and its elements padded with plain buttons to 69, as the live window walks.
fn counted(flag: Value, returned: Value, total: Value) -> Value {
    let mut snap = capture();
    let sc = &mut snap["structuredContent"];
    let elements = sc["elements"].as_array_mut().expect("elements");
    while elements.len() < 69 {
        let i = elements.len();
        elements.push(json!({"element_index": i, "role": "AXButton", "parent_index": 0}));
    }
    sc["elements_complete"] = flag;
    sc["returned_element_count"] = returned;
    sc["total_element_count"] = total;
    snap
}

#[test]
fn bound_admits_equal_counts_without_flag() {
    // #11694: cua-driver 0.23.2 says `elements_complete: false` on a full walk.
    let snap = counted(json!(false), json!(69), json!(69));
    assert_eq!(bound(&snap, 7).expect("admitted")["image"], FAKE_PNG);
    let mut not_degraded = snap.clone();
    not_degraded["structuredContent"]["degraded"] = json!(false);
    assert!(bound(&not_degraded, 7).is_ok());
}

#[test]
fn bound_refuses_incomplete_tree() {
    let unequal = counted(json!(false), json!(69), json!(70));
    assert_eq!(refused(&unequal), "screenshot_unverified");
    // The flag alone never vouches for a tree whose counts disagree.
    let flagged = counted(json!(true), json!(69), json!(70));
    assert_eq!(refused(&flagged), "screenshot_unverified");
    let mut degraded = counted(json!(false), json!(69), json!(69));
    for value in [json!(true), json!("yes"), json!(1)] {
        degraded["structuredContent"]["degraded"] = value;
        assert_eq!(refused(&degraded), "screenshot_unverified");
    }
    let absent = [Value::Null, json!("69")];
    for missing in &absent {
        for snap in [
            counted(json!(true), missing.clone(), json!(69)),
            counted(json!(true), json!(69), missing.clone()),
            counted(json!(true), missing.clone(), missing.clone()),
        ] {
            assert_eq!(refused(&snap), "screenshot_unverified", "{missing}");
        }
    }
    // Equal counts vouch for nothing when the elements they count are absent.
    let mut no_elements = counted(json!(false), json!(69), json!(69));
    let sc = no_elements["structuredContent"]
        .as_object_mut()
        .expect("object");
    sc.remove("elements");
    assert_eq!(refused(&no_elements), "screenshot_unverified");
    let mut short = counted(json!(false), json!(69), json!(69));
    short["structuredContent"]["elements"]
        .as_array_mut()
        .expect("elements")
        .pop();
    assert_eq!(refused(&short), "screenshot_unverified");
    let mut removed = capture();
    let sc = removed["structuredContent"]
        .as_object_mut()
        .expect("object");
    sc.remove("returned_element_count");
    sc.remove("total_element_count");
    assert_eq!(refused(&removed), "screenshot_unverified");
    let mut bare = capture();
    bare.as_object_mut()
        .expect("object")
        .remove("structuredContent");
    assert_eq!(refused(&bare), "redaction_failed");
}

#[test]
fn bound_refuses_unproven_capture() {
    let mut invalid = capture();
    invalid["structuredContent"]["screenshot_frame_valid"] = json!(false);
    let mut errored = capture();
    errored["structuredContent"]["screenshot_error"] = json!({"code": "px_frame_mismatch"});
    let mut two = capture();
    let part = two["content"][1].clone();
    two["content"].as_array_mut().expect("content").push(part);
    let mut none = capture();
    none["content"].as_array_mut().expect("content").truncate(1);
    let mut gif = capture();
    gif["content"][1]["mimeType"] = json!("image/gif");
    let mut resource = capture();
    resource["content"][1] = json!({"type": "resource", "data": FAKE_PNG, "mimeType": "image/png"});
    for snap in [invalid, errored, two, none, gif, resource] {
        assert_eq!(refused(&snap), "screenshot_unavailable", "{snap}");
    }
}

#[test]
fn bound_refuses_other_window() {
    let snap = with_capture(window_state(0), &json!({"pid": 100, "window_id": 8}), false);
    assert_eq!(refused(&snap), "window_mismatch");
}

#[test]
fn bound_refuses_oversized_image() {
    let mut snap = capture();
    snap["content"][1]["data"] = json!("A".repeat(MAX_IMAGE_BASE64 + 1));
    assert_eq!(refused(&snap), "screenshot_too_large");
}

#[test]
fn read_results_lose_image_parts() {
    let mut snap = capture();
    strip_images(&mut snap);
    let parts = snap["content"].as_array().expect("content");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["type"], "text");
}

#[test]
fn screenshot_rule_is_filter_and_not_default() {
    let rule = rule(SCREENSHOT_TOOL).expect("a broker row");
    assert_eq!(rule.verdict, Verdict::Filter);
    assert!(!DEFAULT_TOOLS.contains(&SCREENSHOT_TOOL));
    let scope = Scope {
        pids: BTreeSet::from([100]),
        bundle_ids: BTreeSet::new(),
    };
    let code = |args: Value| {
        check_call(SCREENSHOT_TOOL, &args, &scope)
            .err()
            .map(|e| e.code)
    };
    assert_eq!(code(json!({"pid": 100, "window_id": 7})), None);
    assert_eq!(code(json!({"pid": 100})), Some("window_required"));
    assert_eq!(
        code(json!({"pid": 100, "window_id": "7"})),
        Some("window_required")
    );
    assert_eq!(
        code(json!({"pid": 101, "window_id": 7})),
        Some("pid_out_of_scope")
    );
    for extra in [
        "screenshot_out_file",
        "query",
        "max_elements",
        "include_screenshot",
    ] {
        let args = json!({"pid": 100, "window_id": 7, extra: 1});
        assert_eq!(code(args), Some("screenshot_arg"), "{extra}");
    }
    let built = driver_args(
        json!({"pid": 100, "window_id": 7})
            .as_object()
            .expect("object"),
    );
    assert_eq!(
        built,
        json!({"pid": 100, "window_id": 7, "include_screenshot": true})
    );
}

#[test]
fn screenshot_result_is_an_image_part() {
    let result = json!({"window_id": 7, "pid": 100, "mime_type": "image/png",
        "width": 1, "height": 1, "image": FAKE_PNG});
    let out = image_result(result);
    assert_eq!(out["isError"], false);
    assert_eq!(
        out["content"][0],
        json!({"type": "image", "data": FAKE_PNG, "mimeType": "image/png"})
    );
    assert_eq!(out["content"][1]["type"], "text");
    assert!(out["structuredContent"].get("image").is_none());
    let replay = image_result(json!({"replayed_image": "note"}));
    assert_eq!(replay["content"].as_array().map(Vec::len), Some(1));
}

/// The daemon arguments of each `get_window_state` call, `session` removed.
fn snapshot_args(rig: &Rig) -> Vec<Value> {
    rig.daemon(|d| d.calls())
        .into_iter()
        .filter(|(name, _)| name == "get_window_state")
        .map(|(_, mut args)| {
            args.as_object_mut().expect("args").remove("session");
            args
        })
        .collect()
}

fn shot_rig() -> Rig {
    Rig::with(&[SCREENSHOT_TOOL], WINDOW)
}

#[test]
fn broker_screenshot_returns_image_of_granted_window() {
    let mut rig = shot_rig();
    let reply = rig.call(
        SCREENSHOT_TOOL,
        "s1",
        json!({"pid": pid(), "window_id": WINDOW}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["image"], FAKE_PNG);
    assert!(reply["result"].get("structuredContent").is_none());
    assert!(!reply.to_string().contains("AXMenuBar"));
    assert_eq!(
        snapshot_args(&rig),
        [json!({"pid": pid(), "window_id": WINDOW, "include_screenshot": true})]
    );
    let audit = std::fs::read_to_string(rig.audit_path()).expect("audit");
    assert!(!audit.contains(FAKE_PNG), "the audit log holds no pixels");
    // A retry answers from the journal, which keeps no pixels.
    let again = rig.call(
        SCREENSHOT_TOOL,
        "s1",
        json!({"pid": pid(), "window_id": WINDOW}),
    );
    assert_eq!(again["ok"], true, "{again}");
    assert!(again["result"]["replayed_image"].is_string(), "{again}");
    assert!(!again.to_string().contains(FAKE_PNG));
    assert_eq!(
        snapshot_args(&rig).len(),
        1,
        "a replay never reaches the daemon"
    );
}

#[test]
fn screenshot_needs_its_own_grant() {
    let mut rig = Rig::new();
    let reply = rig.call(
        SCREENSHOT_TOOL,
        "s1",
        json!({"pid": pid(), "window_id": WINDOW}),
    );
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    assert!(snapshot_args(&rig).is_empty());
}

#[test]
fn screenshot_of_other_window_is_refused() {
    let mut rig = shot_rig();
    let reply = rig.call(
        SCREENSHOT_TOOL,
        "s1",
        json!({"pid": pid(), "window_id": WINDOW + 1}),
    );
    assert_eq!(reply["code"], "window_out_of_scope", "{reply}");
    let reply = rig.call(SCREENSHOT_TOOL, "s2", json!({"pid": pid()}));
    assert_eq!(reply["code"], "window_required", "{reply}");
    assert!(snapshot_args(&rig).is_empty());
}

#[test]
fn screenshot_refusals_carry_no_pixels() {
    for (mode, code) in [
        ("secure", "sensitive_content"),
        ("degraded", "screenshot_unverified"),
        ("refused", "driver_refused"),
        ("text_only", "redaction_failed"),
    ] {
        let mut rig = shot_rig();
        rig.daemon(|d| d.set_snapshot_mode(mode));
        let reply = rig.call(
            SCREENSHOT_TOOL,
            "s1",
            json!({"pid": pid(), "window_id": WINDOW}),
        );
        assert_eq!(reply["code"], code, "{mode}: {reply}");
        let text = reply.to_string();
        assert!(
            !text.contains(FAKE_PNG) && !text.contains("Shut Down"),
            "{mode}: {text}"
        );
    }
}

#[test]
fn read_tool_asks_for_no_image() {
    let mut rig = Rig::new();
    let snap = rig.snap("r1");
    assert!(!snap.to_string().contains(FAKE_PNG), "{snap}");
    assert_eq!(snapshot_args(&rig)[0]["include_screenshot"], false);
}

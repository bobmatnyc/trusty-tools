//! Port of the spike's `tests/test_policy.py`: 28 cases, one `#[test]` each,
//! with the same inputs. `invoke_menu_is_denied` and
//! `verdict_counts_match_the_plan` are added for the plan's one change.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use trusty_computer::policy::{RULES, Scope, Verdict, check_call, rule};

const INVENTORY: &str = include_str!("fixtures/evidence/list-tools-0.23.2.txt");
const BRIEF_DENIED: [&str; 5] = [
    "kill_app",
    "clipboard_read",
    "bring_to_front",
    "set_config",
    "replay_trajectory",
];

fn scope() -> Scope {
    Scope {
        pids: BTreeSet::from([100]),
        bundle_ids: BTreeSet::from(["org.example.fixture".to_owned()]),
    }
}

fn code(tool: &str, args: Value) -> &'static str {
    match check_call(tool, &args, &scope()) {
        Ok(rule) => panic!("{tool} {args} admitted as {:?}", rule.verdict),
        Err(e) => e.code,
    }
}

#[test]
fn every_inventory_tool_is_classified() {
    let tools: BTreeSet<&str> = INVENTORY
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split(':').next())
        .collect();
    assert_eq!(tools.len(), 56);
    let table: BTreeSet<&str> = RULES.iter().map(|r| r.tool).collect();
    assert_eq!(
        tools,
        table,
        "{:?}",
        tools.symmetric_difference(&table).collect::<Vec<_>>()
    );
}

#[test]
fn brief_named_tools_are_denied() {
    for tool in BRIEF_DENIED {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Deny));
        assert_eq!(code(tool, json!({"pid": 100})), "denied_tool");
    }
}

#[test]
fn unknown_tool_is_denied() {
    assert_eq!(code("some_future_tool", json!({})), "unknown_tool");
}

#[test]
fn invoke_menu_is_denied() {
    assert_eq!(rule("invoke_menu").map(|r| r.verdict), Some(Verdict::Deny));
    assert_eq!(code("invoke_menu", json!({"pid": 100})), "denied_tool");
}

#[test]
fn verdict_counts_match_the_plan() {
    let count = |v: Verdict| RULES.iter().filter(|r| r.verdict == v).count();
    assert_eq!(
        (
            count(Verdict::Allow),
            count(Verdict::Filter),
            count(Verdict::Deny)
        ),
        (6, 13, 37)
    );
}

#[test]
fn non_object_args_are_refused() {
    assert_eq!(code("click", json!([1])), "bad_args");
}

/// One `test_filter_refuses` parametrize row.
macro_rules! refuses {
    ($name:ident, $tool:expr, $args:tt, $code:expr) => {
        #[test]
        fn $name() {
            assert_eq!(code($tool, json!($args)), $code);
        }
    };
}

refuses!(filter_refuses_click_pid_out_of_scope, "click", {"pid": 999, "element_token": "t"}, "pid_out_of_scope");
refuses!(filter_refuses_click_pid_required, "click", {"element_token": "t"}, "pid_required");
refuses!(filter_refuses_click_pixel_only, "click", {"pid": 100, "x": 1, "y": 2}, "pixel_address");
refuses!(filter_refuses_click_token_and_pixel, "click", {"pid": 100, "element_token": "t", "x": 1}, "pixel_address");
refuses!(filter_refuses_click_foreground, "click",
    {"pid": 100, "element_token": "t", "delivery_mode": "foreground"}, "foreground_delivery");
refuses!(filter_refuses_click_desktop_scope, "click",
    {"pid": 100, "element_token": "t", "scope": "desktop"}, "desktop_scope");
refuses!(filter_refuses_click_modifier, "click",
    {"pid": 100, "element_token": "t", "modifier": ["cmd"]}, "modifier");
refuses!(filter_refuses_click_debug_image, "click",
    {"pid": 100, "element_token": "t", "debug_image_out": "/tmp/a.png"}, "path_write");
refuses!(filter_refuses_type_text_too_long, "type_text",
    {"pid": 100, "element_token": "t", "text": "x".repeat(2001)}, "text_too_long");
refuses!(filter_refuses_hotkey_cmd_q, "hotkey", {"pid": 100, "keys": ["CMD", "q"]}, "denied_chord");
refuses!(filter_refuses_press_key_cmd_tab, "press_key",
    {"pid": 100, "key": "tab", "modifiers": ["cmd"]}, "denied_chord");
refuses!(filter_refuses_window_state_screenshot, "get_window_state",
    {"pid": 100, "window_id": 1, "include_screenshot": true}, "screenshot");
refuses!(filter_refuses_window_state_out_file, "get_window_state",
    {"pid": 100, "window_id": 1, "screenshot_out_file": "/tmp/s.png"}, "path_write");
refuses!(filter_refuses_launch_out_of_scope, "launch_app", {"bundle_id": "com.apple.Terminal"}, "app_out_of_scope");
refuses!(filter_refuses_launch_urls, "launch_app",
    {"bundle_id": "org.example.fixture", "urls": ["file:///etc"]}, "launch_arg");
refuses!(filter_refuses_check_permissions_prompt, "check_permissions", {"prompt": true}, "tcc_prompt");
refuses!(
    filter_refuses_list_windows_no_pid,
    "list_windows",
    {},
    "pid_required"
);

/// One `test_filter_admits` parametrize row.
macro_rules! admits {
    ($name:ident, $tool:expr, $args:tt) => {
        #[test]
        fn $name() {
            let rule = check_call($tool, &json!($args), &scope()).expect("admitted");
            assert_ne!(rule.verdict, Verdict::Deny);
        }
    };
}

admits!(filter_admits_click_token, "click", {"pid": 100, "element_token": "t"});
admits!(filter_admits_click_index, "click",
    {"pid": 100, "element_index": 3, "snapshot_id": "s1", "delivery_mode": "background"});
admits!(filter_admits_type_text, "type_text", {"pid": 100, "element_token": "t", "text": "hello"});
admits!(filter_admits_hotkey_cmd_a, "hotkey", {"pid": 100, "keys": ["cmd", "a"]});
admits!(filter_admits_window_state, "get_window_state", {"pid": 100, "window_id": 1});
admits!(filter_admits_launch_in_scope, "launch_app", {"bundle_id": "org.example.fixture"});
admits!(filter_admits_check_permissions, "check_permissions", {});
admits!(filter_admits_list_apps, "list_apps", {});

/// #11460 fix round: aliases, key order and supersets of a denied chord are refused.
#[test]
fn chord_aliases_order_and_supersets_are_denied() {
    let denied: [&[&str]; 9] = [
        &["command", "q"],
        &["cmd", "q", "shift"],
        &["cmd", "shift", "q"],
        &["q", "META"],
        &["cmd", "option", "esc"],
        &["command", "alt", "escape"],
        &["esc", "opt", "cmd"],
        &["\u{2318}", "\u{2325}", "escape"],
        &["control", "command", "q"],
    ];
    for keys in denied {
        let err = check_call("hotkey", &json!({"pid": 100, "keys": keys}), &scope())
            .expect_err("a denied chord is refused");
        assert_eq!(err.code, "denied_chord", "{keys:?}");
    }
    let key = json!({"pid": 100, "key": "Q", "modifiers": ["Command", "Shift"]});
    let err = check_call("press_key", &key, &scope()).expect_err("press_key superset");
    assert_eq!(err.code, "denied_chord");
    let ok = json!({"pid": 100, "keys": ["command", "shift", "a"]});
    check_call("hotkey", &ok, &scope()).expect("cmd+shift+a is not denied");
}

/// #11460 fix round: a call may not name a token and a snapshot index at once.
#[test]
fn token_and_snapshot_index_together_are_refused() {
    for extra in [
        json!({"snapshot_id": "s1", "element_index": 3}),
        json!({"snapshot_id": "s1"}),
        json!({"element_index": 3}),
    ] {
        let mut args = json!({"pid": 100, "element_token": "t"});
        for (k, v) in extra.as_object().expect("object") {
            args[k] = v.clone();
        }
        let err = check_call("click", &args, &scope()).expect_err("dual address");
        assert_eq!(err.code, "dual_address", "{args}");
    }
}

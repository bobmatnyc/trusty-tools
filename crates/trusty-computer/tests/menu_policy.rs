//! Menu invocation under a path deny list (issue #11695).
//!
//! Authority check: menu invocation is limited to the granted app, and the
//! deny list is enforced. `menu_outside_grant_refused` and
//! `menu_denied_path_refused` prove each half against the broker on the fake
//! daemon: the call is refused and never reaches the driver.
//! `menu_localized_label_refused` and `menu_paths_refuse_first_two_menus_by_position`
//! prove the snapshot allowlist catches a label the deny list does not know.

mod common;

use std::collections::BTreeSet;

use common::rig::{Rig, WINDOW, pid};
use serde_json::{Value, json};
use trusty_computer::menu::{self, denied, menu_paths};
use trusty_computer::policy::{Scope, Verdict, check_call, rule};

fn scope() -> Scope {
    Scope {
        pids: BTreeSet::from([100]),
        bundle_ids: BTreeSet::new(),
    }
}

fn code(args: Value) -> &'static str {
    match check_call("invoke_menu", &args, &scope()) {
        Ok(_) => panic!("{args} admitted"),
        Err(e) => e.code,
    }
}

fn menu_call(path: Value) -> Value {
    json!({"pid": pid(), "window_id": WINDOW, "path": path})
}

#[test]
fn invoke_menu_is_filter_and_not_default() {
    assert_eq!(
        rule("invoke_menu").map(|r| r.verdict),
        Some(Verdict::Filter)
    );
    assert!(!trusty_computer::grants::DEFAULT_TOOLS.contains(&"invoke_menu"));
    // #11695: activation inside a menu call does not lift the standalone deny.
    assert_eq!(
        rule("bring_to_front").map(|r| r.verdict),
        Some(Verdict::Deny)
    );
    let ok = json!({"pid": 100, "window_id": 7, "path": ["File", "New Document"]});
    assert!(check_call("invoke_menu", &ok, &scope()).is_ok());
}

#[test]
fn denied_labels_match_prefix_case_and_ellipsis() {
    for label in [
        "Quit",
        "Quit Safari",
        "  close window ",
        "Close All",
        "Log Out Bob\u{2026}",
        "Shut Down...",
        "Restart\u{2026}",
        "Force Quit\u{2026}",
        "Services",
        "Open With",
        "Share\u{2026}",
        "System Settings\u{2026}",
        "System Preferences...",
        "Apple",
        "\u{f8ff}",
        "Recent Items",
        "SHUT\u{a0}DOWN",
        "Hide Others",
        "Show All",
        "Move to Trash",
        "Print\u{2026}",
        "Export As\u{2026}",
        "Save As\u{2026}",
    ] {
        assert!(denied(label), "{label:?} admitted");
    }
    for label in ["Quitter", "Closed Captions", "Shared", "New Window", "Save"] {
        assert!(!denied(label), "{label:?} refused");
    }
}

#[test]
fn menu_path_shape_is_checked() {
    let long = "x".repeat(menu::MAX_LABEL_CHARS + 1);
    let deep: Vec<&str> = vec!["Edit"; menu::MAX_PATH_SEGMENTS + 1];
    for args in [
        json!({"pid": 100, "window_id": 7}),
        json!({"pid": 100, "window_id": 7, "path": "File"}),
        json!({"pid": 100, "window_id": 7, "path": []}),
        json!({"pid": 100, "window_id": 7, "path": ["File"]}),
        json!({"pid": 100, "window_id": 7, "path": ["File", 3]}),
        json!({"pid": 100, "window_id": 7, "path": ["File", " "]}),
        json!({"pid": 100, "window_id": 7, "path": [long]}),
        json!({"pid": 100, "window_id": 7, "path": deep}),
        json!({"pid": 100, "path": ["File", "New"]}),
        json!({"pid": 100, "window_id": 7, "path": ["File", "New"], "session": "s"}),
    ] {
        assert_eq!(code(args.clone()), "bad_menu_path", "{args}");
    }
    assert_eq!(
        code(json!({"window_id": 7, "path": ["File", "New"]})),
        "pid_required"
    );
}

/// Authority check: the deny list is enforced at any depth, before the driver.
#[test]
fn menu_denied_path_refused() {
    for path in [
        json!(["Fixture", "Quit Fixture"]),
        json!(["File", "Close Window"]),
        json!(["Apple", "Shut Down\u{2026}"]),
        json!(["Fixture", "Services", "New Terminal at Folder"]),
        json!(["File", "Share", "Mail"]),
        json!(["File", "Open With", "TextEdit"]),
        json!(["Apple", "System Settings\u{2026}"]),
    ] {
        assert_eq!(
            code(json!({"pid": 100, "window_id": 7, "path": path})),
            "denied_menu_path"
        );
    }
    let mut rig = Rig::with(&["invoke_menu"], WINDOW);
    let reply = rig.call(
        "invoke_menu",
        "r1",
        menu_call(json!(["Fixture", "Quit Fixture"])),
    );
    assert_eq!(reply["code"], "denied_menu_path", "{reply}");
    assert!(rig.agent_calls().is_empty());
    let line = rig.audit().last().cloned().expect("audit line");
    assert_eq!(line["refused"], "denied_menu_path");
    assert_eq!(line["menu_path"], json!(["Fixture", "Quit Fixture"]));
}

/// Authority check: the menu of an app or window outside the grant is refused.
#[test]
fn menu_outside_grant_refused() {
    let mut rig = Rig::with(&["invoke_menu"], WINDOW);
    let other_pid = json!({"pid": 1, "window_id": WINDOW, "path": ["File", "New"]});
    assert_eq!(
        rig.call("invoke_menu", "r1", other_pid)["code"],
        "pid_out_of_scope"
    );
    let other_window = json!({"pid": pid(), "window_id": WINDOW + 1, "path": ["File", "New"]});
    assert_eq!(
        rig.call("invoke_menu", "r2", other_window)["code"],
        "window_out_of_scope"
    );
    assert!(rig.agent_calls().is_empty());
    // A grant that does not name invoke_menu cannot invoke a menu at all.
    let mut plain = Rig::new();
    let reply = plain.call("invoke_menu", "r3", menu_call(json!(["File", "New"])));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    assert!(plain.agent_calls().is_empty());
}

/// A rig granting `invoke_menu` whose fake shows the `menus` snapshot, read once.
fn menu_rig() -> Rig {
    let mut rig = Rig::with(&["invoke_menu"], WINDOW);
    rig.daemon(|d| d.set_snapshot_mode("menus"));
    rig.snap("snap");
    rig
}

/// HIGH from code-critic: a localized "Dienste" (Services) is not on the
/// English deny list, but it is in the app menu, so no snapshot lists it.
#[test]
fn menu_localized_label_refused() {
    let mut rig = menu_rig();
    let before = rig.agent_calls().len();
    let reply = rig.call(
        "invoke_menu",
        "r1",
        menu_call(json!(["Fixture", "Dienste"])),
    );
    assert_eq!(reply["code"], "menu_path_not_exposed", "{reply}");
    let unlisted = rig.call("invoke_menu", "r2", menu_call(json!(["File", "Open"])));
    assert_eq!(unlisted["code"], "menu_path_not_exposed", "{unlisted}");
    assert_eq!(
        rig.agent_calls().len(),
        before,
        "a refused path reached the driver"
    );
    let line = rig.audit().last().cloned().expect("audit line");
    assert_eq!(line["refused"], "menu_path_not_exposed");
}

#[test]
fn menu_call_needs_a_prior_snapshot() {
    let mut rig = Rig::with(&["invoke_menu"], WINDOW);
    let reply = rig.call("invoke_menu", "r1", menu_call(json!(["File", "New"])));
    assert_eq!(reply["code"], "menu_path_not_exposed", "{reply}");
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn granted_menu_call_reaches_driver_and_is_audited() {
    let mut rig = menu_rig();
    let reply = rig.call("invoke_menu", "r1", menu_call(json!(["File", "New"])));
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["confirmation"].is_object(), "{reply}");
    assert!(rig.agent_calls().contains(&"invoke_menu".to_owned()));
    let lines = rig.audit();
    let forward = lines
        .iter()
        .find(|l| l["tool"] == "invoke_menu" && l["phase"] == "forward")
        .expect("forward line");
    assert_eq!(forward["menu_path"], json!(["File", "New"]));
    assert_eq!(forward["activates_app"], true);
}

#[test]
fn read_carries_menu_only_under_an_invoke_menu_grant() {
    let mut rig = Rig::with(&["invoke_menu"], WINDOW);
    rig.daemon(|d| d.set_snapshot_mode("menus"));
    let snap = rig.snap("snap");
    let sc = &snap["structuredContent"];
    // The Apple menu and the app menu are refused by position.
    assert_eq!(
        sc["menu"],
        json!({"paths": [["File", "New"]], "truncated": false}),
        "{snap}"
    );
    let text = snap.to_string();
    for word in ["Shut Down", "Recent Items", "tok-shut", "Dienste"] {
        assert!(!text.contains(word), "{word} in {text}");
    }
    let mut plain = Rig::new();
    let snap = plain.snap("snap");
    let sc = snap["structuredContent"].as_object().expect("structured");
    assert!(!sc.contains_key("menu"), "{snap}");
}

fn el(i: i64, role: &str, label: Option<&str>, parent: Option<i64>) -> Value {
    json!({"element_index": i, "role": role, "label": label, "parent_index": parent})
}

fn app_with_menus() -> Value {
    let elements = vec![
        el(0, "AXWindow", Some("doc"), None),
        el(1, "AXButton", Some("Quit Later"), Some(0)),
        el(2, "AXMenuBar", None, None),
        el(3, "AXMenuBarItem", Some("Apfel"), Some(2)),
        el(4, "AXMenu", None, Some(3)),
        el(5, "AXMenuItem", Some("Neustart"), Some(4)),
        el(6, "AXMenuBarItem", Some("Fixture"), Some(2)),
        el(7, "AXMenu", None, Some(6)),
        el(8, "AXMenuItem", Some("About Fixture"), Some(7)),
        el(9, "AXMenuItem", Some("Services"), Some(7)),
        el(10, "AXMenu", None, Some(9)),
        el(11, "AXMenuItem", Some("Send to Mail"), Some(10)),
        el(12, "AXMenuItem", Some("Quit Fixture"), Some(7)),
        el(13, "AXMenuBarItem", Some("File"), Some(2)),
        el(14, "AXMenu", None, Some(13)),
        el(15, "AXMenuItem", Some("New"), Some(14)),
        el(16, "AXMenuItem", Some(""), Some(14)),
        el(17, "AXMenuItem", Some("Close"), Some(14)),
        el(18, "AXMenuItem", Some("Export"), Some(14)),
        el(23, "AXMenuItem", Some("Duplicate"), Some(14)),
        el(19, "AXMenu", None, Some(18)),
        el(20, "AXMenuItem", Some("PDF\u{2026}"), Some(19)),
        // A parent cycle is skipped, never followed forever.
        el(21, "AXMenuItem", Some("Loop"), Some(22)),
        el(22, "AXMenuItem", Some("Loop 2"), Some(21)),
    ];
    json!({"structuredContent": {"elements": elements}})
}

#[test]
fn menu_paths_drop_denied_and_apple_menu() {
    let listed = menu_paths(&app_with_menus());
    assert_eq!(
        listed,
        json!({"paths": [["File", "New"], ["File", "Duplicate"]], "truncated": false})
    );
    for path in listed["paths"].as_array().expect("paths") {
        let args = json!({"pid": 100, "window_id": 7, "path": path});
        assert!(check_call("invoke_menu", &args, &scope()).is_ok(), "{path}");
    }
}

#[test]
fn menu_paths_ignore_window_elements() {
    let window_only = json!({"structuredContent": {"elements": [
        el(0, "AXWindow", Some("doc"), None),
        el(1, "AXMenuItem", Some("Context Item"), Some(0)),
    ]}});
    assert_eq!(menu_paths(&window_only)["paths"], json!([]));
    assert_eq!(menu_paths(&json!({}))["paths"], json!([]));
}

/// The first two menu-bar items are refused whatever their labels say.
#[test]
fn menu_paths_refuse_first_two_menus_by_position() {
    let elements = vec![
        el(0, "AXMenuBar", None, None),
        el(1, "AXMenuBarItem", Some("Pomme"), Some(0)),
        el(2, "AXMenuItem", Some("Redemarrer"), Some(1)),
        el(3, "AXMenuBarItem", Some("Vorschau"), Some(0)),
        el(4, "AXMenuItem", Some("Dienste"), Some(3)),
        el(5, "AXMenuItem", Some("Vorschau beenden"), Some(3)),
        el(6, "AXMenuBarItem", Some("Ablage"), Some(0)),
        el(7, "AXMenuItem", Some("Neu"), Some(6)),
    ];
    let listed = menu_paths(&json!({"structuredContent": {"elements": elements}}));
    assert_eq!(listed["paths"], json!([["Ablage", "Neu"]]), "{listed}");
}

/// Authority check: the broker's scope gate is the authority boundary (#11745).
///
/// Why: cua-driver 0.23.2's `invoke_menu` activates the target app before
/// checking menu path availability. The broker checks pid/window scope BEFORE
/// the driver call, so out-of-scope invocations never reach the driver.
///
/// What: Proves that `invoke_menu` with an out-of-scope pid or window_id is
/// refused at the broker level with `pid_out_of_scope` or `window_out_of_scope`,
/// and the driver is never called.
///
/// Test: `broker_scope_gate_is_authority_boundary_for_invoke_menu`
#[test]
fn broker_scope_gate_is_authority_boundary_for_invoke_menu() {
    // Set up: grant with invoke_menu permission for a specific pid and window.
    let mut rig = menu_rig();
    // #11745: baseline before any out-of-scope call, so both refusals are covered.
    let calls_before = rig.agent_calls().len();

    // Out-of-scope pid: broker refuses with pid_out_of_scope, driver never called.
    let wrong_pid = json!({"pid": pid() + 1, "window_id": WINDOW, "path": ["File", "New"]});
    let reply = rig.call("invoke_menu", "r1", wrong_pid);
    assert_eq!(
        reply["code"], "pid_out_of_scope",
        "out-of-scope pid should be refused at broker: {reply}"
    );

    // Out-of-scope window_id: broker refuses with window_out_of_scope, driver never called.
    let wrong_window = json!({"pid": pid(), "window_id": WINDOW + 1, "path": ["File", "New"]});
    let reply = rig.call("invoke_menu", "r2", wrong_window);
    assert_eq!(
        reply["code"], "window_out_of_scope",
        "out-of-scope window should be refused at broker: {reply}"
    );

    // Verify: driver was never invoked for either out-of-scope call.
    assert_eq!(
        rig.agent_calls().len(),
        calls_before,
        "driver should never be called for out-of-scope invoke_menu"
    );

    // Audit: both refusals are recorded.
    let audit = rig.audit();
    let pid_refused = audit.iter().any(|l| {
        l["tool"] == "invoke_menu"
            && l["refused"] == "pid_out_of_scope"
            && l["activates_app"] == true
    });
    let window_refused = audit.iter().any(|l| {
        l["tool"] == "invoke_menu"
            && l["refused"] == "window_out_of_scope"
            && l["activates_app"] == true
    });
    assert!(
        pid_refused && window_refused,
        "both refusals should be audited with activates_app flag"
    );
}

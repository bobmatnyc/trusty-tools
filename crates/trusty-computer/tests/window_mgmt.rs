//! Focus, move and resize of the granted window (issue #11696 Phase B): the
//! argument checks, the grant flag, the owner read, and the broker pipeline
//! against the fake daemon, including the refusal of every window that is not
//! the granted pid's before any mutating driver call.

mod common;

use common::rig::{Rig, WINDOW, consent, grant_req, pid};
use serde_json::{Value, json};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::policy::{Scope, Verdict, check_call, rule};
use trusty_computer::window_mgmt::{
    Bounds, FOCUS_TOOL, GRANT_FLAG, MOVE_TOOL, RESIZE_TOOL, TOOLS, owned_bounds,
};

/// The daemon tools that change a window; none may run on a refused call.
const MUTATING: [&str; 2] = ["bring_to_front", "set_window_frame"];

fn scope() -> Scope {
    Scope {
        pids: [100].into_iter().collect(),
        ..Scope::default()
    }
}

fn code(tool: &str, args: Value) -> Option<&'static str> {
    check_call(tool, &args, &scope()).err().map(|e| e.code)
}

#[test]
fn window_args_are_checked() {
    assert_eq!(code(FOCUS_TOOL, json!({"pid": 100, "window_id": 7})), None);
    let mv = json!({"pid": 100, "window_id": 7, "x": -200.5, "y": 40});
    assert_eq!(code(MOVE_TOOL, mv), None);
    let rs = json!({"pid": 100, "window_id": 7, "width": 640, "height": 1});
    assert_eq!(code(RESIZE_TOOL, rs), None);
    for (tool, args, want) in [
        (
            FOCUS_TOOL,
            json!({"pid": 101, "window_id": 7}),
            "pid_out_of_scope",
        ),
        (
            MOVE_TOOL,
            json!({"window_id": 7, "x": 1, "y": 1}),
            "pid_required",
        ),
        (FOCUS_TOOL, json!({"pid": 100}), "window_required"),
        (
            FOCUS_TOOL,
            json!({"pid": 100, "window_id": "7"}),
            "window_required",
        ),
        (
            FOCUS_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1}),
            "window_arg",
        ),
        (
            MOVE_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1, "y": 1, "width": 9}),
            "window_arg",
        ),
        (
            MOVE_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1, "y": 1, "scope": "desktop"}),
            "window_arg",
        ),
        (
            MOVE_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1}),
            "bad_window_frame",
        ),
        (
            MOVE_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1e9, "y": 1}),
            "bad_window_frame",
        ),
        (
            RESIZE_TOOL,
            json!({"pid": 100, "window_id": 7, "width": 0, "height": 10}),
            "bad_window_frame",
        ),
        (
            RESIZE_TOOL,
            json!({"pid": 100, "window_id": 7, "width": 10, "height": 20000}),
            "bad_window_frame",
        ),
        (
            RESIZE_TOOL,
            json!({"pid": 100, "window_id": 7, "width": "10", "height": 10}),
            "bad_window_frame",
        ),
    ] {
        assert_eq!(code(tool, args.clone()), Some(want), "{tool} {args}");
    }
    for tool in TOOLS {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Filter));
    }
    // The driver's own window tools stay DENY; only the scoped broker tools reach them.
    for tool in MUTATING {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Deny), "{tool}");
    }
}

/// A `list_windows` result with the given records.
fn listing(windows: Value) -> Value {
    json!({"structuredContent": {"windows": windows}})
}

fn record(pid: u32, window_id: u64) -> Value {
    json!({"pid": pid, "window_id": window_id,
        "bounds": {"x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0}})
}

#[test]
fn owner_read_proves_the_window_is_the_granted_pids() {
    let want = Bounds {
        x: 10.0,
        y: 20.0,
        width: 300.0,
        height: 200.0,
    };
    let ok = listing(json!([record(100, 6), record(100, 7)]));
    assert_eq!(owned_bounds(&ok, 100, 7), Ok(want));
    for bad in [
        listing(json!([record(101, 7)])),
        listing(json!([record(100, 6)])),
        listing(json!([record(100, 7), record(101, 7)])),
        listing(json!([])),
    ] {
        let code = owned_bounds(&bad, 100, 7).err().map(|(c, _)| c);
        assert_eq!(code, Some("window_not_in_granted_app"), "{bad}");
    }
    let mut nan = record(100, 7);
    nan["bounds"]["width"] = json!("wide");
    for bad in [listing(json!([nan])), json!({"content": []})] {
        let code = owned_bounds(&bad, 100, 7).err().map(|(c, _)| c);
        assert_eq!(code, Some("window_frame_unavailable"), "{bad}");
    }
}

/// The daemon calls named `name`, each without the broker's `session` label.
fn driver_calls(rig: &Rig, name: &str) -> Vec<Value> {
    rig.daemon(|d| d.calls())
        .into_iter()
        .filter(|(n, _)| n == name)
        .map(|(_, mut args)| {
            args.as_object_mut().expect("args").remove("session");
            args
        })
        .collect()
}

fn mutating_calls(rig: &Rig) -> usize {
    MUTATING.iter().map(|t| driver_calls(rig, t).len()).sum()
}

fn forwards(rig: &Rig) -> Vec<Value> {
    rig.audit()
        .into_iter()
        .filter(|r| r["phase"] == "forward")
        .collect()
}

/// A rig granted `window_management` whose daemon lists `windows`.
fn window_rig(windows: Value) -> Rig {
    let rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.daemon(|d| d.set_windows(windows));
    rig
}

fn granted_listing() -> Value {
    json!([record(pid(), WINDOW), record(pid(), WINDOW + 1)])
}

#[test]
fn window_tools_need_the_grant_flag() {
    assert!(!DEFAULT_TOOLS.contains(&GRANT_FLAG));
    let mut rig = Rig::new();
    rig.daemon(|d| d.set_windows(granted_listing()));
    let reply = rig.call(FOCUS_TOOL, "f1", json!({"pid": pid(), "window_id": WINDOW}));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    // No window tool is grantable by its own name; only the flag grants them.
    for tool in TOOLS {
        let reply = consent(
            &rig.server.operator,
            &rig.dialog,
            &grant_req(&[tool], WINDOW),
        );
        assert_eq!(reply["code"], "bad_tool", "{tool}: {reply}");
    }
    assert_eq!(mutating_calls(&rig), 0);
}

#[test]
fn window_tools_drive_only_the_granted_window() {
    let mut rig = window_rig(granted_listing());
    let (p, w) = (pid(), WINDOW);
    let reply = rig.call(FOCUS_TOOL, "f1", json!({"pid": p, "window_id": w}));
    assert_eq!(reply["ok"], true, "{reply}");
    let reply = rig.call(
        MOVE_TOOL,
        "m1",
        json!({"pid": p, "window_id": w, "x": -40, "y": 60}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    let reply = rig.call(
        RESIZE_TOOL,
        "r1",
        json!({"pid": p, "window_id": w, "width": 640, "height": 480}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        driver_calls(&rig, "bring_to_front"),
        [json!({"pid": p, "window_id": w})]
    );
    assert_eq!(
        driver_calls(&rig, "set_window_frame"),
        [
            json!({"pid": p, "window_id": w, "x": -40, "y": 60, "width": 300.0, "height": 200.0}),
            json!({"pid": p, "window_id": w, "x": 10.0, "y": 20.0, "width": 640, "height": 480}),
        ]
    );
    // Each call read the granted pid's windows only, just before it ran.
    assert_eq!(
        driver_calls(&rig, "list_windows"),
        vec![json!({"pid": p}); 3]
    );
    let fwd = forwards(&rig);
    assert_eq!(fwd.len(), 3, "{fwd:?}");
    assert_eq!(
        fwd[0]["window_bounds"],
        json!({"x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0})
    );
    assert_eq!(fwd[0]["activates_app"], true);
    assert!(fwd[1].get("activates_app").is_none(), "{:?}", fwd[1]);
}

/// The pid-scoping refusal: a window that is not the granted pid's never
/// reaches a mutating driver call, and the static refusals reach no driver
/// call at all.
#[test]
fn window_of_another_pid_is_refused_before_the_driver() {
    let mut rig = window_rig(granted_listing());
    let (p, w) = (pid(), WINDOW);
    // Another pid, or another window of the granted pid: refused before any driver call.
    for (i, (tool, args, want)) in [
        (
            FOCUS_TOOL,
            json!({"pid": p + 1, "window_id": w}),
            "pid_out_of_scope",
        ),
        (
            MOVE_TOOL,
            json!({"pid": p, "window_id": w + 1, "x": 0, "y": 0}),
            "window_out_of_scope",
        ),
        (
            RESIZE_TOOL,
            json!({"pid": p, "window_id": w + 1, "width": 9, "height": 9}),
            "window_out_of_scope",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let reply = rig.call(tool, &format!("s{i}"), args);
        assert_eq!(reply["code"], want, "{tool}: {reply}");
    }
    assert!(driver_calls(&rig, "list_windows").is_empty());
    // The daemon lists the granted window id under another pid, or not at all.
    for (i, listed) in [json!([record(p + 1, w)]), json!([record(p, w + 1)])]
        .into_iter()
        .enumerate()
    {
        rig.daemon(|d| d.set_windows(listed));
        for (j, tool) in TOOLS.into_iter().enumerate() {
            let args = match tool {
                MOVE_TOOL => json!({"pid": p, "window_id": w, "x": 0, "y": 0}),
                RESIZE_TOOL => json!({"pid": p, "window_id": w, "width": 9, "height": 9}),
                _ => json!({"pid": p, "window_id": w}),
            };
            let reply = rig.call(tool, &format!("o{i}{j}"), args);
            assert_eq!(
                reply["code"], "window_not_in_granted_app",
                "{tool}: {reply}"
            );
        }
    }
    assert_eq!(mutating_calls(&rig), 0);
    assert!(
        forwards(&rig).is_empty(),
        "a refused window call is never forwarded"
    );
}

#[test]
fn owner_read_failure_refuses_the_call() {
    // No window list in the reply: the owner cannot be proven.
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    let reply = rig.call(FOCUS_TOOL, "f1", json!({"pid": pid(), "window_id": WINDOW}));
    assert_eq!(reply["code"], "window_frame_unavailable", "{reply}");
    assert_eq!(mutating_calls(&rig), 0);
}

//! Graceful quit of the granted app (issue #11696 Phase D): the argument
//! checks, the `app_quit` grant flag, the refusal of every other app or pid
//! before any quit is sent, and the broker pipeline on a fake quitter.

mod common;

use std::sync::atomic::Ordering;

use common::rig::{BUNDLE, Rig, WINDOW, consent, grant_req, pid};
use serde_json::{Value, json};

use trusty_computer::app_quit::{GRANT_FLAG, QUIT_TOOL};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::policy::{Scope, Verdict, check_call, rule};

fn scope() -> Scope {
    Scope {
        pids: [i64::from(pid())].into(),
        bundle_ids: [BUNDLE.to_owned()].into(),
    }
}

fn code(args: Value) -> Option<&'static str> {
    check_call(QUIT_TOOL, &args, &scope()).err().map(|e| e.code)
}

fn quit(rig: &mut Rig, rid: &str, bundle: &str) -> Value {
    rig.call(QUIT_TOOL, rid, json!({"pid": pid(), "bundle_id": bundle}))
}

#[test]
fn quit_args_are_checked() {
    assert_eq!(rule(QUIT_TOOL).expect("rule").verdict, Verdict::Filter);
    assert_eq!(code(json!({"pid": pid(), "bundle_id": BUNDLE})), None);
    for (args, want) in [
        (json!({"bundle_id": BUNDLE}), "pid_required"),
        (json!({"pid": 1, "bundle_id": BUNDLE}), "pid_out_of_scope"),
        (json!({"pid": pid()}), "app_out_of_scope"),
        (
            json!({"pid": pid(), "bundle_id": "com.apple.Safari"}),
            "app_out_of_scope",
        ),
        (json!({"pid": pid(), "bundle_id": 7}), "app_out_of_scope"),
        (
            json!({"pid": pid(), "bundle_id": BUNDLE, "force": true}),
            "quit_arg",
        ),
    ] {
        assert_eq!(code(args.clone()), Some(want), "{args}");
    }
    // The driver's kill -9 stays DENY.
    assert_eq!(rule("kill_app").expect("kill_app").verdict, Verdict::Deny);
}

/// Refusal tests: no flag, another app, another pid, `kill_app`, and the
/// tool's own name in a grant; no quit is sent for any of them.
#[test]
fn other_apps_are_refused_before_any_quit() {
    assert!(!DEFAULT_TOOLS.contains(&GRANT_FLAG));
    assert!(!DEFAULT_TOOLS.contains(&QUIT_TOOL));
    let mut bare = Rig::new();
    let reply = quit(&mut bare, "q0", BUNDLE);
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    let reply = quit(&mut rig, "q1", "com.apple.finder");
    assert_eq!(reply["code"], "app_out_of_scope", "{reply}");
    let reply = rig.call(QUIT_TOOL, "q2", json!({"pid": 1, "bundle_id": BUNDLE}));
    assert_eq!(reply["code"], "pid_out_of_scope", "{reply}");
    let reply = rig.call("kill_app", "q3", json!({"pid": pid()}));
    assert_eq!(reply["code"], "denied_tool", "{reply}");
    let reply = consent(
        &rig.server.operator,
        &rig.dialog,
        &grant_req(&[QUIT_TOOL], WINDOW),
    );
    assert_eq!(reply["code"], "bad_tool", "{reply}");
    assert!(bare.quitter.sent().is_empty());
    assert!(rig.quitter.sent().is_empty());
}

/// The pid no longer runs the granted app: the grant dies, no quit is sent.
#[test]
fn replaced_app_is_not_quit() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.resolver.set(pid(), "com.apple.Safari", "Safari");
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["code"], "app_changed", "{reply}");
    // The re-check revokes the grant; the audit line records it.
    let line = rig
        .audit()
        .into_iter()
        .rfind(|r| r["tool"] == QUIT_TOOL)
        .expect("audit line");
    assert_eq!(line["refused"], "app_changed", "{line}");
    assert_eq!(line["revoked"], true, "{line}");
    assert!(rig.quitter.sent().is_empty());
}

#[test]
fn quit_runs_only_for_the_granted_app() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        reply["result"]["structuredContent"],
        json!({"quit_requested": true, "pid": pid(), "bundle_id": BUNDLE})
    );
    assert_eq!(rig.quitter.sent(), [(pid(), BUNDLE.to_owned())]);
    // A retry of the same request is answered from the journal, not re-sent.
    let again = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(rig.quitter.sent().len(), 1);
    // The daemon is never asked to quit or kill anything.
    let daemon = rig.daemon(|d| d.calls());
    assert!(
        !daemon
            .iter()
            .any(|(n, _)| n == "kill_app" || n == QUIT_TOOL),
        "{daemon:?}"
    );
    let done = rig
        .audit()
        .into_iter()
        .rfind(|r| r["tool"] == QUIT_TOOL && r["phase"] == "done")
        .expect("done line");
    assert_eq!(done["verdict"], "FILTER");
    assert_eq!(done["quit_bundle_id"], BUNDLE);
}

#[test]
fn quitter_failure_is_reported() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.quitter.broken.store(true, Ordering::SeqCst);
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["code"], "quit_error", "{reply}");
}

//! #8433 P0: the egress guard's channel-taint arm.

use super::*;
use crate::commands::egress_guard::{Verdict, with_taint};
use serde_json::json;
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};

fn call(tool: &str, session: Option<&str>) -> Value {
    let mut payload = json!({
        "tool_name": tool,
        "tool_input": { "file_path": "/etc/hosts", "command": "ls" },
    });
    if let Some(session) = session {
        payload["session_id"] = json!(session);
    }
    payload
}

fn tainted_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: std::path::PathBuf::from("/"),
    };
    taint_managed_session(&taint_dir(home.path()), "m-1", "s-tainted", &mark).expect("taint");
    home
}

/// 🔴 #8433: every gated tool of a tainted session is denied.
#[test]
fn tainted_session_gated_call_denies() {
    let home = tainted_home();
    for tool in GATED_TOOLS {
        let reason = taint_verdict(&call(tool, Some("s-tainted")), Some(home.path()));
        assert!(
            reason.is_some_and(|r| r.contains("channel-tainted")),
            "{tool}"
        );
    }
}

/// 🔴 #8433 (critic MEDIUM): `MultiEdit` writes a path like `Edit`, so a
/// tainted session's `MultiEdit` is denied.
#[test]
fn tainted_session_multiedit_denies() {
    let home = tainted_home();
    let reason = taint_verdict(&call("MultiEdit", Some("s-tainted")), Some(home.path()));
    assert!(reason.is_some_and(|r| r.contains("channel-tainted")));
}

/// 🔴 #8433: an untainted session's gated call is not judged here.
#[test]
fn untainted_session_gated_call_is_not_judged() {
    let home = tainted_home();
    assert_eq!(
        taint_verdict(&call("Read", Some("s-clean")), Some(home.path())),
        None
    );
}

/// 🔴 #8433 (fail closed): a gated call naming no session denies.
#[test]
fn gated_call_without_session_id_denies() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(taint_verdict(&call("Read", None), Some(home.path())).is_some());
    let mut numeric = call("Read", None);
    numeric["session_id"] = json!(7);
    assert!(taint_verdict(&numeric, Some(home.path())).is_some());
}

/// 🔴 #8433 (fail closed): an unknown home denies a gated call.
#[test]
fn unknown_home_denies_gated_call() {
    assert!(taint_verdict(&call("Read", Some("s-clean")), None).is_some());
}

/// #8433: a tool that cannot reach a path is not judged, tainted or not.
#[test]
fn non_gated_tool_is_not_judged() {
    let home = tainted_home();
    assert_eq!(
        taint_verdict(&call("TodoWrite", Some("s-tainted")), Some(home.path())),
        None
    );
    assert_eq!(taint_verdict(&call("TodoWrite", None), None), None);
}

/// 🔴 #8433: the arm turns the guard's allow into a deny, and a deny stands.
/// #9741: the policy reads ready here, so the taint deny is the verdict.
#[test]
fn with_taint_denies_an_allowed_gated_call_of_a_tainted_session() {
    let home = tainted_home();
    let read = Ok(call("Read", Some("s-tainted")));
    let ready = || Ok(());
    let verdict = with_taint(Verdict::Allow, &read, Some(home.path()), ready);
    assert!(
        matches!(
            verdict,
            Verdict::Deny {
                check: TAINT_CHECK,
                ..
            }
        ),
        "{verdict:?}"
    );
    let first = Verdict::Deny {
        check: "egress-family",
        reason: "x".into(),
    };
    assert_eq!(
        with_taint(first.clone(), &read, Some(home.path()), ready),
        first
    );
    let clean = Ok(call("Read", Some("s-clean")));
    assert_eq!(
        with_taint(Verdict::Allow, &clean, Some(home.path()), ready),
        Verdict::Allow
    );
}

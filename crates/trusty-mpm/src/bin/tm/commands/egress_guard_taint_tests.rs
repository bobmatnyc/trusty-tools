//! #8433 P0: the egress guard's channel-taint arm.

use super::*;
use crate::commands::egress_guard::{Verdict, with_taint};
use serde_json::json;
use trusty_mpm::core::channels_taint::{
    TaintMark, TaintState, read_state, taint_dir, taint_managed_session,
};

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

/// A canonical temp home with `s-tainted` tainted by `m-1`, whose mark is
/// scoped to `<home>/ws` (#9923: a session launched at `<home>` is outside it).
struct Home {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
}

impl Home {
    fn path(&self) -> &std::path::Path {
        &self.home
    }

    fn ws(&self) -> std::path::PathBuf {
        self.home.join("ws")
    }
}

fn tainted_home() -> Home {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    std::fs::create_dir(home.join("ws")).expect("mkdir ws");
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: home.join("ws"),
    };
    taint_managed_session(&taint_dir(&home), "m-1", "s-tainted", &mark).expect("taint");
    Home { _tmp: tmp, home }
}

/// 🔴 #8433: every gated tool of a tainted session is denied.
#[test]
fn tainted_session_gated_call_denies() {
    let home = tainted_home();
    for tool in GATED_TOOLS {
        let reason = taint_verdict(&call(tool, Some("s-tainted")), Some(home.path()), None);
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
    let reason = taint_verdict(
        &call("MultiEdit", Some("s-tainted")),
        Some(home.path()),
        None,
    );
    assert!(reason.is_some_and(|r| r.contains("channel-tainted")));
}

/// 🔴 #8433: an untainted session no mark covers is not judged here.
#[test]
fn untainted_session_gated_call_is_not_judged() {
    let home = tainted_home();
    let outside = Some(home.path());
    assert_eq!(
        taint_verdict(&call("Read", Some("s-clean")), Some(home.path()), outside),
        None
    );
    assert_eq!(
        read_state(&taint_dir(home.path()), "s-clean"),
        TaintState::Untainted
    );
}

/// 🔴 #9923: an untainted session launched under a live mark, or whose live
/// cwd is under it, is tainted for that mark and denied.
#[test]
fn an_untainted_session_under_a_live_mark_is_tainted_and_denied() {
    let home = tainted_home();
    let dir = taint_dir(home.path());
    let ws = home.ws();
    let reason = taint_verdict(&call("Read", Some("s-new")), Some(home.path()), Some(&ws));
    assert!(
        reason.is_some_and(|r| r.contains("channel-tainted") && r.contains("`m-1`")),
        "launched in ws"
    );
    assert!(read_state(&dir, "s-new").is_tainted());

    let mut in_ws = call("Read", Some("s-cwd"));
    in_ws["cwd"] = json!(ws.display().to_string());
    assert!(
        taint_verdict(&in_ws, Some(home.path()), Some(home.path())).is_some(),
        "launched outside, live cwd in ws"
    );
    assert!(read_state(&dir, "s-cwd").is_tainted());
}

/// 🔴 #9923 (fail closed): a relative launch directory is unknown, so any
/// live mark covers it.
#[test]
fn a_relative_launch_dir_under_a_live_mark_denies() {
    let home = tainted_home();
    let relative = std::path::Path::new("elsewhere");
    let reason = taint_verdict(
        &call("Read", Some("s-rel")),
        Some(home.path()),
        Some(relative),
    );
    assert!(reason.is_some_and(|r| r.contains("#9923")));
}

/// 🔴 #8433 (fail closed): a gated call naming no session denies.
#[test]
fn gated_call_without_session_id_denies() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(taint_verdict(&call("Read", None), Some(home.path()), None).is_some());
    let mut numeric = call("Read", None);
    numeric["session_id"] = json!(7);
    assert!(taint_verdict(&numeric, Some(home.path()), None).is_some());
}

/// 🔴 #8433 (fail closed): an unknown home denies a gated call.
#[test]
fn unknown_home_denies_gated_call() {
    assert!(taint_verdict(&call("Read", Some("s-clean")), None, None).is_some());
}

/// #8433: a tool that cannot reach a path is not judged, tainted or not.
#[test]
fn non_gated_tool_is_not_judged() {
    let home = tainted_home();
    assert_eq!(
        taint_verdict(
            &call("TodoWrite", Some("s-tainted")),
            Some(home.path()),
            None
        ),
        None
    );
    assert_eq!(taint_verdict(&call("TodoWrite", None), None, None), None);
}

/// 🔴 #8433: the arm turns the guard's allow into a deny, and a deny stands.
/// #9741: the policy reads ready here, so the taint deny is the verdict.
#[test]
fn with_taint_denies_an_allowed_gated_call_of_a_tainted_session() {
    let home = tainted_home();
    let read = Ok(call("Read", Some("s-tainted")));
    let ready = || Ok(());
    let verdict = with_taint(Verdict::Allow, &read, Some(home.path()), None, ready);
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
        with_taint(first.clone(), &read, Some(home.path()), None, ready),
        first
    );
    let clean = Ok(call("Read", Some("s-clean")));
    let outside = Some(home.path());
    assert_eq!(
        with_taint(Verdict::Allow, &clean, Some(home.path()), outside, ready),
        Verdict::Allow
    );
}

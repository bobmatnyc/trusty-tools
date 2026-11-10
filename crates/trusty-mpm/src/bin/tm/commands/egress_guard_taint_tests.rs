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

/// The full guard verdict — family check, then the taint arm with a ready
/// policy — for `tool` called by `session` under `home`.
fn judge_connector(home: &Home, session: &str, tool: &str) -> Verdict {
    use crate::commands::egress_guard::decide;
    use crate::commands::egress_guard_families::FamilyTable;
    let read = Ok(call(tool, Some(session)));
    let first = decide(&read, &FamilyTable::builtin());
    // Launched at the home, outside the mark's `ws`, so `s-clean` stays clean.
    let launch = Some(home.path());
    with_taint(first, &read, Some(home.path()), launch, || Ok(()))
}

fn is_taint_deny(verdict: &Verdict) -> bool {
    matches!(
        verdict,
        Verdict::Deny {
            check: TAINT_CHECK,
            ..
        }
    )
}

/// 🔴 #10735 (Architect answer to D6d "keep taint"): after taint, a claude.ai
/// connector tool that sends, writes or changes state is denied by the taint
/// arm, the session's path calls stay denied, and a non-connector Slack server
/// keeps its family deny.
#[test]
fn a_tainted_session_keeps_its_taint_deny_and_reaches_the_connectors_10735() {
    let home = tainted_home();
    assert!(is_taint_deny(&judge_connector(
        &home,
        "s-tainted",
        "mcp__claude_ai_Slack__slack_send_message"
    )));
    assert_eq!(
        judge_connector(&home, "s-tainted", "mcp__claude_ai_Notion__notion-search"),
        Verdict::Allow
    );
    for tool in ["Read", "Write"] {
        assert!(
            is_taint_deny(&judge_connector(&home, "s-tainted", tool)),
            "{tool}"
        );
    }
    assert!(matches!(
        judge_connector(&home, "s-tainted", "mcp__slack-mcp__slack_send_message"),
        Verdict::Deny {
            check: "egress-family",
            ..
        }
    ));
}

/// 🔴 #10735: every send, post, write, create, update, delete, upload or share
/// tool of a claude.ai connector is denied once the session is tainted.
#[test]
fn tainted_session_denies_claude_ai_connector_writes_10735() {
    let home = tainted_home();
    for tool in [
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__claude_ai_Slack__slack_schedule_message",
        "mcp__claude_ai_Gmail__create_draft",
        "mcp__claude_ai_Google_Chat__send_message",
        "mcp__claude_ai_Notion__notion-create-pages",
        "mcp__claude_ai_Notion__notion-update-page",
        "mcp__claude_ai_Google_Drive__upload_file",
        "mcp__claude_ai_Google_Drive__share_file",
        "mcp__claude_ai_Vercel__delete_project",
        "mcp__claude_ai_Vercel__postComment",
        // A read verb first does not lift a write verb later in the name.
        "mcp__claude_ai_Notion__get_or_create_page",
    ] {
        assert!(
            is_taint_deny(&judge_connector(&home, "s-tainted", tool)),
            "{tool}"
        );
    }
}

/// 🔴 #10735: a claude.ai connector tool whose verb is a known read stays
/// allowed in a tainted session.
#[test]
fn tainted_session_allows_claude_ai_connector_reads_10735() {
    let home = tainted_home();
    for tool in [
        "mcp__claude_ai_Notion__notion-search",
        "mcp__claude_ai_Notion__notion-fetch",
        "mcp__claude_ai_Slack__slack_read_channel",
        "mcp__claude_ai_Slack__slack_search_public",
        "mcp__claude_ai_Gmail__search_threads",
        "mcp__claude_ai_Gmail__get_thread",
        "mcp__claude_ai_Google_Drive__google_drive_search",
        "mcp__claude_ai_Vercel__list_projects",
        "mcp__claude_ai_Vercel__getDeployment",
    ] {
        assert_eq!(
            judge_connector(&home, "s-tainted", tool),
            Verdict::Allow,
            "{tool}"
        );
    }
}

/// 🔴 #10735 (fail closed): a claude.ai connector tool whose verb is not a
/// known read is treated as a write once the session is tainted.
#[test]
fn tainted_session_denies_unknown_claude_ai_connector_verbs_10735() {
    let home = tainted_home();
    for tool in [
        "mcp__claude_ai_Slack__slack_frobnicate",
        "mcp__claude_ai_Hugging_Face__hf_fs",
        "mcp__claude_ai_Notion__notion",
        "mcp__claude_ai_Vercel__invite_member_list",
        "mcp__claude_ai_Gmail__readandsend",
        "mcp__claude_ai_Notion__search_and_archive",
    ] {
        assert!(
            is_taint_deny(&judge_connector(&home, "s-tainted", tool)),
            "{tool}"
        );
    }
}

/// 🔴 #10735: untainted, a claude.ai connector write is allowed and a native
/// connector is denied by its family, as before the taint change.
#[test]
fn untainted_session_connector_verdicts_are_unchanged_10735() {
    let home = tainted_home();
    for tool in [
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__claude_ai_Notion__notion-create-pages",
        "mcp__claude_ai_Slack__slack_frobnicate",
    ] {
        assert_eq!(
            judge_connector(&home, "s-clean", tool),
            Verdict::Allow,
            "{tool}"
        );
    }
    for session in ["s-clean", "s-tainted"] {
        for tool in [
            "mcp__slack-mcp__slack_send_message",
            "mcp__notion__notion-search",
            "mcp__gworkspace-mcp__send_gmail_message",
        ] {
            assert!(
                matches!(
                    judge_connector(&home, session, tool),
                    Verdict::Deny {
                        check: "egress-family",
                        ..
                    }
                ),
                "{session} {tool}"
            );
        }
    }
}

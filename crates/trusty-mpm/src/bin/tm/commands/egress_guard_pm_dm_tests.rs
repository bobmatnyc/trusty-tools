//! Unit tests for the #11194 owner-DM Slack send arm (`egress_guard_pm_dm.rs`).
//! Every case runs the guard's own pipeline — the #8878 floor, the
//! family/Bash decision, the #10447 Architect lift, this lift, the taint arm
//! and both audits — over an injected PM environment, so no test reads or
//! sets a process-global environment variable. Channel and user ids other
//! than [`OWNER_DM`] are synthetic.

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::commands::egress_guard::{verdict, with_taint};
use crate::commands::egress_guard_architect::{self as architect, AUDIT_CHECK};
use crate::commands::egress_guard_families::FamilyTable;
use crate::commands::pm_guard_trust_anchor::evaluate;
use crate::commands::pm_guard_trust_anchor::tests::{Fixture, fixture, payload};
use trusty_mpm::core::config::MpmConfig;

/// The fixed audit time every case writes.
const TIME: &str = "2026-11-13T19:00:00Z";

/// A message body the audit must never carry.
const BODY: &str = "BODY-SECRET-11194";

/// A tm-launched PM session: the `pm` stamp, launched in the fixture's work
/// directory, with no subagent marker.
fn pm_env(fx: &Fixture) -> HookEnv {
    HookEnv {
        home: Some(fx.home.clone()),
        stamp: Some("pm".into()),
        project_dir: Some(fx.cwd.clone().into_os_string()),
        ..HookEnv::default()
    }
}

/// The audit file of `fx`.
fn log_of(fx: &Fixture) -> PathBuf {
    fx.home.join("data").join("egress-architect-allowed.log")
}

/// The guard's verdict for `read`, as `guard_once` computes it.
fn run_read(read: &Result<Value, HookStdinError>, env: &HookEnv, log: &Path) -> Verdict {
    let table = FamilyTable::builtin();
    let first = verdict(read, &table, |p| evaluate(p, env, MpmConfig::default));
    let (first, arch) = architect::lift(first, read, &table, env, MpmConfig::default);
    let (first, dm) = lift(first, read, env);
    let first = with_taint(first, read, env.home.as_deref(), None, || Ok(()));
    let first = architect::audited(first, arch, Some(log), TIME);
    audited(first, dm, Some(log), TIME)
}

/// [`run_read`] for a parsed payload.
fn run(call: &Value, env: &HookEnv, log: &Path) -> Verdict {
    run_read(&Ok(call.clone()), env, log)
}

/// The check slug of a deny, or a panic naming the allowed call.
fn denied(v: Verdict, what: &str) -> &'static str {
    match v {
        Verdict::Deny { check, .. } => check,
        Verdict::Allow => panic!("{what} must be denied"),
    }
}

/// A slack-mcp send to `channel` carrying [`BODY`].
fn send(fx: &Fixture, channel: Value) -> Value {
    payload(fx, SEND_TOOL, json!({ "channel": channel, "text": BODY }))
}

/// 🔴 #11194: a PM main thread may send to the owner's DM, and the allow
/// appends one audit line with no message body.
#[test]
fn pm_main_thread_may_send_to_the_owner_dm() {
    let fx = fixture();
    let log = log_of(&fx);
    let call = send(&fx, json!(OWNER_DM));
    assert_eq!(run(&call, &pm_env(&fx), &log), Verdict::Allow);
    let thread = json!({ "channel": OWNER_DM, "text": BODY, "thread_ts": "1.2" });
    let reply = payload(&fx, SEND_TOOL, thread);
    assert_eq!(run(&reply, &pm_env(&fx), &log), Verdict::Allow);

    let text = std::fs::read_to_string(&log).expect("audit log written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one line per allowed call: {text}");
    assert_eq!(
        lines[0],
        format!(
            "{ALLOWED_AUDIT} time={TIME} session=s-1 family=slack tool={SEND_TOOL} \
             channel={OWNER_DM}"
        )
    );
    assert!(!text.contains(BODY), "no body in the audit: {text}");
}

/// 🔴 #11194: any other target — a channel, a user id, a near miss in case,
/// whitespace or prefix — and any other Slack write tool or family stays denied.
#[test]
fn pm_send_to_any_other_target_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = pm_env(&fx);
    for channel in [
        "C0SYNTH0001",
        "U0SYNTH0001",
        "d0aj5p0prtk",
        " D0AJ5P0PRTK",
        "D0AJ5P0PRTK ",
        "#D0AJ5P0PRTK",
        "D0AJ5P0PRTKX",
        "D0AJ5P0PRT",
    ] {
        let call = send(&fx, json!(channel));
        assert_eq!(denied(run(&call, &env, &log), channel), "egress-family");
    }
    let dm = json!({ "channel": OWNER_DM, "channel_id": OWNER_DM, "text": BODY });
    for tool in [
        "mcp__slack-mcp__slack_update_canvas",
        "mcp__slack-mcp__slack_canvas_create",
        "mcp__slack-mcp__slack_canvas_push",
        "mcp__slack-mcp__slack_create_conversation",
        "mcp__Slack-MCP__slack_send_message",
        "mcp__slack__slack_send_message",
        "mcp__slack_mcp__slack_send_message",
        "mcp__gmail__send_message",
        "mcp__notion__notion-create-pages",
        "mcp__google_chat__send_message",
    ] {
        let call = payload(&fx, tool, dm.clone());
        assert_eq!(denied(run(&call, &env, &log), tool), "egress-family");
    }
    let bash = json!({ "command": "tm slack send D0AJ5P0PRTK hi" });
    let call = payload(&fx, "Bash", bash);
    assert_eq!(denied(run(&call, &env, &log), "Bash"), "egress-bash");
    assert!(!log.exists(), "no deny writes an allow line");
}

/// 🔴 #11194: a subagent's DM send stays denied, by payload `agent_id` and by
/// `CLAUDE_MPM_SUB_AGENT`; so does a thread the payload does not establish.
#[test]
fn pm_subagent_dm_send_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    let mut call = send(&fx, json!(OWNER_DM));
    call["agent_id"] = json!("agent-synth-1");
    assert_eq!(
        denied(run(&call, &pm_env(&fx), &log), "agent_id"),
        "egress-family"
    );
    let env = HookEnv {
        sub_agent: true,
        ..pm_env(&fx)
    };
    let main = send(&fx, json!(OWNER_DM));
    assert_eq!(denied(run(&main, &env, &log), "env"), "egress-family");
    for (key, value) in [("agent_id", json!("")), ("agent_id", json!(7))] {
        let mut odd = send(&fx, json!(OWNER_DM));
        odd[key] = value;
        assert_eq!(denied(run(&odd, &pm_env(&fx), &log), key), "egress-family");
    }
    let mut no_session = send(&fx, json!(OWNER_DM));
    no_session
        .as_object_mut()
        .expect("object")
        .remove("session_id");
    let verdict = run(&no_session, &pm_env(&fx), &log);
    assert_eq!(denied(verdict, "no session_id"), "egress-family");
    assert!(!log.exists());
}

/// 🔴 #11194: a canvas whose `channel_id` is anything but the owner's DM
/// string stays denied.
#[test]
fn pm_canvas_with_another_channel_id_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    for channel_id in [
        json!("C0SYNTH0001"),
        json!(""),
        json!(null),
        json!(1),
        json!([OWNER_DM]),
        json!({ "id": OWNER_DM }),
    ] {
        let input = json!({ "title": "t", "markdown": BODY, "channel_id": channel_id });
        let call = payload(&fx, CANVAS_TOOL, input);
        let what = channel_id.to_string();
        assert_eq!(
            denied(run(&call, &pm_env(&fx), &log), &what),
            "egress-family"
        );
    }
    assert!(!log.exists());
}

/// 🔴 #11194: a canvas with no `channel_id`, or the owner's DM, is allowed
/// and audited.
#[test]
fn pm_canvas_with_no_channel_id_is_allowed() {
    let fx = fixture();
    let log = log_of(&fx);
    let bare = payload(&fx, CANVAS_TOOL, json!({ "title": "t", "markdown": BODY }));
    assert_eq!(run(&bare, &pm_env(&fx), &log), Verdict::Allow);
    let input = json!({ "markdown": BODY, "channel_id": OWNER_DM });
    let dm = payload(&fx, CANVAS_TOOL, input);
    assert_eq!(run(&dm, &pm_env(&fx), &log), Verdict::Allow);
    let text = std::fs::read_to_string(&log).expect("audit log written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            format!(
                "{ALLOWED_AUDIT} time={TIME} session=s-1 family=slack tool={CANVAS_TOOL} channel=-"
            ),
            format!(
                "{ALLOWED_AUDIT} time={TIME} session=s-1 family=slack tool={CANVAS_TOOL} \
                 channel={OWNER_DM}"
            ),
        ]
    );
}

/// 🔴 #11194 fail-open check: a send with `channel` missing, empty, null or
/// not a string, a non-object input, and an unparsable payload all deny.
#[test]
fn pm_send_with_malformed_input_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = pm_env(&fx);
    let missing = payload(&fx, SEND_TOOL, json!({ "text": BODY }));
    assert_eq!(
        denied(run(&missing, &env, &log), "missing"),
        "egress-family"
    );
    let wrong_key = payload(&fx, SEND_TOOL, json!({ "channel_id": OWNER_DM }));
    assert_eq!(
        denied(run(&wrong_key, &env, &log), "channel_id"),
        "egress-family"
    );
    for channel in [
        json!(""),
        json!(null),
        json!(0),
        json!(true),
        json!([OWNER_DM]),
        json!({ "id": OWNER_DM }),
    ] {
        let what = channel.to_string();
        let call = send(&fx, channel);
        assert_eq!(denied(run(&call, &env, &log), &what), "egress-family");
    }
    for input in [json!(OWNER_DM), json!(null), json!([OWNER_DM])] {
        let call = payload(&fx, SEND_TOOL, input);
        assert_eq!(
            denied(run(&call, &env, &log), "non-object input"),
            "egress-unclassifiable-payload"
        );
    }
    let bad: Result<Value, HookStdinError> = Err(HookStdinError::NotAnObject);
    assert_eq!(
        denied(run_read(&bad, &env, &log), "unparsable"),
        "egress-unreadable-payload"
    );
    assert!(!log.exists());
}

/// 🔴 #11194: an owner-DM allow whose audit line cannot be written denies,
/// as the #10447 Architect allow does; a deny passes through unchanged.
#[test]
fn pm_dm_allow_needs_its_audit_line() {
    let fx = fixture();
    let allowed = PmDmAllowed {
        tool: SEND_TOOL.into(),
        session: "s-1".into(),
        channel: Some(OWNER_DM),
    };
    let unknown = audited(Verdict::Allow, Some(allowed.clone()), None, TIME);
    let Verdict::Deny { check, reason } = unknown else {
        panic!("no data dir must deny");
    };
    assert_eq!(check, AUDIT_CHECK);
    assert!(reason.contains("#11194"), "{reason}");
    let blocker = fx.home.join("file");
    std::fs::write(&blocker, "x").expect("write blocker");
    let path = blocker.join("log");
    let unwritable = audited(Verdict::Allow, Some(allowed.clone()), Some(&path), TIME);
    assert_eq!(denied(unwritable, "unwritable log"), AUDIT_CHECK);
    assert_eq!(audited(Verdict::Allow, None, None, TIME), Verdict::Allow);
    let deny = Verdict::Deny {
        check: "egress-family",
        reason: "r".into(),
    };
    assert_eq!(audited(deny.clone(), Some(allowed), None, TIME), deny);
}

/// 🔴 #11194: payload fields are reduced to a safe alphabet, so a session id
/// carrying a newline cannot forge a second audit line.
#[test]
fn pm_dm_audit_line_carries_no_body_and_no_newline() {
    let allowed = PmDmAllowed {
        tool: SEND_TOOL.into(),
        session: "s-1\negress-pm-dm-allowed session=forged".into(),
        channel: None,
    };
    let line = allowed.audit_line(TIME);
    assert!(!line.contains('\n'), "{line}");
    assert!(!line.contains("session=forged"), "{line}");
    assert!(line.ends_with("channel=-"), "{line}");
}

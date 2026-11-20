//! Unit tests for the #11273 main-session read pass
//! (`egress_guard_main_read.rs`). Every case runs the guard's own pipeline —
//! the #8878 floor, the family/Bash decision, the #10447 Architect lift, the
//! #11194 owner-DM lift, this pass, the taint arm and the three audits — over
//! an injected home, stamp and process table, so no test reads or sets a
//! process-global environment variable.

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::commands::egress_guard::{verdict, with_taint};
use crate::commands::egress_guard_architect::{self as architect, AUDIT_CHECK};
use crate::commands::egress_guard_bash::BASH_CHECK;
use crate::commands::egress_guard_pm_dm::{self as pm_dm, OWNER_DM, SEND_TOOL};
use crate::commands::egress_guard_taint::TAINT_CHECK;
use crate::commands::pm_guard_trust_anchor::evaluate;
use crate::commands::pm_guard_trust_anchor::tests::{
    Fixture, architect_env, fixture, payload, spoof_env,
};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile::ArchitectConfig;

/// The fixed audit time every case writes.
const TIME: &str = "2026-11-14T12:00:00Z";

/// A call argument the audit must never carry.
const BODY: &str = "BODY-SECRET-11273";

/// One read tool per family, native servers included, plus a resource read.
const READS: &[&str] = &[
    "mcp__slack-mcp__slack_read_channel",
    "mcp__slack-mcp__slack_search_messages",
    "mcp__gworkspace-mcp__search_gmail_messages",
    "mcp__gworkspace-mcp__list_message_attachments",
    "mcp__gmail__read_email",
    "mcp__notion__notion-search",
    "mcp__google_chat__list_spaces",
];

/// Sends and writes, one or more per family.
const SENDS: &[&str] = &[
    "mcp__slack-mcp__slack_send_message",
    "mcp__slack-mcp__slack_create_canvas",
    // #10890: an edit grant is a write; a subagent must stay denied.
    "mcp__slack-mcp__slack_canvas_access_set",
    "mcp__gworkspace-mcp__compose_email",
    "mcp__gworkspace-mcp__download_gmail_attachment",
    "mcp__gmail__send_message",
    "mcp__notion__notion-create-pages",
    "mcp__google_chat__send_message",
];

/// A tm-launched PM main session: the `pm` stamp, no process record.
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

/// The user config: `env`'s launch directory on the `[supervisor]` allowlist.
fn config_for(env: &HookEnv) -> MpmConfig {
    let dir = env.project_dir.clone().map(PathBuf::from);
    MpmConfig {
        architect: dir.map(|d| ArchitectConfig {
            projects: vec![d],
            ..ArchitectConfig::default()
        }),
        ..MpmConfig::default()
    }
}

/// The guard's verdict for `read`, as `guard_once` computes it.
fn run_read(read: &Result<Value, HookStdinError>, env: &HookEnv, log: &Path) -> Verdict {
    let config = || config_for(env);
    let table = FamilyTable::builtin();
    let first = verdict(read, &table, |p| evaluate(p, env, config));
    let (first, arch) = architect::lift(first, read, &table, env, config);
    let (first, dm) = pm_dm::lift(first, read, env);
    let (first, main) = lift(first, read, &table, env);
    let first = with_taint(first, read, env.home.as_deref(), None, || Ok(()));
    let first = architect::audited(first, arch, Some(log), TIME);
    let first = pm_dm::audited(first, dm, Some(log), TIME);
    audited(first, main, Some(log), TIME)
}

/// [`run_read`] for a parsed payload.
fn run(call: &Value, env: &HookEnv, log: &Path) -> Verdict {
    run_read(&Ok(call.clone()), env, log)
}

/// The check slug and reason of a deny, or a panic naming the allowed call.
fn denied(v: Verdict, what: &str) -> (&'static str, String) {
    match v {
        Verdict::Deny { check, reason } => (check, reason),
        Verdict::Allow => panic!("{what} must be denied"),
    }
}

/// A family call to `tool` carrying [`BODY`], aimed at a channel that is not
/// the owner's DM (so the #11194 lift does not apply).
fn call(fx: &Fixture, tool: &str) -> Value {
    let input = json!({ "channel": "C1", "channel_id": "C1", "text": BODY });
    payload(fx, tool, input)
}

/// A resource read naming the native Slack server.
fn resource_read(fx: &Fixture) -> Value {
    let input = json!({ "server": "slack-mcp", "uri": "slack://C1" });
    payload(fx, "ReadMcpResourceTool", input)
}

/// The number of audit lines written so far.
fn lines(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

/// 🔴 #11273 criterion 1: a PM, an unbound Architect and a bound Architect
/// main session each pass a read in every family and a resource read; each
/// allow writes one audit line.
#[test]
fn main_session_reads_pass_in_every_family_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let envs = [
        ("pm", pm_env(&fx)),
        ("unbound architect", spoof_env(&fx)),
        ("bound architect", architect_env(&fx)),
    ];
    let mut expected = 0;
    for (who, env) in &envs {
        for tool in READS {
            assert_eq!(
                run(&call(&fx, tool), env, &log),
                Verdict::Allow,
                "{who} {tool}"
            );
            expected += 1;
        }
        assert_eq!(run(&resource_read(&fx), env, &log), Verdict::Allow, "{who}");
        expected += 1;
    }
    assert_eq!(lines(&log), expected, "one audit line per allowed read");
    let text = std::fs::read_to_string(&log).expect("audit log");
    assert!(text.contains(&format!(
        "{ALLOWED_AUDIT} time={TIME} session=s-1 role=pm family=slack \
         tool=mcp__slack-mcp__slack_read_channel"
    )));
    assert!(text.contains("role=architect"));
}

/// 🔴 #11273 criterion 1: a tool the read list does not know is a write,
/// whatever its verb; so is a known name in another case or another family.
#[test]
fn an_unknown_family_tool_is_a_write_11273() {
    for (tool, family) in [
        ("mcp__slack-mcp__slack_read_channel_v2", "slack"),
        ("mcp__slack-mcp__SLACK_READ_CHANNEL", "slack"),
        ("mcp__slack-mcp__slack_get_everything", "slack"),
        ("mcp__notion__notion-search", "slack"),
        ("mcp__slack-mcp__slack_read_channel", "a-new-family"),
        ("mcp__slack-mcp", "slack"),
        ("slack_read_channel", "slack"),
    ] {
        assert_eq!(
            tool_access(tool, family),
            Access::Write,
            "{tool} in {family}"
        );
    }
    let fx = fixture();
    let log = log_of(&fx);
    // #11521: a stamped main thread may call it; its subagent may not.
    let renamed = call(&fx, "mcp__slack-mcp__slack_fetch_and_post");
    assert_eq!(run(&renamed, &pm_env(&fx), &log), Verdict::Allow);
    for (sub, env) in subagent_calls(&fx, "mcp__gmail__fetch_and_post", &pm_env(&fx)) {
        let (check, _) = denied(run(&sub, &env, &log), "an unknown subagent tool");
        assert_eq!(check, "egress-family");
    }
    assert_eq!(lines(&log), 1);
}

/// #11273: every listed read is a read; the send and write tools are not.
#[test]
fn every_listed_read_is_a_read_and_sends_are_writes_11273() {
    let families = [
        ("slack", SLACK_READS),
        ("gmail", GMAIL_READS),
        ("googlemail", GMAIL_READS),
        ("gworkspace", GWORKSPACE_READS),
        ("notion", NOTION_READS),
        ("googlechat", GOOGLECHAT_READS),
        ("gchat", GOOGLECHAT_READS),
    ];
    for (family, reads) in families {
        for tool in reads {
            let name = format!("mcp__srv__{tool}");
            assert_eq!(tool_access(&name, family), Access::Read, "{name}");
        }
    }
    let table = FamilyTable::builtin();
    for tool in SENDS {
        let Classification::Family(family) = table.classify(tool) else {
            panic!("{tool} must be a family tool");
        };
        assert_eq!(tool_access(tool, &family), Access::Write, "{tool}");
    }
}

/// The reads of [`READS`] in the Gmail and Google Workspace class (GP-043).
const GWORKSPACE_CLASS: &[&str] = &[
    "mcp__gworkspace-mcp__search_gmail_messages",
    "mcp__gworkspace-mcp__list_message_attachments",
    "mcp__gworkspace-mcp__get_gmail_message_content",
    "mcp__gmail__read_email",
];

/// A call from a subagent: an `agent_id` payload, or `CLAUDE_MPM_SUB_AGENT`.
fn subagent_calls(fx: &Fixture, tool: &str, env: &HookEnv) -> [(Value, HookEnv); 2] {
    let mut dispatched = call(fx, tool);
    dispatched["agent_id"] = json!("agent-7");
    let nested = HookEnv {
        sub_agent: true,
        ..env.clone()
    };
    [(dispatched, env.clone()), (call(fx, tool), nested)]
}

/// 🔴 #11273 criterion 3: an Architect subagent's read stays denied in every
/// family, and a PM subagent's outside the GP-043 Gmail/Workspace class, under
/// an `agent_id` payload or `CLAUDE_MPM_SUB_AGENT`; nothing is audited.
#[test]
fn subagent_reads_stay_denied_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let pm_other: Vec<&str> = READS
        .iter()
        .copied()
        .filter(|t| !GWORKSPACE_CLASS.contains(t))
        .collect();
    for (env, tools) in [
        (pm_env(&fx), pm_other.as_slice()),
        (spoof_env(&fx), READS),
        (architect_env(&fx), READS),
    ] {
        for tool in tools {
            let mut dispatched = call(&fx, tool);
            dispatched["agent_id"] = json!("agent-7");
            let (check, _) = denied(run(&dispatched, &env, &log), "an agent_id read");
            assert_eq!(check, "egress-family", "{tool}");
            let nested = HookEnv {
                sub_agent: true,
                ..env.clone()
            };
            let (check, _) = denied(run(&call(&fx, tool), &nested, &log), "a marked read");
            assert_eq!(check, "egress-family", "{tool}");
        }
    }
    assert_eq!(lines(&log), 0, "a denied read must not audit an allow");
}

/// 🔴 #11273 criterion 4: an unknown thread (no or empty `session_id`, a
/// non-string `agent_id`), an unstamped session and an unknown stamp each
/// keep the read denied.
#[test]
fn unknown_thread_or_unstamped_session_reads_stay_denied_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let tool = "mcp__slack-mcp__slack_read_channel";
    let mut no_session = call(&fx, tool);
    no_session
        .as_object_mut()
        .expect("object")
        .remove("session_id");
    let mut empty_session = call(&fx, tool);
    empty_session["session_id"] = json!("");
    let mut odd_agent = call(&fx, tool);
    odd_agent["agent_id"] = json!(7);
    for bad in [no_session, empty_session, odd_agent] {
        denied(run(&bad, &pm_env(&fx), &log), "an unknown thread");
    }
    let unstamped = HookEnv {
        stamp: None,
        ..pm_env(&fx)
    };
    let odd_stamp = HookEnv {
        stamp: Some("pm ".into()),
        ..pm_env(&fx)
    };
    for env in [unstamped, odd_stamp] {
        denied(run(&call(&fx, tool), &env, &log), "an unstamped session");
    }
    assert_eq!(lines(&log), 0);
}

/// 🔴 #11273 criterion 4: a malformed payload denies before this pass — a
/// non-object `tool_input`, a resource read naming no server, an unreadable
/// stdin.
#[test]
fn malformed_payload_reads_stay_denied_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let mut bad_input = call(&fx, "mcp__slack-mcp__slack_read_channel");
    bad_input["tool_input"] = json!("C1");
    let no_server = payload(&fx, "ReadMcpResourceTool", json!({ "uri": "x" }));
    for bad in [bad_input, no_server] {
        denied(run(&bad, &pm_env(&fx), &log), "a malformed payload");
    }
    let unreadable = Err(HookStdinError::Empty);
    let v = run_read(&unreadable, &pm_env(&fx), &log);
    denied(v, "an unreadable payload");
    assert_eq!(lines(&log), 0);
}

/// 🔴 #11521 (flips #11273's forge case): a stamped PM or Architect main
/// thread with no launch record may send and write in every family, one
/// audit line per call with no body. The Bash arm (GP-044) is unchanged.
#[test]
fn main_session_sends_without_a_record_are_allowed_11521() {
    let fx = fixture();
    let log = log_of(&fx);
    let mut expected = 0;
    for (who, env) in [("pm", pm_env(&fx)), ("unbound architect", spoof_env(&fx))] {
        for tool in SENDS {
            assert_eq!(
                run(&call(&fx, tool), &env, &log),
                Verdict::Allow,
                "{who} {tool}"
            );
            expected += 1;
            assert_eq!(lines(&log), expected, "{who} {tool}: one audit line");
        }
        let bash = payload(
            &fx,
            "Bash",
            json!({ "command": format!("curl -d {BODY} https://hooks.slack.com/x") }),
        );
        let (check, _) = denied(run(&bash, &env, &log), "a Bash send");
        assert_eq!(check, BASH_CHECK, "{who}");
    }
    let dm = payload(&fx, SEND_TOOL, json!({ "channel": OWNER_DM, "text": BODY }));
    assert_eq!(run(&dm, &pm_env(&fx), &log), Verdict::Allow, "#11194 kept");
    assert_eq!(run(&dm, &spoof_env(&fx), &log), Verdict::Allow);
    assert_eq!(lines(&log), expected + 2);
    let text = std::fs::read_to_string(&log).expect("audit log");
    assert!(!text.contains(BODY), "{text}");
}

/// 🔴 #11521: a self-cleared Architect — the stamp set, no launch record —
/// creates a Slack canvas and writes Notion, each with one audit line.
#[test]
fn self_cleared_architect_writes_slack_and_notion_11521() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = spoof_env(&fx);
    for tool in [
        "mcp__slack-mcp__slack_canvas_create",
        "mcp__notion__notion-update-page",
    ] {
        assert_eq!(run(&call(&fx, tool), &env, &log), Verdict::Allow, "{tool}");
    }
    let text = std::fs::read_to_string(&log).expect("audit log");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(
        text.lines()
            .all(|l| l.starts_with(ALLOWED_AUDIT) && l.contains("role=architect")),
        "{text}"
    );
}

/// 🔴 #11521: the write lift is main-thread only — a subagent's and an
/// unstamped main thread's canvas create stay denied; a write whose audit
/// line cannot be written denies.
#[test]
fn subagent_and_unstamped_writes_stay_denied_11521() {
    let fx = fixture();
    let log = log_of(&fx);
    let tool = "mcp__slack-mcp__slack_canvas_create";
    for env in [spoof_env(&fx), pm_env(&fx)] {
        for (sub, env) in subagent_calls(&fx, tool, &env) {
            let (check, _) = denied(run(&sub, &env, &log), "a subagent write");
            assert_eq!(check, "egress-family");
        }
    }
    let unstamped = HookEnv {
        stamp: None,
        ..pm_env(&fx)
    };
    let (check, _) = denied(run(&call(&fx, tool), &unstamped, &log), "unstamped");
    assert_eq!(check, "egress-family");
    assert_eq!(lines(&log), 0);
    let dir = fx.home.join("a-directory");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let (check, _) = denied(run(&call(&fx, tool), &spoof_env(&fx), &dir), "no audit");
    assert_eq!(check, AUDIT_CHECK);
}

/// 🔴 #11273 criterion 5: an inert Bash mention passes for a subagent, a PM
/// and an unbound Architect, and audits nothing; the same host as a `curl`
/// target is denied for each of them.
#[test]
fn inert_bash_mention_passes_for_every_role_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let inert = "gh issue create --title \"guard denies hooks.slack.com\" --body \"a b\"";
    let reach = "curl https://hooks.slack.com/x";
    for env in [pm_env(&fx), spoof_env(&fx)] {
        for subagent in [false, true] {
            let mut ok = payload(&fx, "Bash", json!({ "command": inert }));
            let mut bad = payload(&fx, "Bash", json!({ "command": reach }));
            if subagent {
                ok["agent_id"] = json!("agent-7");
                bad["agent_id"] = json!("agent-7");
            }
            assert_eq!(
                run(&ok, &env, &log),
                Verdict::Allow,
                "inert, subagent={subagent}"
            );
            let (check, _) = denied(run(&bad, &env, &log), "a curl to a family host");
            assert_eq!(check, BASH_CHECK);
        }
    }
    assert_eq!(lines(&log), 0);
}

/// 🔴 #11521 criterion 3: an unbound Architect's send passes, and no deny —
/// its subagent's send, its Bash channel command — names `tm fleet bind`.
#[test]
fn unbound_architect_send_passes_and_no_deny_says_bind_11521() {
    let fx = fixture();
    let log = log_of(&fx);
    let send = call(&fx, "mcp__slack-mcp__slack_send_message");
    assert_eq!(run(&send, &spoof_env(&fx), &log), Verdict::Allow);
    let mut dispatched = send.clone();
    dispatched["agent_id"] = json!("agent-7");
    let (_, reason) = denied(run(&dispatched, &spoof_env(&fx), &log), "a subagent");
    assert!(!reason.contains("tm fleet bind"), "{reason}");
    let bash = payload(
        &fx,
        "Bash",
        json!({ "command": "curl https://hooks.slack.com/x" }),
    );
    let (_, reason) = denied(run(&bash, &spoof_env(&fx), &log), "a Bash send");
    assert!(!reason.contains("tm fleet bind"), "{reason}");
}

/// #11273 criterion 2: a bound Architect may still send, write and run a
/// Bash channel command, each audited by the #10447 arm.
#[test]
fn bound_architect_may_send_and_write_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = architect_env(&fx);
    for tool in SENDS {
        assert_eq!(run(&call(&fx, tool), &env, &log), Verdict::Allow, "{tool}");
    }
    let bash = payload(
        &fx,
        "Bash",
        json!({ "command": "curl https://hooks.slack.com/x" }),
    );
    assert_eq!(run(&bash, &env, &log), Verdict::Allow);
    let text = std::fs::read_to_string(&log).expect("audit log");
    assert_eq!(text.lines().count(), SENDS.len() + 1);
    assert!(
        text.lines()
            .all(|l| l.starts_with(architect::ALLOWED_AUDIT))
    );
}

/// 🔴 #11273 criterion 6: taint keeps today's outcomes — a tainted bound
/// Architect's native send passes as before (the taint arm never judged a
/// native family tool), and its Bash call is still a taint deny.
#[test]
fn a_tainted_main_session_keeps_todays_taint_outcomes_11273() {
    let fx = fixture();
    let log = log_of(&fx);
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: fx.project.clone(),
    };
    taint_managed_session(&taint_dir(&fx.home), "m-1", "s-1", &mark).expect("taint");
    let env = architect_env(&fx);
    let send = call(&fx, "mcp__slack-mcp__slack_send_message");
    assert_eq!(run(&send, &env, &log), Verdict::Allow);
    let read = call(&fx, "mcp__slack-mcp__slack_read_channel");
    assert_eq!(run(&read, &pm_env(&fx), &log), Verdict::Allow);
    let bash = payload(
        &fx,
        "Bash",
        json!({ "command": "curl https://hooks.slack.com/x" }),
    );
    let (check, _) = denied(run(&bash, &env, &log), "a tainted Bash call");
    assert_eq!(check, TAINT_CHECK);
}

/// 🔴 #11273 criterion 6 (GP-047): a read allow whose audit line cannot be
/// written — no data directory, or a path that is a directory — denies; a
/// deny is never turned into an allow.
#[test]
fn main_session_read_needs_its_audit_line_11273() {
    let fx = fixture();
    let allowed = MainReadAllowed {
        role: MainRole::Pm,
        family: "slack".into(),
        tool: "mcp__slack-mcp__slack_read_channel".into(),
        session: "s-1".into(),
    };
    let v = audited(Verdict::Allow, Some(allowed.clone()), None, TIME);
    assert_eq!(denied(v, "no data dir").0, AUDIT_CHECK);
    let dir = fx.home.join("a-directory");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let v = audited(Verdict::Allow, Some(allowed.clone()), Some(&dir), TIME);
    assert_eq!(denied(v, "an unwritable path").0, AUDIT_CHECK);
    let deny = Verdict::Deny {
        check: TAINT_CHECK,
        reason: "r".into(),
    };
    let v = audited(deny.clone(), Some(allowed), Some(&log_of(&fx)), TIME);
    assert_eq!(v, deny);
    assert_eq!(lines(&log_of(&fx)), 0);
}

/// #11273: the audit line names role, family, tool and session only, one
/// line, with no call input.
#[test]
fn main_session_read_audit_line_carries_no_input_11273() {
    let allowed = MainReadAllowed {
        role: MainRole::Architect,
        family: "slack".into(),
        tool: format!("mcp__slack-mcp__slack_read_channel\n{BODY}"),
        session: "s-1\nforged".into(),
    };
    let line = allowed.audit_line(TIME);
    assert!(!line.contains('\n'), "{line}");
    assert!(line.starts_with(&format!("{ALLOWED_AUDIT} time={TIME} session=s-1forged")));
    assert!(line.contains("role=architect family=slack"));
}

/// #11273: only an `egress-family` deny of a readable payload is judged — a
/// trust-anchor, Bash or malformed-name deny of a read-shaped call stands.
#[test]
fn only_a_family_deny_is_judged_11273() {
    let fx = fixture();
    let table = FamilyTable::builtin();
    let read = Ok(call(&fx, "mcp__slack-mcp__slack_read_channel"));
    for check in [
        "trust-anchor-write",
        BASH_CHECK,
        "egress-malformed-mcp-name",
    ] {
        let first = Verdict::Deny {
            check,
            reason: "r".into(),
        };
        let (v, allowed) = lift(first.clone(), &read, &table, &pm_env(&fx));
        assert_eq!((v, allowed), (first, None), "{check}");
    }
    let first = Verdict::Deny {
        check: FAMILY_CHECK,
        reason: "r".into(),
    };
    let (v, allowed) = lift(
        first.clone(),
        &Err(HookStdinError::Empty),
        &table,
        &pm_env(&fx),
    );
    assert_eq!((v, allowed), (first, None));
}

/// 🔴 GP-043 (owner ruling 2026-11-16): a PM main session and a PM subagent
/// each pass a Gmail or Google Workspace read, each with one audit line.
#[test]
fn pm_subagent_gworkspace_reads_pass_gp_043() {
    let fx = fixture();
    let log = log_of(&fx);
    let mut expected = 0;
    for tool in GWORKSPACE_CLASS {
        assert_eq!(
            run(&call(&fx, tool), &pm_env(&fx), &log),
            Verdict::Allow,
            "pm {tool}"
        );
        expected += 1;
        for (sub, env) in subagent_calls(&fx, tool, &pm_env(&fx)) {
            assert_eq!(run(&sub, &env, &log), Verdict::Allow, "pm subagent {tool}");
            expected += 1;
        }
    }
    assert_eq!(lines(&log), expected);
    let text = std::fs::read_to_string(&log).expect("audit");
    assert!(text.contains("role=pm-subagent"), "{text}");
    assert!(!text.contains(BODY), "{text}");
}

/// 🔴 GP-043: a PM subagent's Gmail/Workspace send or write stays denied, and
/// so does an unknown or unlisted gworkspace tool (fail closed). The PM's
/// main thread may call each of them (#11521).
#[test]
fn pm_subagent_other_family_and_unlisted_reads_stay_denied_gp_043() {
    let fx = fixture();
    let log = log_of(&fx);
    let gated = [
        "mcp__gworkspace-mcp__compose_email",
        "mcp__gworkspace-mcp__send_draft",
        "mcp__gworkspace-mcp__modify_gmail_messages",
        "mcp__gworkspace-mcp__download_gmail_attachment",
        "mcp__gworkspace-mcp__send_notification_email",
        "mcp__gmail__send_message",
        // Unknown or unclassified: fail closed.
        "mcp__gworkspace-mcp__get_gmail_everything",
        "mcp__gworkspace-mcp__search_gmail_messages_v2",
        "mcp__gworkspace-mcp__list_messages_and_reply",
    ];
    let main_log = fx.home.join("main.log");
    for tool in gated {
        let v = run(&call(&fx, tool), &pm_env(&fx), &main_log);
        assert_eq!(v, Verdict::Allow, "pm {tool} (#11521)");
        for (sub, env) in subagent_calls(&fx, tool, &pm_env(&fx)) {
            let (check, _) = denied(run(&sub, &env, &log), tool);
            assert_eq!(check, "egress-family", "pm subagent {tool}");
        }
    }
    let unstamped = HookEnv {
        stamp: None,
        ..pm_env(&fx)
    };
    for (sub, env) in subagent_calls(&fx, GWORKSPACE_CLASS[0], &unstamped) {
        denied(run(&sub, &env, &log), "an unstamped subagent read");
    }
    assert_eq!(lines(&log), 0);
}

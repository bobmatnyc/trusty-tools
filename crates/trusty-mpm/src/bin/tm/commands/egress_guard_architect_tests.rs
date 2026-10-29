//! Unit tests for the #10447 Architect main-thread allow arm
//! (`egress_guard_architect.rs`). Every case runs the guard's own pipeline —
//! the #8878 floor, the family/Bash decision, this lift, the taint arm and
//! the audit — over an injected home, config and process table, so no test
//! reads or sets a process-global environment variable.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::commands::egress_guard::{verdict, with_taint};
use crate::commands::egress_guard_bash::MAX_COMMAND_BYTES;
use crate::commands::egress_guard_taint::TAINT_CHECK;
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, Fixture, allowlist, architect_env, architect_table, fixture, launch_record, payload,
    spoof_env, table,
};
use crate::commands::pm_guard_trust_anchor::{TRUST_ANCHOR_RULE, evaluate};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};

/// The fixed audit time every case writes.
const TIME: &str = "2026-10-29T12:00:00Z";

/// A message body the audit must never carry.
const BODY: &str = "BODY-SECRET-10447";

/// The guard's verdict for `call`, as `guard_once` computes it.
fn run(call: &Value, env: &HookEnv, config: impl Fn() -> MpmConfig, log: &Path) -> Verdict {
    let read: Result<Value, HookStdinError> = Ok(call.clone());
    let table = FamilyTable::builtin();
    let first = verdict(&read, &table, |p| evaluate(p, env, &config));
    let (first, allowed) = lift(first, &read, &table, env, &config);
    let first = with_taint(first, &read, env.home.as_deref(), None, || Ok(()));
    audited(first, allowed, Some(log), TIME)
}

/// The audit file of `fx`.
fn log_of(fx: &Fixture) -> PathBuf {
    fx.home.join("data").join("egress-architect-allowed.log")
}

/// The check slug of a deny, or a panic naming the allowed call.
fn denied(v: Verdict, what: &str) -> &'static str {
    match v {
        Verdict::Deny { check, .. } => check,
        Verdict::Allow => panic!("{what} must be denied"),
    }
}

/// A Slack MCP send carrying [`BODY`].
fn slack_send(fx: &Fixture) -> Value {
    let input = json!({ "channel_id": "C1", "text": BODY });
    payload(fx, "mcp__slack-mcp__slack_post_message", input)
}

/// A Bash command naming the Slack host, with [`BODY`] as an argument.
fn slack_bash(fx: &Fixture) -> Value {
    let command = format!("curl -s -d text={BODY} https://slack.com/api/chat.postMessage");
    payload(fx, "Bash", json!({ "command": command }))
}

/// 🔴 #10447: the Architect's main thread may send through every MCP family
/// and run a Bash command naming a channel host or `tm slack`; each allow
/// appends one audit line with the family, tool, time and session, and no
/// message body or command argument.
#[test]
fn architect_main_thread_may_send_slack_and_name_the_slack_host() {
    let fx = fixture();
    let env = architect_env(&fx);
    let log = log_of(&fx);
    let config = || allowlist(&fx);
    assert_eq!(run(&slack_send(&fx), &env, config, &log), Verdict::Allow);
    assert_eq!(run(&slack_bash(&fx), &env, config, &log), Verdict::Allow);
    for tool in [
        "mcp__claude_ai_Gmail__send_message",
        "mcp__notion__notion-create-pages",
        "mcp__google_chat__send_message",
    ] {
        let call = payload(&fx, tool, json!({ "text": BODY }));
        assert_eq!(run(&call, &env, config, &log), Verdict::Allow, "{tool}");
    }
    let tm_slack = payload(&fx, "Bash", json!({ "command": "tm slack send '#ops' hi" }));
    assert_eq!(run(&tm_slack, &env, config, &log), Verdict::Allow);

    let text = std::fs::read_to_string(&log).expect("audit log written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "one line per allowed call: {text}");
    assert_eq!(
        lines[0],
        format!(
            "{ALLOWED_AUDIT} time={TIME} session=s-1 family=slack \
             tool=mcp__slack-mcp__slack_post_message"
        )
    );
    assert_eq!(
        lines[1],
        format!("{ALLOWED_AUDIT} time={TIME} session=s-1 family=Slack tool=Bash")
    );
    for secret in [BODY, "chat.postMessage", "curl", "#ops"] {
        assert!(!text.contains(secret), "audit leaked `{secret}`: {text}");
    }
}

/// 🔴 #10447 (Architect 13:03Z, option b): a relaunched Architect `claude`
/// with no launch record is denied; once its launcher writes the record the
/// next call is allowed, and a subagent in that session stays denied.
#[test]
fn architect_relaunch_is_allowed_only_once_recorded() {
    let fx = fixture();
    let log = log_of(&fx);
    let config = || allowlist(&fx);
    let relaunched = HookEnv {
        claude: table(architect_table()),
        ..spoof_env(&fx)
    };
    for call in [slack_send(&fx), slack_bash(&fx)] {
        denied(
            run(&call, &relaunched, config, &log),
            "an unrecorded relaunch",
        );
    }
    assert!(!log.exists());
    // The record `tm fleet init` / `tm fleet relaunch` writes for its `claude`.
    launch_record(&fx, ARCHITECT, &fx.project);
    assert_eq!(
        run(&slack_send(&fx), &relaunched, config, &log),
        Verdict::Allow
    );
    let mut dispatched = slack_send(&fx);
    dispatched["agent_id"] = json!("agent-7");
    denied(run(&dispatched, &relaunched, config, &log), "a subagent");
    let lines = std::fs::read_to_string(&log).expect("log").lines().count();
    assert_eq!(lines, 1, "only the recorded main-thread call audits");
}

/// 🔴 #10447 (code-critic HIGH): a PM or subagent running
/// `cd <architect> && TRUSTY_MPM_SESSION_PROFILE=supervisor claude -p …`
/// carries every environment claim, but its nearest `claude` ancestor is the
/// nested one, which has no launch record, so it stays denied — even with the
/// real Architect `claude` recorded further up the tree.
#[test]
fn nested_supervisor_claude_in_the_architect_dir_is_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    launch_record(&fx, ARCHITECT, &fx.project);
    // hook 100 -> nested claude 70 -> shell 65 -> recorded Architect claude 60.
    let nested = HookEnv {
        claude: table(vec![
            (100, Some(70), false),
            (70, Some(65), true),
            (65, Some(60), false),
            (60, Some(1), true),
        ]),
        ..spoof_env(&fx)
    };
    for call in [slack_send(&fx), slack_bash(&fx)] {
        denied(
            run(&call, &nested, || allowlist(&fx), &log),
            "a nested claude -p",
        );
    }
    assert!(!log.exists());
}

/// 🔴 #10447 (bonus): the lift does not reach the #8433 taint arm; a
/// channel-tainted Architect session's Bash call to Slack is a taint deny.
#[test]
fn a_tainted_architect_bash_call_still_hits_the_taint_deny() {
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
    let check = denied(
        run(
            &slack_bash(&fx),
            &architect_env(&fx),
            || allowlist(&fx),
            &log,
        ),
        "a tainted session",
    );
    assert_eq!(check, TAINT_CHECK);
    assert!(!log.exists());
}

/// 🔴 #10447: a subagent of the Architect — an `agent_id` payload, or a
/// process with `CLAUDE_MPM_SUB_AGENT` set — stays denied, and audits nothing.
#[test]
fn architect_subagent_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    let config = || allowlist(&fx);
    for call in [slack_send(&fx), slack_bash(&fx)] {
        let mut dispatched = call.clone();
        dispatched["agent_id"] = json!("agent-7");
        let check = denied(
            run(&dispatched, &architect_env(&fx), config, &log),
            "agent_id",
        );
        assert!(["egress-family", BASH_CHECK].contains(&check), "{check}");
        let nested = HookEnv {
            sub_agent: true,
            ..architect_env(&fx)
        };
        let check = denied(run(&call, &nested, config, &log), "CLAUDE_MPM_SUB_AGENT");
        assert!(["egress-family", BASH_CHECK].contains(&check), "{check}");
    }
    assert!(!log.exists(), "a denied call must not audit an allow");
}

/// 🔴 #10447: a PM project carrying the supervisor stamp, its own
/// `profile = "supervisor"` and a launch record of its own, but absent from
/// `[supervisor] projects`, stays denied.
#[test]
fn pm_project_with_supervisor_env_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    std::fs::write(
        fx.cwd.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .expect("write pm profile");
    let mut env = architect_env(&fx);
    launch_record(&fx, ARCHITECT, &fx.cwd);
    env.project_dir = Some(fx.cwd.clone().into_os_string());
    for call in [slack_send(&fx), slack_bash(&fx)] {
        denied(run(&call, &env, || allowlist(&fx), &log), "a PM project");
    }
    assert!(!log.exists());
}

/// 🔴 #10447 (fail closed): an unreadable or malformed user config, and an
/// unreadable project `.trusty-mpm.toml`, each keep the deny. The first
/// assertion proves the on-disk config path allows when it reads.
#[test]
fn unreadable_config_or_profile_stays_denied() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = architect_env(&fx);
    let root = fx.home.join(".trusty-mpm");
    let config_file = root.join("config.toml");
    let load = || MpmConfig::load(&root);
    let grant = format!("[architect]\nprojects = [\"{}\"]\n", fx.project.display());
    std::fs::write(&config_file, grant).expect("write config");
    assert_eq!(run(&slack_send(&fx), &env, load, &log), Verdict::Allow);

    std::fs::write(&config_file, "[architect\nprojects = 1\n").expect("malformed config");
    denied(
        run(&slack_send(&fx), &env, load, &log),
        "a malformed config",
    );
    std::fs::remove_file(&config_file).expect("rm config");
    std::fs::create_dir(&config_file).expect("config as a directory");
    denied(
        run(&slack_bash(&fx), &env, load, &log),
        "an unreadable config",
    );

    let profile = fx.project.join(".trusty-mpm.toml");
    std::fs::remove_file(&profile).expect("rm profile");
    std::fs::create_dir(&profile).expect("profile as a directory");
    for call in [slack_send(&fx), slack_bash(&fx)] {
        denied(
            run(&call, &env, || allowlist(&fx), &log),
            "an unreadable profile",
        );
    }
    let lines = std::fs::read_to_string(&log).expect("log").lines().count();
    assert_eq!(lines, 1, "only the readable-config control audits");
}

/// 🔴 #10447: the #8878 floor is never lifted. A Bash command that names the
/// Slack host and overwrites a guarded project settings file, and a Write
/// setting `disableAllHooks`, stay trust-anchor denies for the Architect's
/// main thread; an over-cap Bash command naming Slack stays an egress deny.
#[test]
fn trust_anchor_deny_holds_for_the_architect_main_thread() {
    let fx = fixture();
    let log = log_of(&fx);
    let env = architect_env(&fx);
    let config = || allowlist(&fx);
    let dir = fx.cwd.join(".claude");
    std::fs::create_dir_all(&dir).expect("mkdir .claude");
    let guarded = json!({ "hooks": { "PreToolUse": [{ "matcher": "", "hooks": [
        { "type": "command", "command": "/usr/local/bin/tm hook --egress-guard", "timeout": 10 }
    ]}]}});
    std::fs::write(dir.join("settings.json"), guarded.to_string()).expect("write settings");

    let command = "curl -s https://slack.com/api/auth.test > .claude/settings.json";
    let bash = payload(&fx, "Bash", json!({ "command": command }));
    assert_eq!(
        denied(run(&bash, &env, config, &log), command),
        TRUST_ANCHOR_RULE
    );
    let path = dir.join("settings.local.json").display().to_string();
    let input = json!({ "file_path": path, "content": "{\"disableAllHooks\": true}" });
    let write = payload(&fx, "Write", input);
    assert_eq!(
        denied(run(&write, &env, config, &log), "Write"),
        TRUST_ANCHOR_RULE
    );

    let big = format!(
        "curl https://slack.com/api/x -d '{}'",
        "a".repeat(MAX_COMMAND_BYTES)
    );
    let oversize = payload(&fx, "Bash", json!({ "command": big }));
    assert_eq!(
        denied(run(&oversize, &env, config, &log), "over cap"),
        BASH_CHECK
    );
    assert!(!log.exists());
}

/// 🔴 #10447: an Architect allow whose audit line cannot be written denies;
/// a deny and an allow with no Architect fact pass through unchanged.
#[test]
fn architect_allow_needs_its_audit_line() {
    let fx = fixture();
    let allowed = ArchitectAllowed {
        family: "slack".into(),
        tool: "mcp__slack-mcp__x".into(),
        session: "s-1".into(),
    };
    let unknown = audited(Verdict::Allow, Some(allowed.clone()), None, TIME);
    assert_eq!(denied(unknown, "no data dir"), AUDIT_CHECK);
    let blocker = fx.home.join("file");
    std::fs::write(&blocker, "x").expect("write blocker");
    let unwritable = audited(
        Verdict::Allow,
        Some(allowed.clone()),
        Some(&blocker.join("log")),
        TIME,
    );
    assert_eq!(denied(unwritable, "unwritable log"), AUDIT_CHECK);
    assert_eq!(audited(Verdict::Allow, None, None, TIME), Verdict::Allow);
    let deny = Verdict::Deny {
        check: "egress-family",
        reason: "r".into(),
    };
    assert_eq!(audited(deny.clone(), Some(allowed), None, TIME), deny);
}

/// 🔴 #10447: payload fields are reduced to a safe alphabet, so a session id
/// carrying a newline cannot forge a second audit line.
#[test]
fn architect_audit_line_carries_no_body_and_no_newline() {
    let allowed = ArchitectAllowed {
        family: "slack".into(),
        tool: "mcp__slack__x y".into(),
        session: "s-1\negress-architect-allowed session=forged".into(),
    };
    let line = allowed.audit_line(TIME);
    assert!(!line.contains('\n'), "{line}");
    assert!(!line.contains("session=forged"), "{line}");
    assert_eq!(
        ArchitectAllowed {
            session: String::new(),
            ..allowed
        }
        .audit_line(TIME),
        format!("{ALLOWED_AUDIT} time={TIME} session=- family=slack tool=mcp__slack__xy")
    );
}

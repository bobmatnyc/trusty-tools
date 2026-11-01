//! `[egress_guard] mode` through the built `tm hook --egress-guard` (#10690).
//!
//! Why: the owner turned the guard's refusals off for now while keeping every
//! deny logged. Only the binary proves the three claims together: `enforce`
//! still refuses with today's text and policy id, `log` lets the same call
//! through while its record and audit POST are still written, and every fault
//! on the switch itself enforces.
//! What: a scratch `$HOME` whose `~/.trusty-mpm/config.toml` carries the mode
//! under test, a temp data directory for the denial store, and a mock daemon
//! socket that captures the audit POST. Each case runs the same denied input —
//! a Slack send, and a Write to the `config.yaml` trust anchor — under both
//! modes; the fail-closed rows run the Slack send under each bad switch.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_guard_log_mode_10690`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

/// The Slack send every family case denies.
const SLACK_SEND: &str = "mcp__slack-mcp__slack_send_message";

/// The prefix a log-mode deny's record and POST carry.
const LOG_PREFIX: &str = "[egress_guard mode=log: recorded, not enforced] ";

/// A scratch home and data directory, with `config` as the user config.
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fixture {
    /// `config` is the `config.toml` body; `None` writes no file at all.
    fn new(config: Option<&str>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let data = root.join("data");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
        if let Some(body) = config {
            std::fs::write(home.join(".trusty-mpm/config.toml"), body).expect("write config");
        }
        common::write_disk_threshold(&home, 100);
        std::fs::create_dir_all(home.join(".trusty-tools/trusty-mpm")).expect("mkdir tools");
        std::fs::create_dir_all(&data).expect("mkdir data");
        Self {
            _dir: dir,
            home,
            data,
        }
    }

    /// The `config.yaml` trust anchor (#8878).
    fn anchor(&self) -> PathBuf {
        self.home.join(".trusty-tools/trusty-mpm/config.yaml")
    }

    /// The denial store's raw lines.
    fn records(&self) -> String {
        std::fs::read_to_string(self.data.join("trusty-mpm/pm-guard-denials.jsonl"))
            .unwrap_or_default()
    }
}

/// What one guard run left: its output and the audit POST it sent.
struct Run {
    out: Output,
    post: serde_json::Value,
}

/// Run the guard over `payload` in `fx` against a mock daemon socket.
fn run(fx: &Fixture, payload: &serde_json::Value) -> Run {
    let (url, frames) = common::socket::spawn_mock(|_| {
        common::socket::Reply::Frame(common::socket::result_frame("{}"))
    });
    let mut command = common::tm_command_in(&fx.home);
    common::socket::aim(&mut command, &url);
    command
        .args(["hook", "--egress-guard"])
        .env("TRUSTY_DATA_DIR_OVERRIDE", &fx.data)
        .env_remove("TRUSTY_NO_BUG_CAPTURE")
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .current_dir(&fx.data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn tm hook --egress-guard");
    let mut pipe = child.stdin.take().expect("stdin");
    let _ = pipe.write_all(payload.to_string().as_bytes());
    drop(pipe);
    let out = child.wait_with_output().expect("wait for the guard");
    let post = std::iter::from_fn(|| frames.recv_timeout(std::time::Duration::from_secs(8)).ok())
        .find(|frame| frame["method"] == "mpm.hooks.ingest")
        .map(|frame| frame["params"].clone())
        .unwrap_or(serde_json::Value::Null);
    Run { out, post }
}

/// A main-thread `PreToolUse` payload for `tool`.
fn payload(tool: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "session_id": "s-10690",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    })
}

/// The Slack send payload.
fn slack_send() -> serde_json::Value {
    payload(SLACK_SEND, serde_json::json!({"text": "hi"}))
}

/// A Write to `path`.
fn write_to(path: &Path) -> serde_json::Value {
    payload(
        "Write",
        serde_json::json!({"file_path": path.display().to_string(), "content": "{}"}),
    )
}

/// The deny reason on stdout; panics unless `out` is exactly one deny.
fn denied_reason(out: &Output, case: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{case}: exit 0, got {:?}", out.status);
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{case}: stdout is not one JSON object ({e}): {stdout:?}"));
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{case}: {value}"
    );
    value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Assert nothing on stdout and exit 0 — Claude Code's allow.
fn assert_allowed(out: &Output, case: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{case}: exit 0, got {:?}", out.status);
    assert!(stdout.trim().is_empty(), "{case}: must allow: {stdout}");
}

/// Assert `run`'s record and POST are the log-mode copy of a `check` deny
/// whose reason contains `text`.
fn assert_logged(fx: &Fixture, run: &Run, check: &str, text: &str) {
    let records = fx.records();
    assert!(
        records.contains(&format!("check={check}")),
        "record: {records}"
    );
    assert!(
        records.contains(LOG_PREFIX),
        "record names the mode: {records}"
    );
    assert!(records.contains(text), "record keeps the reason: {records}");
    let body = &run.post["payload"];
    assert_eq!(body["pm_guard_decision"], "deny", "{}", run.post);
    assert_eq!(body["egress_guard_mode"], "log", "{}", run.post);
    let reason = body["pm_guard_reason"].as_str().unwrap_or_default();
    assert!(reason.starts_with(LOG_PREFIX), "{reason}");
    assert!(reason.contains(text), "{reason}");
}

/// Assert an enforce-mode deny's record and POST carry no mode marker.
fn assert_enforced_audit(fx: &Fixture, run: &Run, check: &str) {
    let records = fx.records();
    assert!(
        records.contains(&format!("check={check}")),
        "record: {records}"
    );
    assert!(!records.contains(LOG_PREFIX), "{records}");
    let body = &run.post["payload"];
    assert_eq!(body["pm_guard_decision"], "deny", "{}", run.post);
    assert!(body.get("egress_guard_mode").is_none(), "{}", run.post);
}

/// 🔴 #10690: the same Slack send is refused under `enforce` with today's
/// text and policy id, and allowed under `log` with its record and POST.
#[test]
fn log_mode_records_and_posts_a_family_deny_and_allows_10690() {
    let text = &format!("tm hook --egress-guard denies `{SLACK_SEND}`: it is a ");
    let fx = Fixture::new(Some("[egress_guard]\nmode = \"enforce\"\n"));
    let enforced = run(&fx, &slack_send());
    let reason = denied_reason(&enforced.out, "enforce");
    assert!(reason.contains(text), "{reason}");
    assert!(reason.contains("[policy GP-043 egress-family"), "{reason}");
    assert_enforced_audit(&fx, &enforced, "egress-family");

    let fx = Fixture::new(Some("[egress_guard]\nmode = \"log\"\n"));
    let logged = run(&fx, &slack_send());
    assert_allowed(&logged.out, "log");
    assert_logged(&fx, &logged, "egress-family", text);
}

/// 🔴 #10690 (owner instruction): the #8878 trust-anchor floor follows the
/// mode — refused under `enforce`, recorded and allowed under `log`.
#[test]
fn log_mode_records_and_posts_a_trust_anchor_deny_and_allows_10690() {
    let text = "Trust-anchor write denied (#8878)";
    let fx = Fixture::new(Some("[egress_guard]\nmode = \"enforce\"\n"));
    let enforced = run(&fx, &write_to(&fx.anchor()));
    let reason = denied_reason(&enforced.out, "enforce");
    assert!(reason.contains(text), "{reason}");
    assert!(
        reason.contains("[policy GP-006 trust-anchor-write"),
        "{reason}"
    );
    assert_enforced_audit(&fx, &enforced, "trust-anchor-write");

    let fx = Fixture::new(Some("[egress_guard]\nmode = \"log\"\n"));
    let logged = run(&fx, &write_to(&fx.anchor()));
    assert_allowed(&logged.out, "log");
    assert_logged(&fx, &logged, "trust-anchor-write", text);
}

/// 🔴 #10690 Fail-Open Check: every fault on the switch enforces — no file,
/// no section, no key, a malformed or unreadable file, a wrong type, and each
/// unknown value. An unknown value also warns once on stderr.
#[test]
fn log_mode_switch_fails_closed_to_enforce_10690() {
    let rows: &[(&str, Option<&str>, bool)] = &[
        ("no file", None, false),
        ("no section", Some("[pm_guard]\nenabled = true\n"), false),
        ("no key", Some("[egress_guard]\n"), false),
        ("malformed", Some("[egress_guard\nmode = \"log\"\n"), false),
        ("wrong type", Some("[egress_guard]\nmode = true\n"), false),
        ("Log", Some("[egress_guard]\nmode = \"Log\"\n"), true),
        ("off", Some("[egress_guard]\nmode = \"off\"\n"), true),
        ("empty", Some("[egress_guard]\nmode = \"\"\n"), true),
    ];
    for (case, config, warns) in rows {
        let fx = Fixture::new(*config);
        let result = run(&fx, &slack_send());
        let reason = denied_reason(&result.out, case);
        assert!(reason.contains("[policy GP-043"), "{case}: {reason}");
        let stderr = String::from_utf8_lossy(&result.out.stderr);
        let warnings = stderr.matches("unknown `[egress_guard] mode").count();
        assert_eq!(warnings, usize::from(*warns), "{case}: {stderr}");
    }
    // Unreadable: `config.toml` is a directory, so the read itself fails.
    let fx = Fixture::new(None);
    std::fs::create_dir(fx.home.join(".trusty-mpm/config.toml")).expect("block config");
    let result = run(&fx, &slack_send());
    denied_reason(&result.out, "unreadable");
}

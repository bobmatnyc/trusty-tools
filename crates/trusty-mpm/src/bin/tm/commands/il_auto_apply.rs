//! IL auto-apply: a turn carrying a ```` ```il ```` block updates THAT
//! session's ledger from the hook (G0b).
//!
//! Why: Bob: "all ledger updates should happen automatically on receiving an
//! IL structured response with an IL packet". The block is parsed and
//! written in process by `trusty_architect::ledger::il`, the port of the
//! `ilio.py`/`datg.py` rules; nothing shells out to Python per message.
//! What: [`turn_text`] picks the text each entry point carries: (a) a PM's
//! agent handback (`PostToolUse` of `Agent`/`Task`, the `tool_response`),
//! (b) an inbound session or Architect message and (c) the Architect's own
//! prompt (both `UserPromptSubmit`, the `prompt`). [`run`] finds the
//! session's `datg` store (`<launch dir>/.trusty-mpm/datg[/<session>]`) and
//! applies the block. A rejected or failed block is appended to the denial
//! store `list_recent_errors` reads, check `il-auto-apply:<RULE>`, and is
//! never reported as applied. [`on_hook`] never fails and never blocks the
//! turn: every outcome goes to stderr only.
//! Test: `il_auto_apply_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_architect::SystemClock;
use trusty_architect::ledger::fresh::state_dir;
use trusty_architect::ledger::il;
use trusty_architect::ledger::store::{StorePaths, launch_dir};
use trusty_architect::ledger::writer::Writer;
use trusty_common::error_capture::TRUSTY_NO_BUG_CAPTURE_ENV;
use trusty_mpm::daemon::bug_report::{
    DeniedCall, append_denial, denial_record, pm_guard_denials_path,
};

/// The Architect's session name (ilio's default `who`).
const ARCHITECT: &str = "tm-architect";

/// Which of the three entry points delivered the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// (a) `PostToolUse` of an `Agent`/`Task` call in a PM.
    AgentHandback,
    /// (b) `UserPromptSubmit` in a session other than the Architect.
    InboundMessage,
    /// (c) `UserPromptSubmit` in the Architect.
    ArchitectPrompt,
}

impl Entry {
    fn label(self) -> &'static str {
        match self {
            Self::AgentHandback => "il:agent-handback",
            Self::InboundMessage => "il:inbound-message",
            Self::ArchitectPrompt => "il:architect-prompt",
        }
    }
}

/// Where one hook run applies: the turn's cwd, the `datg` session name, the
/// denial store (`None` = the ambient one).
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) cwd: PathBuf,
    pub(crate) session: Option<String>,
    pub(crate) denials: Option<PathBuf>,
}

/// What one hook run did.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The turn carries no IL block.
    NoBlock,
    /// The session has no `datg` store.
    NoLedger,
    /// The block was written, or was a replay.
    Applied(il::Applied),
    /// Nothing (or, for `WRITE-ACT`, only the state) was written; the rule.
    Failed(String),
}

/// Text of a tool response: a string, or the `text` of its content blocks.
fn response_text(v: &Value) -> Option<String> {
    let blocks = v.as_array().or_else(|| v.get("content")?.as_array());
    match (v.as_str(), blocks) {
        (Some(s), _) => Some(s.to_owned()),
        (None, Some(b)) => Some(
            b.iter()
                .filter_map(|c| c.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// The entry point and text an event carries, when it can carry a block.
pub(crate) fn turn_text(
    event: &str,
    payload: &Value,
    session: Option<&str>,
) -> Option<(Entry, String)> {
    match event {
        "PostToolUse" => {
            let tool = payload.get("tool_name")?.as_str()?;
            matches!(tool, "Agent" | "Task")
                .then(|| response_text(payload.get("tool_response")?))
                .flatten()
                .map(|t| (Entry::AgentHandback, t))
        }
        "UserPromptSubmit" => {
            let text = payload.get("prompt")?.as_str()?.to_owned();
            let entry = if session == Some(ARCHITECT) {
                Entry::ArchitectPrompt
            } else {
                Entry::InboundMessage
            };
            Some((entry, text))
        }
        _ => None,
    }
}

/// Applies the block an event carries to the target session's store.
pub(crate) fn run(
    event: &str,
    payload: &Value,
    session_id: &str,
    target: &Target,
    writer: &Writer<'_>,
) -> Outcome {
    // See G0b IL auto-apply
    let Some((entry, text)) = turn_text(event, payload, target.session.as_deref()) else {
        return Outcome::NoBlock;
    };
    if il::fence_body(&text).is_none() {
        return Outcome::NoBlock;
    }
    let dir = state_dir(&launch_dir(&target.cwd), target.session.as_deref());
    if !dir.join("state.json").is_file() {
        return Outcome::NoLedger;
    }
    let session = target.session.clone().unwrap_or_default();
    let who = std::env::var("ILIO_WHO")
        .ok()
        .filter(|w| !w.is_empty())
        .or_else(|| target.session.clone())
        .unwrap_or_else(|| ARCHITECT.to_owned());
    let paths = StorePaths::at(dir);
    match il::apply(writer, &paths, &session, &who, &text) {
        Ok(done) => Outcome::Applied(done),
        Err(e) => {
            let packet = il::packet_id(&text).unwrap_or_default();
            record(target, entry, session_id, &packet, e.code(), &e.to_string());
            Outcome::Failed(e.code().to_owned())
        }
    }
}

/// Appends the failure where `list_recent_errors` reads denials.
fn record(target: &Target, entry: Entry, session_id: &str, packet: &str, code: &str, reason: &str) {
    if std::env::var_os(TRUSTY_NO_BUG_CAPTURE_ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let Some(path) = target.denials.clone().or_else(pm_guard_denials_path) else {
        eprintln!("tm hook: il auto-apply `{code}` not recorded: no data directory");
        return;
    };
    let check = format!("il-auto-apply:{code}");
    let cwd = target.cwd.display().to_string();
    let call = DeniedCall {
        check: &check,
        tool: entry.label(),
        command: packet,
        cwd: &cwd,
        session_id,
        reason,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut rec = denial_record(&call, now);
    rec.message = format!("il auto-apply refused {} [{code}]", entry.label());
    if let Err(err) = append_denial(&path, &rec) {
        eprintln!(
            "tm hook: il auto-apply `{code}` not recorded to {}: {err}",
            path.display()
        );
    }
}

/// The `datg` session name: `$DATG_SESSION`, else the tmux session of
/// `$TMUX_PANE` (datg `per_pm_session`). Asked only once a block is seen.
fn datg_session() -> Option<String> {
    if let Some(s) = std::env::var("DATG_SESSION").ok().filter(|s| !s.is_empty()) {
        return Some(s);
    }
    let pane = std::env::var("TMUX_PANE").ok()?;
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#{session_name}"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// The hook's call: bounded, prints to stderr only, never fails the turn.
pub(crate) fn on_hook(event: &str, payload: Option<&Value>, session_id: &str) {
    let Some(payload) = payload else { return };
    let quick = turn_text(event, payload, None).is_some_and(|(_, t)| t.contains("```il"));
    if !quick {
        return;
    }
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let target = Target {
        cwd,
        session: datg_session(),
        denials: None,
    };
    let scan = trusty_mpm::core::session_ref_publish::scan_for_credentials;
    let writer = Writer::new(&SystemClock, &scan);
    match run(event, payload, session_id, &target, &writer) {
        Outcome::Applied(a) if a.replayed => eprintln!("tm hook: IL {} already applied", a.packet),
        Outcome::Applied(a) => eprintln!("tm hook: IL {} applied, v{}", a.packet, a.v),
        Outcome::Failed(code) => eprintln!("tm hook: IL block NOT applied [{code}]"),
        Outcome::NoBlock | Outcome::NoLedger => {}
    }
}

#[cfg(test)]
#[path = "il_auto_apply_tests.rs"]
mod tests;

//! Ledger task id round trip for subagent dispatches (#11674).
//!
//! Why: a PM dispatch can name the ledger task it serves (`Task: <id>@MMDD`),
//! but nothing checked that the subagent's handback restates it, so a missing
//! or wrong id looked like any other return and was credited to whatever task
//! was current. This module is the pure half of the check: the grammar, the
//! four-way verdict and the ledger event, shared by the daemon (which binds the
//! id at dispatch) and `tm hook` (which judges the handback at `SubagentStop`).
//! What: [`dispatch_task`] reads the bound id from a dispatch prompt;
//! [`pulse_task`] reads the strict pulse from a final message; [`classify`]
//! turns a [`StopBinding`] plus the final message into one [`RoundtripEvent`]
//! whose outcome is `ok`, `missing`, `mismatch` or `unbound`. Every error arm
//! fails closed: only a bound id that the last line restates exactly is `ok`.
//! Test: `subagent_task_roundtrip_tests.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The `kind` of the notice line appended to the PM's `log.jsonl`.
pub const LEDGER_EVENT_KIND: &str = "subagent_task";

/// Payload key `tm hook` adds to a `SubagentStop` with the dispatch's
/// `tool_use_id`, read from the subagent's `.meta.json` (#11674).
///
/// Why: a foreground dispatch's `PostToolUse` (the only event that teaches the
/// daemon the `agent_id`) lands AFTER its `SubagentStop`, so the stop must
/// carry the other join key itself. A distinct name keeps it from being read
/// as the stop's own `tool_use_id`.
pub const DISPATCH_TOOL_USE_ID_KEY: &str = "dispatch_tool_use_id";

/// The binding the daemon reports for one `SubagentStop`.
///
/// What: `delegation_found` is whether any delegation matched the stop's
/// `agent_id` or dispatch `tool_use_id`; `ledger_task` is that delegation's
/// bound id. Serialized into the `POST /hooks` response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopBinding {
    /// Did a delegation match the stop?
    pub delegation_found: bool,
    /// The matched delegation's `tool_use_id`.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// The matched delegation's bound ledger task.
    #[serde(default)]
    pub ledger_task: Option<String>,
}

/// The verdict on one handback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The last line restates the bound id.
    Ok,
    /// A task was bound; the last line carries no valid pulse.
    Missing,
    /// A task was bound; the last line names a different one.
    Mismatch,
    /// No task was bound (or the binding could not be read).
    Unbound,
}

impl Outcome {
    /// The wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Mismatch => "mismatch",
            Self::Unbound => "unbound",
        }
    }
}

/// What the final line of a handback holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pulse {
    /// A strict `Task: <id>@MMDD | done: ...` line; the `<id>@MMDD` tag.
    Tagged(String),
    /// The last line starts with `Task:` but breaks the grammar.
    Malformed,
    /// No `Task:` line last.
    Absent,
}

/// One ledger event: `{ts, session, kind, tool_use_id, agent_id, bound,
/// returned, outcome}` plus `error`/`blocked` when set.
///
/// Why: the shape is the datg notice line (`{ts, session, kind, ...}`), so the
/// existing `log.jsonl` readers keep parsing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundtripEvent {
    /// RFC 3339 time.
    pub ts: String,
    /// The datg session name, or empty.
    pub session: String,
    /// Always [`LEDGER_EVENT_KIND`].
    pub kind: String,
    /// The dispatch's `tool_use_id`, when known.
    pub tool_use_id: Option<String>,
    /// The subagent's runtime `agent_id`, when known.
    pub agent_id: Option<String>,
    /// The bound `<id>@MMDD`, if any.
    pub bound: Option<String>,
    /// The `<id>@MMDD` the last line restated, if any.
    pub returned: Option<String>,
    /// The verdict.
    pub outcome: Outcome,
    /// Why the verdict could not be better, when a lookup or parse failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this stop was sent back to restate its last line.
    #[serde(default)]
    pub blocked: bool,
}

/// Is `tag` an IL task tag `<code>-T<xx>@MMDD` (task_tag.py `IL_ID`)?
///
/// What: `[a-z][a-z0-9]{1,7}-T[0-9a-km-np-z]{2}@\d{4}`. A goal (`-G`) id, a
/// bare `#N` issue number or a runtime agent id never matches.
pub fn is_task_tag(tag: &str) -> bool {
    let Some((id, stamp)) = tag.split_once('@') else {
        return false;
    };
    let stamp_ok = stamp.len() == 4 && stamp.bytes().all(|b| b.is_ascii_digit());
    let Some((code, suffix)) = id.split_once("-T") else {
        return false;
    };
    let mut code_chars = code.chars();
    let code_ok = code_chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=8).contains(&code.len())
        && code_chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let suffix_ok = suffix.len() == 2
        && suffix
            .chars()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_lowercase() && c != 'l' && c != 'o'));
    stamp_ok && code_ok && suffix_ok
}

/// The ledger task a dispatch prompt binds: the first line that is
/// `Task: <id>@MMDD`, or `None`.
///
/// Why: the PM writes the binding as a header line; quoted ids elsewhere in a
/// prompt (a PR comment to post, a goal line) are content, not a binding.
/// What: trims each line; takes the token after `Task:` up to whitespace or
/// `|`; returns it when [`is_task_tag`] accepts it.
/// Test: `subagent_task_roundtrip_dispatch_grammar`.
pub fn dispatch_task(prompt: &str) -> Option<String> {
    prompt.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Task:")?;
        let token = rest
            .trim_start()
            .split(|c: char| c.is_whitespace() || c == '|')
            .next()?;
        is_task_tag(token).then(|| token.to_owned())
    })
}

/// Read the strict pulse on the last non-empty line of `message`.
///
/// What: `Task: <id>@MMDD | done: <text>` (further `| key: value` fields are
/// allowed) is [`Pulse::Tagged`]; any other last line starting `Task:` is
/// [`Pulse::Malformed`]; anything else is [`Pulse::Absent`].
/// Test: `subagent_task_roundtrip_pulse_grammar`.
pub fn pulse_task(message: &str) -> Pulse {
    let Some(last) = message.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        return Pulse::Absent;
    };
    if !last.starts_with("Task:") {
        return Pulse::Absent;
    }
    let parsed = last
        .strip_prefix("Task: ")
        .and_then(|rest| rest.split_once(" | done: "))
        .filter(|(tag, done)| is_task_tag(tag) && !done.trim().is_empty());
    match parsed {
        Some((tag, _)) => Pulse::Tagged(tag.to_owned()),
        None => Pulse::Malformed,
    }
}

/// Judge one handback against its binding.
///
/// Why: the verdict must fail closed — a lookup that failed, a binding the
/// daemon could not find, or an unreadable message never becomes `ok`.
/// What: `binding = None` means the daemon reported nothing (down, too old,
/// or the POST failed) and yields `unbound` with an `error`. A found binding
/// with no `ledger_task` is `unbound`. Otherwise the pulse decides: `ok` when
/// it equals the bound tag, `mismatch` when it names another, `missing` when
/// it is absent or malformed (or the message itself is absent).
/// Test: `subagent_task_roundtrip_cases`,
/// `subagent_task_roundtrip_error_arms_never_record_ok`.
pub fn classify(
    binding: Option<&StopBinding>,
    agent_id: Option<&str>,
    message: Option<&str>,
    ts: String,
    session: &str,
) -> RoundtripEvent {
    let mut errors = Vec::new();
    match binding {
        None => errors.push("daemon reported no binding for this stop"),
        Some(b) if !b.delegation_found => errors.push("no delegation matches this stop"),
        Some(_) => {}
    }
    let bound = binding
        .filter(|b| b.delegation_found)
        .and_then(|b| b.ledger_task.clone());
    let pulse = match message {
        Some(m) => pulse_task(m),
        None => {
            errors.push("SubagentStop carried no last_assistant_message");
            Pulse::Absent
        }
    };
    if pulse == Pulse::Malformed {
        errors.push("last line is a malformed Task tag");
    }
    let returned = match &pulse {
        Pulse::Tagged(t) => Some(t.clone()),
        _ => None,
    };
    let outcome = match (&bound, &returned) {
        (None, _) => Outcome::Unbound,
        (Some(_), None) => Outcome::Missing,
        (Some(b), Some(r)) if b == r => Outcome::Ok,
        (Some(_), Some(_)) => Outcome::Mismatch,
    };
    RoundtripEvent {
        ts,
        session: session.to_owned(),
        kind: LEDGER_EVENT_KIND.to_owned(),
        tool_use_id: binding.and_then(|b| b.tool_use_id.clone()),
        agent_id: agent_id.map(str::to_owned),
        bound,
        returned,
        outcome,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
        blocked: false,
    }
}

/// Should this stop be sent back once to restate its last line?
///
/// What: only `missing`/`mismatch`, and never when Claude Code reports
/// `stop_hook_active` (the stop is already the answer to a block).
pub fn wants_block(event: &RoundtripEvent, stop_hook_active: bool) -> bool {
    matches!(event.outcome, Outcome::Missing | Outcome::Mismatch) && !stop_hook_active
}

/// Claim the one block a dispatch may get; `true` only for the first claim.
///
/// Why: `stop_hook_active` alone guards one chain; a marker keyed by
/// `tool_use_id`/`agent_id` stops a loop across chains (#11674: once per id).
/// What: `create_new` of `<dir>/<key>`; any error (including an existing
/// marker or an unusable key) is `false`, so a failure never blocks.
/// Test: `subagent_task_roundtrip_block_is_claimed_once`.
pub fn claim_block_once(dir: &Path, key: &str) -> bool {
    let safe = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !safe || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(key))
        .is_ok()
}

/// Where block markers live: `<tmp>/trusty-mpm-subagent-task-blocks`.
pub fn block_marker_dir() -> PathBuf {
    std::env::temp_dir().join("trusty-mpm-subagent-task-blocks")
}

/// Append `event` as one line to the ledger `log.jsonl` at `log`.
///
/// What: one `write_all` of the JSON line plus `\n` in append mode, the way
/// datg's own notices are appended. Creates no directory: a PM with no ledger
/// store has no log to write.
///
/// # Errors
/// Serialization or I/O failure.
pub fn append_ledger_event(log: &Path, event: &RoundtripEvent) -> std::io::Result<()> {
    let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new().append(true).open(log)?;
    file.write_all(line.as_bytes())
}

/// The one-line summary the PM sees for an event.
pub fn summary(event: &RoundtripEvent) -> String {
    let none = "-";
    format!(
        "subagent task round trip (#11674): outcome={} bound={} returned={} agent_id={} \
         tool_use_id={}{}",
        event.outcome.as_str(),
        event.bound.as_deref().unwrap_or(none),
        event.returned.as_deref().unwrap_or(none),
        event.agent_id.as_deref().unwrap_or(none),
        event.tool_use_id.as_deref().unwrap_or(none),
        event
            .error
            .as_deref()
            .map(|e| format!(" error={e}"))
            .unwrap_or_default(),
    )
}

/// The hook's stdout JSON for an event.
///
/// What: always a `systemMessage` with [`summary`]; when `event.blocked`, also
/// `decision: "block"` with a `reason` telling the subagent the exact last
/// line to end with.
pub fn hook_output(event: &RoundtripEvent) -> Value {
    let mut out = json!({ "systemMessage": summary(event) });
    if event.blocked {
        let bound = event.bound.as_deref().unwrap_or("<id>@MMDD");
        out["decision"] = json!("block");
        out["reason"] = json!(format!(
            "Your final message must end with the line `Task: {bound} | done: <one-line \
             summary>` (#11674). Restate your result briefly and end with that exact line."
        ));
    }
    out
}

#[cfg(test)]
#[path = "subagent_task_roundtrip_tests.rs"]
mod tests;

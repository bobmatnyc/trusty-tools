//! IL auto-apply: a block in THIS session's own turn updates THAT session's
//! ledger from the hook; a block that arrives from elsewhere is only a
//! proposal (G0b).
//!
//! Why: Bob: "all ledger updates should happen automatically on receiving an
//! IL structured response with an IL packet". Until the owner decides who may
//! write the ledger, the safe rule holds: only the session's own output
//! changes it (// See G0b IL authority (owner decision pending)). The block
//! is parsed and written in process by `trusty_architect::ledger::il`, the
//! port of the `ilio.py`/`datg.py` rules; nothing shells out to Python per
//! message.
//! What: [`turn_text`] picks the text each entry point carries. Applied: the
//! session's own last assistant message, read from the `Stop` payload's
//! `transcript_path` (screened against the Claude config directory, tail
//! read only). Never applied: (a) a PM's agent handback (`PostToolUse` of
//! `Agent`/`Task`, the `tool_response`) and (b) an inbound session or
//! Architect message or (c) the Architect's prompt (both `UserPromptSubmit`,
//! the `prompt`). Each of those is logged once per packet as
//! `il-auto-apply:PROPOSAL` in the denial store `list_recent_errors` reads,
//! with the entry point and packet id in the message and the fence body
//! (scrubbed, 600 chars) in `command`, and writes nothing. [`run`] finds the
//! session's `datg` store (`<launch dir>/.trusty-mpm/datg[/<session>]`) and
//! routes an own-turn block (`il::route`): a replay is a no-op; a block whose
//! applied fields the Rust writer covers is written in process; any other
//! goes, whole, to `ilio.py apply` (`il_auto_apply_ilio`), under the store's
//! `il::hold` lock and a pending marker, so a packet runs ilio at most once
//! (`PENDING-UNKNOWN` when an earlier run left no result; also recorded at
//! mark time, before ilio runs; a `PENDING-RESOLVED` line for the same packet
//! follows when the run reports; the denial file is append-only, never rewritten). The lock
//! wait (1.5 s) plus the ilio timeout (2 s) stay inside the `Stop` hook's 5 s
//! kill. A store-less
//! session records `NO-LEDGER` once per packet. A rejected or failed block is
//! appended to the same denial store, check `il-auto-apply:<RULE>`, and is
//! never reported as applied. [`on_hook`] never fails and never blocks the
//! turn: every outcome goes to stderr only.
//! Test: `il_auto_apply_tests.rs`.

use std::path::{Path, PathBuf};

use super::il_auto_apply_ilio::{ILIO_TIMEOUT, IlioCall, ilio_script, run_ilio};
use serde_json::Value;
use trusty_architect::SystemClock;
use trusty_architect::ledger::fresh::state_dir;
use trusty_architect::ledger::il;
use trusty_architect::ledger::store::{StorePaths, launch_dir};
use trusty_architect::ledger::writer::Writer;
use trusty_common::error_capture::TRUSTY_NO_BUG_CAPTURE_ENV;
use trusty_common::error_capture::fingerprint::compute_fingerprint;
use trusty_mpm::daemon::bug_report::{
    DeniedCall, append_denial, denial_record, pm_guard_denials_path,
};

/// The Architect's session name (ilio's default `who`).
const ARCHITECT: &str = "tm-architect";
/// How long a delivery waits for the store's apply lock.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);
// The installed `Stop` hook is killed at 5 s (`core/standalone/hooks`): the
// lock wait plus one ilio run must end well inside it.
const _: () = assert!(LOCK_WAIT.as_millis() + ILIO_TIMEOUT.as_millis() < 4000);

/// Chars of a proposal's fence body kept in its record.
const PROPOSAL_BODY_CHARS: usize = 600;

/// Cap on transcript bytes read from the tail (one final turn fits easily).
const MAX_TRANSCRIPT_TAIL: u64 = 256 * 1024;

/// Which entry point delivered the block; only `OwnTurn` may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// `Stop`: the session's own last assistant message.
    OwnTurn,
    /// (a) `PostToolUse` of an `Agent`/`Task` call in a PM.
    AgentHandback,
    /// (b) `UserPromptSubmit` in a session other than the Architect.
    InboundMessage,
    /// (c) `UserPromptSubmit` in the Architect.
    ArchitectPrompt,
}

impl Entry {
    /// Only the session's own output changes its ledger.
    fn applies(self) -> bool {
        self == Self::OwnTurn
    }

    fn label(self) -> &'static str {
        match self {
            Self::OwnTurn => "il:own-turn",
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
    /// The Claude config directory a `Stop` transcript must sit under.
    pub(crate) claude_config: Option<PathBuf>,
    /// `ilio.py`, when found; the timeout one run gets.
    pub(crate) ilio: Option<PathBuf>,
    pub(crate) ilio_timeout: std::time::Duration,
}

/// What one hook run did.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The turn carries no IL block.
    NoBlock,
    /// The session has no `datg` store.
    NoLedger,
    /// The packet was applied before; nothing was written.
    Replayed(String),
    /// The block came from outside the session's own turn: logged as a
    /// proposal (its packet), nothing written.
    Proposed(String),
    /// The Rust writer wrote the block.
    Applied(il::Applied),
    /// `ilio.py apply` exited 0 on the block: (packet, its summary).
    IlioApplied(String, String),
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

/// The last assistant message of the `Stop` payload's transcript. The path
/// is screened against `config` and only the file tail is read.
fn own_turn_text(payload: &Value, config: Option<&Path>) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let raw = payload.get("transcript_path")?.as_str()?;
    let path = trusty_mpm::core::session_record::contained_transcript_path(config?, raw)?;
    if !std::fs::metadata(&path).ok()?.is_file() {
        return None;
    }
    let mut file = std::fs::File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(MAX_TRANSCRIPT_TAIL)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_TRANSCRIPT_TAIL)
        .read_to_end(&mut bytes)
        .ok()?;
    trusty_mpm::core::idle_parking::last_assistant_text_from_transcript(&String::from_utf8_lossy(
        &bytes,
    ))
}

/// The entry point and text an event carries, when it can carry a block.
pub(crate) fn turn_text(
    event: &str,
    payload: &Value,
    session: Option<&str>,
    config: Option<&Path>,
) -> Option<(Entry, String)> {
    match event {
        "Stop" => own_turn_text(payload, config).map(|t| (Entry::OwnTurn, t)),
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
    let session_name = target.session.as_deref();
    let config = target.claude_config.as_deref();
    let Some((entry, text)) = turn_text(event, payload, session_name, config) else {
        return Outcome::NoBlock;
    };
    if il::fence_body(&text).is_none() {
        return Outcome::NoBlock;
    }
    let dir = state_dir(&launch_dir(&target.cwd), target.session.as_deref());
    let packet = il::packet_id(&text).unwrap_or_default();
    if !entry.applies() {
        // See G0b IL authority (owner decision pending): once per packet.
        if !recorded(target, "PROPOSAL", &packet) {
            let why = "an IL block from outside the session's own turn is a proposal; the \
                       ledger is changed only by the session's own turns";
            let body = il::fence_body(&text).unwrap_or_default();
            let body: String = body.chars().take(PROPOSAL_BODY_CHARS).collect();
            record(target, entry, session_id, &packet, "PROPOSAL", &body, why);
        }
        return Outcome::Proposed(packet);
    }
    if !dir.join("state.json").is_file() {
        // G0b critic: once per packet, never silently.
        if !recorded(target, "NO-LEDGER", &packet) {
            let why = format!("no datg store at {}", dir.display());
            record(
                target,
                entry,
                session_id,
                &packet,
                "NO-LEDGER",
                &packet,
                &why,
            );
        }
        return Outcome::NoLedger;
    }
    let session = target.session.clone().unwrap_or_default();
    let who = std::env::var("ILIO_WHO")
        .ok()
        .filter(|w| !w.is_empty())
        .or_else(|| target.session.clone())
        .unwrap_or_else(|| ARCHITECT.to_owned());
    let paths = StorePaths::at(dir);
    let fail = |code: &str, why: &str| {
        record(target, entry, session_id, &packet, code, &packet, why);
        Outcome::Failed(code.to_owned())
    };
    // G0b critic: one delivery at a time from route to the replay marker.
    let _held = match il::hold(&paths, LOCK_WAIT) {
        Ok(f) => f,
        Err(e) => return fail(e.code(), &e.to_string()),
    };
    match il::pending_unknown(&paths, &packet) {
        Ok(true) => {
            let why = "an earlier ilio.py apply of this packet has no result; not re-run";
            return fail("PENDING-UNKNOWN", why);
        }
        Ok(false) => {}
        Err(e) => return fail(e.code(), &e.to_string()),
    }
    // G0b gap ruling: ilio applies the whole block, or Rust a covered one.
    match il::route(&paths, &text, writer.scan) {
        Err(e) => fail(e.code(), &e.to_string()),
        Ok(il::Route::Replay(p)) => Outcome::Replayed(p),
        Ok(il::Route::Rust) => match il::apply(writer, &paths, &session, &who, &text) {
            Ok(done) if done.replayed => Outcome::Replayed(done.packet),
            Ok(done) => Outcome::Applied(done),
            Err(e) => fail(e.code(), &e.to_string()),
        },
        Ok(il::Route::Ilio { packet, fields }) => {
            if let Err(e) = il::mark_pending(&paths, &packet) {
                return fail("WRITE", &format!("pending marker not written: {e}"));
            }
            // Before ilio runs: a hook killed mid-run still leaves a record.
            let why = format!(
                "ilio.py apply of this packet started; marker {}/{packet} stays until it \
                 reports; a record here with no applied result means the run was cut off",
                il::PENDING_DIR
            );
            record(
                target,
                entry,
                session_id,
                &packet,
                "PENDING-UNKNOWN",
                &packet,
                &why,
            );
            let call = IlioCall {
                script: target.ilio.as_deref(),
                project: &launch_dir(&target.cwd),
                session: &session,
                who: &who,
                timeout: target.ilio_timeout,
            };
            match run_ilio(&call, &text) {
                Err((code, why)) => {
                    // A timeout or crash may have written; its marker stays.
                    if !matches!(code.as_str(), "ILIO-TIMEOUT" | "ILIO-EXIT") {
                        let _ = il::clear_pending(&paths, &packet);
                        let why = "ilio.py reported a failure before writing";
                        resolve_pending(target, entry, session_id, &packet, why);
                    }
                    fail(&code, &format!("{why} (fields {})", fields.join(",")))
                }
                Ok(summary) => {
                    if let Err(e) = il::mark_applied(&paths, &packet) {
                        eprintln!("tm hook: IL {packet} applied; replay marker not written: {e}");
                    } else if let Err(e) = il::clear_pending(&paths, &packet) {
                        eprintln!("tm hook: IL {packet} applied; pending marker kept: {e}");
                    }
                    let why = "ilio.py apply reported success";
                    resolve_pending(target, entry, session_id, &packet, why);
                    Outcome::IlioApplied(packet, summary)
                }
            }
        }
    }
}

/// True when the denial store already holds `il-auto-apply:<code>` for
/// `packet`.
fn recorded(target: &Target, code: &str, packet: &str) -> bool {
    let check = format!("check=il-auto-apply:{code} ");
    target
        .denials
        .clone()
        .or_else(pm_guard_denials_path)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|t| t.lines().any(|l| l.contains(&check) && l.contains(packet)))
}

/// Appends `il-auto-apply:PENDING-RESOLVED <packet>`: the run its earlier
/// `PENDING-UNKNOWN` announced has reported. The store only grows; every
/// reader pairs the two (`drop_resolved_pending`, which `list_recent_errors` applies).
fn resolve_pending(target: &Target, entry: Entry, session_id: &str, packet: &str, why: &str) {
    record(
        target,
        entry,
        session_id,
        packet,
        "PENDING-RESOLVED",
        packet,
        why,
    );
}

/// Appends the failure where `list_recent_errors` reads denials.
///
/// `command` is the field `denial_record` scrubs of secrets; the packet id
/// goes in the message so each proposal has its own fingerprint.
fn record(
    target: &Target,
    entry: Entry,
    session_id: &str,
    packet: &str,
    code: &str,
    command: &str,
    reason: &str,
) {
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
        command,
        cwd: &cwd,
        session_id,
        reason,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut rec = denial_record(&call, now);
    rec.message = if code == "PROPOSAL" {
        format!("il auto-apply refused {} [{code}] {packet}", entry.label())
    } else {
        format!("il auto-apply refused {} [{code}]", entry.label())
    };
    if code == "PROPOSAL" {
        // The fingerprint normaliser erases hex runs, and a packet id is
        // one: spell it in letters it keeps, so each proposal is its own bug.
        let spelled: String = packet
            .chars()
            .map(|c| c.to_digit(16).map_or(c, |d| char::from(b'g' + d as u8)))
            .collect();
        let keyed = format!("{} {spelled}", rec.message);
        rec.fingerprint = compute_fingerprint(&rec.crate_target, &keyed, None, None);
    }
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
    // #8443: `$TMUX_PANE` is a `%N` id, which the helper leaves bare; any other
    // value is rendered exact.
    let target = trusty_common::tmux::exact_window_target(&pane);
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &target, "#{session_name}"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// The hook's call: bounded, prints to stderr only, never fails the turn.
pub(crate) fn on_hook(event: &str, payload: Option<&Value>, session_id: &str) {
    let Some(payload) = payload else { return };
    let config = trusty_mpm::core::session_record::claude_config_dir();
    let quick = turn_text(event, payload, None, config.as_deref())
        .is_some_and(|(_, t)| t.contains("```il"));
    if !quick {
        return;
    }
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let env = |k: &str| std::env::var(k).ok();
    let target = Target {
        cwd,
        session: datg_session(),
        denials: None,
        claude_config: config,
        ilio: ilio_script(&env, dirs::home_dir().as_deref()),
        ilio_timeout: ILIO_TIMEOUT,
    };
    let scan = trusty_mpm::core::session_ref_publish::scan_for_credentials;
    let writer = Writer::new(&SystemClock, &scan);
    match run(event, payload, session_id, &target, &writer) {
        Outcome::Replayed(p) => eprintln!("tm hook: IL {p} already applied"),
        Outcome::Applied(a) => eprintln!("tm hook: IL {} applied, v{}", a.packet, a.v),
        Outcome::IlioApplied(p, s) => eprintln!("tm hook: IL {p} applied by ilio.py: {s}"),
        Outcome::Proposed(p) => eprintln!("tm hook: IL {p} is a proposal, not applied"),
        Outcome::Failed(code) => eprintln!("tm hook: IL block NOT applied [{code}]"),
        Outcome::NoBlock | Outcome::NoLedger => {}
    }
}

#[cfg(test)]
#[path = "il_auto_apply_tests.rs"]
mod tests;

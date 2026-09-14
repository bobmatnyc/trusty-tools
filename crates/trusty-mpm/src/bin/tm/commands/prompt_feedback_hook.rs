//! `tm hook --prompt-feedback` — the `Stop`/`SubagentStop` capture arm (#7688).
//!
//! Why: the addendum the PM and every agent are asked to emit lives in a
//! transcript Claude Code deletes on its own schedule and that no tm command
//! reads. This hook is the harvest: it fires as a turn ends, finds the final
//! assistant message, and appends the `## Prompt feedback` section — if there
//! is one — to the ledger.
//!
//! It also measures a stopping SUBAGENT's report against the BASE-AGENT word
//! cap (owner ruling 2026-09-14) and warns on an overrun — see
//! [`warn_over_cap_report`]. Warn-only: nothing here rewrites or blocks.
//!
//! Registered ONLY when the flag is on, by
//! [`prompt_feedback_hooks`](trusty_mpm::core::prompt_self_improvement), so a
//! project with the feature off pays nothing: no matcher in its
//! `.claude/settings.json`, no process spawned per turn.
//!
//! 🔴 FAIL-OPEN, TOTAL. Every arm of this command exits 0. A `Stop` hook that
//! exits non-zero is surfaced to the operator as a broken session, and a prompt
//! critique can never be worth that. An unreadable transcript, an absent
//! transcript path, a malformed payload, an unwritable ledger — each logs one
//! `warn` to stderr and exits 0 having written nothing. Pinned by
//! `an_unreadable_transcript_writes_nothing`.
//!
//! Kept in its own file rather than folded into `misc.rs` or `divert_check.rs`
//! for the reason #6887 split `divert_check` out: the payload read, the
//! transcript scan, and the row build are one concern, and both neighbours sit
//! near the 500-SLOC production cap.
//!
//! Test: the inline suite below.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::prompt_feedback::{FeedbackRow, PM_AGENT_TYPE, UNKNOWN_SUBAGENT_TYPE};

/// How much of a transcript's tail is scanned for the final assistant message.
///
/// Why: the same bound, and the same reason, as
/// [`TRANSCRIPT_TAIL_BYTES`](trusty_mpm::core::transcript_usage) — a long
/// session's transcript reaches hundreds of megabytes, and this runs at the end
/// of EVERY turn. The final assistant message is by definition at the end, so a
/// tail read loses nothing.
/// What: 2 MiB, ample for one turn's content blocks.
/// Test: `reads_the_final_message_from_the_tail_of_a_large_transcript`.
const TRANSCRIPT_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Hand-back word cap for a subagent report that names no failure.
///
/// Why: owner ruling 2026-09-14 (token savings) put a number on BASE-AGENT's
/// "verbosity scales with what went wrong". A qualitative rule cannot be
/// measured, so nothing ever told an operator which agent overruns it.
/// What: 300 prose words, the same number BASE-AGENT.md states.
/// Test: `an_over_cap_clean_report_warns_and_records_the_row`.
const CLEAN_REPORT_WORD_CAP: usize = 300;

/// Hand-back word cap for a report that names a failure.
///
/// What: 600 prose words — a failing run legitimately owes more explanation.
/// Test: `a_failure_report_is_measured_against_the_higher_cap`.
const FAILURE_REPORT_WORD_CAP: usize = 600;

/// Prose markers that put a report on [`FAILURE_REPORT_WORD_CAP`].
///
/// Why: the hook cannot know whether the run failed, so it reads the report's
/// own words. Matched against PROSE ONLY — every passing `cargo test` block
/// contains `0 failed`, so scanning fenced output would put every report on
/// the higher cap. Ties go to the HIGHER cap: a warn-only measurement that
/// cries wolf gets ignored, which costs more than a missed warning.
/// What: lowercase substrings, scanned case-insensitively.
/// Test: `a_failure_report_is_measured_against_the_higher_cap`.
const FAILURE_MARKERS: [&str; 4] = ["needs attention", "failed", "failure", "blocked"];

/// Run the capture. Always exits 0.
///
/// What: reads the hook payload from stdin, resolves the transcript, extracts
/// the section, and appends a row. Returns `Ok(())` unconditionally so the
/// binary's exit status is 0 whatever happened; every failure is a `warn`.
/// Test: `an_unreadable_transcript_writes_nothing`,
/// `a_stop_payload_writes_a_pm_row`, `a_subagent_stop_payload_records_its_type`,
/// `a_subagent_stop_without_an_agent_type_is_not_the_pm`.
pub(crate) async fn prompt_feedback_hook() -> anyhow::Result<()> {
    let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    let payload = read_stdin_payload();
    capture_into(&root, payload.as_ref());
    Ok(())
}

/// Read the hook payload from stdin.
///
/// What: `None` when stdin is empty or not JSON — both are "nothing to do",
/// not errors.
/// Test: via `capture_into`'s tests, which supply the payload directly.
fn read_stdin_payload() -> Option<Value> {
    use std::io::Read as _;
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return None;
    }
    serde_json::from_str(&raw).ok()
}

/// The capture, against a caller-named framework root and payload.
///
/// Why (#5544): the entry point resolves its ledger from the process home
/// directory. Naming the root is what lets the tests below assert the write on
/// a tempdir without a PROCESS-GLOBAL `$HOME` mutation — which this binary's
/// `env_isolation_tests.rs` ratchet forbids outright for a new file.
/// What: returns whether a `## Prompt feedback` row was appended. `false` for
/// every fail-open arm: no payload, no transcript path, an unreadable
/// transcript, no `## Prompt feedback` section, or a ledger that would not take
/// the write. A report-length row from [`warn_over_cap_report`] is a separate,
/// independent append and does NOT change this return value.
/// Test: the inline suite below.
pub(crate) fn capture_into(framework_root: &Path, payload: Option<&Value>) -> bool {
    let Some(payload) = payload else {
        tracing::warn!("prompt-feedback hook received no usable payload; nothing captured");
        return false;
    };

    let Some(transcript) = transcript_path(payload) else {
        tracing::warn!("prompt-feedback hook payload names no transcript; nothing captured");
        return false;
    };

    let Some(message) = final_assistant_message(Path::new(transcript)) else {
        tracing::warn!(
            "prompt-feedback hook could not read a final assistant message from {transcript}; \
             nothing captured"
        );
        return false;
    };

    warn_over_cap_report(framework_root, payload, &message);

    let Some(feedback) = trusty_mpm::core::prompt_feedback::extract_feedback(&message) else {
        // The overwhelmingly common case for a turn that simply did not emit
        // the section. Not a warning — it is not a failure.
        tracing::debug!("no prompt-feedback section in the final message; nothing captured");
        return false;
    };

    trusty_mpm::core::prompt_feedback::append_row(framework_root, &row_for(payload, feedback))
}

/// Build one ledger row for `payload` carrying `feedback`.
///
/// Why: both writers — the `## Prompt feedback` capture and the report-length
/// observation below — book their row against the same session, agent type and
/// prompt digest, so the shape is derived once rather than spelled twice.
/// Test: via `a_stop_payload_writes_a_pm_row` and
/// `an_over_cap_clean_report_warns_and_records_the_row`.
fn row_for(payload: &Value, feedback: String) -> FeedbackRow {
    FeedbackRow {
        ts: chrono::Utc::now().to_rfc3339(),
        session_id: payload
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_type: agent_type_for(payload).to_string(),
        prompt_digest: prompt_digest(payload),
        feedback,
    }
}

/// Measure a stopping subagent's report against its cap; warn when over.
///
/// Why: BASE-AGENT.md caps a hand-back report at [`CLEAN_REPORT_WORD_CAP`]
/// words clean, [`FAILURE_REPORT_WORD_CAP`] with failures (owner ruling
/// 2026-09-14, token savings). Nothing measured compliance, so an agent that
/// wrote 2,000 words of narration cost the operator that context every run
/// with no signal. This is MEASUREMENT ONLY — no shipped mechanism rewrites a
/// subagent's output at `SubagentStop`, and truncating a report the PM has
/// already read would change nothing.
///
/// 🔴 WARN-ONLY, FAIL-OPEN. It never blocks, never rewrites, never returns an
/// error, and its caller exits 0 regardless — a `SubagentStop` hook that fails
/// is surfaced to the operator as a broken session, and a length observation
/// can never be worth that. A malformed payload reaches neither this function
/// nor a warning: `capture_into` has already returned.
/// What: skips a plain `Stop` (the PM's own turn is not a hand-back report),
/// counts the prose words outside fenced blocks, and on an overrun writes one
/// line to stderr and appends the same line to the prompt-feedback ledger —
/// the observation store this hook already writes.
/// Test: `an_over_cap_clean_report_warns_and_records_the_row`,
/// `a_fenced_block_does_not_count_toward_the_report_cap`,
/// `a_failure_report_is_measured_against_the_higher_cap`,
/// `a_malformed_payload_produces_no_length_warning`.
fn warn_over_cap_report(framework_root: &Path, payload: &Value, message: &str) {
    // A `Stop` is the main session ending, not an agent handing back.
    if payload.get("hook_event_name").and_then(Value::as_str) == Some("Stop") {
        return;
    }
    let Some(warning) = over_cap_warning(agent_type_for(payload), message) else {
        return;
    };
    // `eprintln!`, not `tracing::warn!`: `tm hook` installs no subscriber
    // (`tracing_setup::wants_cli_diagnostics` does not cover it), so a
    // `tracing` event here would reach no one.
    eprintln!("{warning}");
    trusty_mpm::core::prompt_feedback::append_row(framework_root, &row_for(payload, warning));
}

/// The one-line overrun warning for `message`, or `None` when it is within cap.
///
/// What: names `agent_type`, the prose word count, and the cap that applied —
/// the three facts an operator needs to act without opening the transcript.
/// Test: `an_over_cap_clean_report_warns_and_records_the_row`.
fn over_cap_warning(agent_type: &str, message: &str) -> Option<String> {
    let prose = prose_outside_fences(message);
    let words = prose.split_whitespace().count();
    let cap = applicable_cap(&prose);
    (words > cap).then(|| {
        format!(
            "report-length: `{agent_type}` handed back {words} prose words, over the \
             {cap}-word cap (owner ruling 2026-09-14); fenced output is not counted"
        )
    })
}

/// The lines of `message` that lie outside fenced code blocks.
///
/// Why: raw gate output is the evidence an agent owes and is explicitly
/// exempt from the cap, so counting it would penalise the reports that comply.
/// What: joins every line outside a ``` fence. An UNTERMINATED fence swallows
/// the rest of the message, which undercounts — the safe direction for a
/// warn-only measurement.
/// Test: `a_fenced_block_does_not_count_toward_the_report_cap`.
fn prose_outside_fences(message: &str) -> String {
    let mut inside = false;
    let mut prose = String::new();
    for line in message.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    prose
}

/// Which cap `prose` is measured against.
///
/// What: [`FAILURE_REPORT_WORD_CAP`] when any [`FAILURE_MARKERS`] entry appears
/// case-insensitively, else [`CLEAN_REPORT_WORD_CAP`].
/// Test: `a_failure_report_is_measured_against_the_higher_cap`.
fn applicable_cap(prose: &str) -> usize {
    let lowered = prose.to_lowercase();
    if FAILURE_MARKERS.iter().any(|m| lowered.contains(m)) {
        FAILURE_REPORT_WORD_CAP
    } else {
        CLEAN_REPORT_WORD_CAP
    }
}

/// Which agent type this event's row is booked under (#7702).
///
/// Why: the row's `agent_type` was the payload's when present and
/// [`PM_AGENT_TYPE`] otherwise, regardless of which event fired. A
/// `SubagentStop` that names no type then lands in the PM's own count — and
/// `tm prompt-feedback --summary` exists to answer "which agent type produces
/// the most complaints", so one mislabelled row is a wrong answer to the only
/// question the command asks. The daemon's delegation tracker already rules
/// that an absent `agent_type` on a stop is evidence about nothing
/// (`a_stop_without_an_agent_type_reconciles_nothing`); this applies the same
/// ruling to the ledger.
/// What: the payload's non-empty `agent_type` when it has one; else
/// [`PM_AGENT_TYPE`] for `hook_event_name == "Stop"` and
/// [`UNKNOWN_SUBAGENT_TYPE`] for every other event name, absent names
/// included — anything that is not a plain `Stop` is not the main session, so
/// guessing the PM there is the one outcome that must never happen.
/// Test: `a_stop_payload_writes_a_pm_row`,
/// `a_subagent_stop_payload_records_its_type`,
/// `a_subagent_stop_without_an_agent_type_is_not_the_pm`,
/// `an_event_with_no_name_is_not_the_pm`.
fn agent_type_for(payload: &Value) -> &str {
    let named = payload
        .get("agent_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|agent| !agent.is_empty());
    if let Some(agent) = named {
        return agent;
    }
    match payload.get("hook_event_name").and_then(Value::as_str) {
        Some("Stop") => PM_AGENT_TYPE,
        _ => UNKNOWN_SUBAGENT_TYPE,
    }
}

/// Which transcript this event's final message lives in.
///
/// Why: a `SubagentStop` carries BOTH its own transcript
/// (`agent_transcript_path`) and the parent's (`transcript_path`), and the
/// feedback being harvested is the SUBAGENT's. Reading the parent there would
/// attribute the parent's last message to the subagent's `agent_type`.
/// What: `agent_transcript_path` when present, else `transcript_path`.
/// Test: `a_subagent_stop_reads_its_own_transcript_not_the_parents`.
fn transcript_path(payload: &Value) -> Option<&str> {
    payload
        .get("agent_transcript_path")
        .and_then(Value::as_str)
        .or_else(|| payload.get("transcript_path").and_then(Value::as_str))
}

/// sha256 of the session's compiled prompt, when one can be read.
///
/// Why: the row is only actionable if it says WHICH prompt was being critiqued.
/// The compiled PM prompt is written per session by
/// [`refresh_compiled_prompt`](trusty_mpm::core::instruction_pipeline), so its
/// digest joins a critique to the exact bytes that drew it.
/// What: `Some(hex)` when the session's compiled prompt is readable, `None`
/// otherwise — an absent digest is a legitimate row, not a failure. A subagent
/// has no compiled-prompt artifact of its own, so its row carries the session's.
/// Test: `records_the_compiled_prompt_digest_when_one_exists`,
/// `records_no_digest_when_the_compiled_prompt_is_absent`.
fn prompt_digest(payload: &Value) -> Option<String> {
    let cwd = payload.get("cwd").and_then(Value::as_str)?;
    let session = payload.get("session_id").and_then(Value::as_str)?;
    let path =
        trusty_mpm::core::instruction_pipeline::compiled_prompt_path(Path::new(cwd), session);
    let bytes = std::fs::read(path).ok()?;
    Some(sha256_hex(&bytes))
}

/// Lowercase hex sha256 of `bytes`.
///
/// Test: via `records_the_compiled_prompt_digest_when_one_exists`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The text of the last assistant message in a JSON-Lines transcript.
///
/// Why: Claude Code writes one transcript LINE per content block, so the final
/// assistant MESSAGE is the concatenation of the trailing run of assistant
/// text blocks, not the single last line. Taking only the last line would
/// capture whatever block happened to be last and miss the section whenever the
/// response ended in more than one block.
/// What: reads at most [`TRANSCRIPT_TAIL_BYTES`] from the end, keeps the
/// assistant text blocks belonging to the LAST `message.id` seen, and joins
/// them in file order. `None` when the file cannot be opened or holds no
/// assistant text.
/// Test: `reads_the_final_message_from_the_tail_of_a_large_transcript`,
/// `joins_multi_block_assistant_messages`,
/// `an_unreadable_transcript_writes_nothing`.
fn final_assistant_message(path: &Path) -> Option<String> {
    use std::io::{BufRead as _, Seek as _};

    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut reader = std::io::BufReader::new(file);
    if len > TRANSCRIPT_TAIL_BYTES {
        reader
            .seek(std::io::SeekFrom::Start(len - TRANSCRIPT_TAIL_BYTES))
            .ok()?;
        // The seek almost certainly landed mid-line; discard that partial line.
        let mut discard = String::new();
        reader.read_line(&mut discard).ok()?;
    }

    let mut last_id: Option<String> = None;
    let mut blocks: Vec<String> = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let message = value.get("message").unwrap_or(&Value::Null);
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last_id.as_deref() != Some(id.as_str()) {
            last_id = Some(id);
            blocks.clear();
        }
        for text in assistant_text_blocks(message) {
            blocks.push(text);
        }
    }

    (!blocks.is_empty()).then(|| blocks.join("\n"))
}

/// Every `text` block of one assistant message body.
///
/// What: the `content` array's `{"type":"text","text":…}` entries, in order. A
/// `tool_use` block carries no prose and is skipped.
/// Test: via `joins_multi_block_assistant_messages`.
fn assistant_text_blocks(message: &Value) -> Vec<String> {
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "prompt_feedback_hook_tests.rs"]
mod tests;

//! `tm hook --completion-evidence` — the warn-only `Stop` check (#10575).
//!
//! Why: the completion standard asks a done claim to carry an evidence map
//! and bans an invented size target; a rule nothing measures goes unseen.
//! What: on `Stop`, reads the PM's final assistant message with the
//! transcript capture of `prompt_feedback_hook` (#7688) and runs
//! [`warnings_for`]. Each warning goes to stderr and, joined, into a
//! `systemMessage` the operator sees.
//!
//! WARN-ONLY, FAIL-OPEN: it never prints a `decision`, never exits non-zero.
//! A missing payload, transcript or message logs one stderr line and prints
//! nothing. INVENTED_TARGET and OWNER-REQUESTED-LIMIT are warnings here
//! (owner ruling Dr8 C).
//! Test: `completion_evidence_hook_tests.rs`;
//! `tests/tm_hook_completion_evidence_10575.rs` through the binary.

use std::io::Read as _;
use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::completion_evidence::warnings_for;

use super::prompt_feedback_hook::final_assistant_message;

/// The most stdin the hook reads.
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// Log prefix of every stderr line.
pub(crate) const LOG_PREFIX: &str = "trusty-mpm: completion-evidence (#10575)";

/// Run the hook against stdin. Always returns.
pub(crate) fn completion_evidence_hook() {
    let mut raw = String::new();
    let payload = std::io::stdin()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_string(&mut raw)
        .ok()
        .and_then(|_| serde_json::from_str::<Value>(&raw).ok());
    let (stdout, logs) = respond(payload.as_ref());
    for line in logs {
        eprintln!("{line}");
    }
    if let Some(out) = stdout {
        println!("{out}");
    }
}

/// The stdout JSON (if any) and the stderr lines for one hook call.
///
/// What: `(None, [log])` for no payload, a non-`Stop` event, no transcript
/// path or no readable final message; `(None, [])` for a message that meets
/// the standard; otherwise `(Some({"systemMessage": …}), warnings)`.
/// Test: `a_done_claim_without_a_map_warns_through_the_hook`,
/// `a_compliant_message_prints_nothing`, `each_failure_arm_logs_and_prints_nothing`.
pub(crate) fn respond(payload: Option<&Value>) -> (Option<String>, Vec<String>) {
    let Some(payload) = payload else {
        return (
            None,
            vec![format!("{LOG_PREFIX}: no usable payload; no check run")],
        );
    };
    if payload.get("hook_event_name").and_then(Value::as_str) != Some("Stop") {
        return (None, Vec::new());
    }
    let Some(transcript) = payload.get("transcript_path").and_then(Value::as_str) else {
        return (
            None,
            vec![format!(
                "{LOG_PREFIX}: payload names no transcript; no check run"
            )],
        );
    };
    let Some(message) = final_assistant_message(Path::new(transcript)) else {
        return (
            None,
            vec![format!(
                "{LOG_PREFIX}: no final assistant message in {transcript}; no check run"
            )],
        );
    };
    let warnings: Vec<String> = warnings_for(&message)
        .into_iter()
        .map(|w| format!("{LOG_PREFIX}: {w}"))
        .collect();
    if warnings.is_empty() {
        return (None, Vec::new());
    }
    let out = serde_json::json!({ "systemMessage": warnings.join("\n") }).to_string();
    (Some(out), warnings)
}

#[cfg(test)]
#[path = "completion_evidence_hook_tests.rs"]
mod tests;

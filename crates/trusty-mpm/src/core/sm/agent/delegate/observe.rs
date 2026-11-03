//! OBSERVE + VERIFY interpretation of a session's pane state (§3.4 phases 4–5).
//!
//! Why: phases 4 (OBSERVE) and 5 (VERIFY) read a session's pane/record state and
//! decide (a) its current verification [`SessionTaskState`] and (b) whether the
//! pane carries gate-satisfying EVIDENCE (a PR URL, captured test output, a
//! diff/write confirmation — §3.5). The spec says the SM can interpret raw panes
//! WITHOUT provider inference ("the session-manager-driver skill's inference
//! applies"), so this interpretation is DETERMINISTIC heuristics over the session
//! JSON — no LLM call, fully unit-testable. Keeping it here (separate from the
//! orchestrator) keeps each file under the SLOC cap and makes the gate logic
//! auditable in one place.
//! What: [`ObservedState`] (the interpreted state + optional captured evidence)
//! and [`interpret_session`] (session JSON + [`TaskExpectation`] →
//! `ObservedState`). The text rules live in the sibling `evidence` module
//! ([`scan_evidence`]). The orchestrator turns an [`ObservedState`] into a
//! goal-store [`SessionUpdate`](crate::core::sm::SessionUpdate).
//! Test: `observe_tests.rs` covers running/failed/verified interpretation and
//! evidence extraction (PR URL, test-pass output) vs. no-evidence.

use serde_json::Value;

use crate::core::sm::goals::SessionTaskState;

use super::evidence::drop_task_echo;
pub use super::evidence::{TaskExpectation, scan_evidence};

/// The interpreted outcome of observing one session (§3.4 phases 4–5).
///
/// Why: OBSERVE/VERIFY need to convey BOTH the interpreted verification state and
/// any captured evidence in one value so the orchestrator can build a single
/// goal-store update. Crucially, `Verified` is only ever reached WITH evidence —
/// the verification gate (§3.5) — so this type couples them: a `Verified` state
/// always carries `Some(evidence)`.
/// What: `state` is the interpreted [`SessionTaskState`]; `evidence` is the
/// gate-satisfying snippet (PR URL / test output / diff) when one was observed,
/// else `None`.
/// Test: `observe_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedState {
    /// The interpreted verification state of the linked task.
    pub state: SessionTaskState,
    /// Gate-satisfying evidence captured from the pane, if any (§3.5).
    pub evidence: Option<String>,
}

/// Interpret a session's record/pane JSON into an [`ObservedState`] (deterministic).
///
/// Why: phase 4 turns the raw session JSON (the `SessionControl::get` body) into a
/// verification state WITHOUT an LLM (§3.4 — the pane heuristic applies). The
/// interpretation is conservative: a session is only `Verified` when the pane
/// carries observed EVIDENCE tied to its task (§3.5), never merely because it
/// reports "done", and never when the record says it failed (#10721 W4).
/// What: reads the nested `session` object (the control surface wraps the record
/// as `{ "session": { … } }`). If the record `state` is a terminal failure
/// (`errored`/`dead`/`failed`/`killed`) → `Failed` with no evidence, before any
/// scan. Otherwise scans the RAW pane/output string (task echoes removed), or —
/// with no pane field — every string value in the payload except values that
/// embed the whole task; evidence → `Verified`. Else a brand-new record stays
/// `Launched` and anything else is `Running` (in flight, not verified).
/// Test: `observe_tests.rs::interpret_running`, `interpret_failed`,
/// `interpret_verified_with_pr_url`, `interpret_no_evidence_stays_unverified`,
/// `interpret_evidence_from_raw_pane`, `failed_state_with_evidence_is_failed`.
pub fn interpret_session(session_json: &Value, expect: &TaskExpectation) -> ObservedState {
    // The control surface returns `{ "session": { … } }`; tolerate both shapes.
    let record = session_json.get("session").unwrap_or(session_json);

    let state_str = record
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();

    // #10721 W4: a failed record wins over any evidence line in its pane.
    if is_failed_state(&state_str) {
        return ObservedState {
            state: SessionTaskState::Failed,
            evidence: None,
        };
    }

    // Evidence is scanned over the RAW pane/output string (issue #1311 review):
    // the escaped compact JSON can garble a captured PR URL or test line.
    // #10721: the pane echoes the task the SM typed, so those lines are dropped;
    // the no-pane fallback reads raw string values instead of compact JSON.
    let haystack = match extract_pane_text(record) {
        Some(pane) => drop_task_echo(&pane, expect.task()),
        None => record_strings(session_json, expect.task()),
    };
    if let Some(evidence) = scan_evidence(&haystack, expect) {
        return ObservedState {
            state: SessionTaskState::Verified,
            evidence: Some(evidence),
        };
    }

    let interpreted =
        if state_str.is_empty() || state_str == "created" || state_str == "provisioning" {
            // No activity yet observed.
            SessionTaskState::Launched
        } else {
            // Active / running / stopped-without-evidence: in flight, not verified.
            SessionTaskState::Running
        };

    ObservedState {
        state: interpreted,
        evidence: None,
    }
}

/// The ordered field names that may hold a session's raw pane / captured output.
///
/// Why: different control surfaces (the tmux-backed session manager, the test mock)
/// name the captured pane text differently; checking a small ordered set finds the
/// raw string regardless of which the surface used, most-specific first.
const PANE_FIELDS: &[&str] = &["pane", "output", "pane_text", "tail", "stdout"];

/// Extract the RAW pane/output string from a session record, if present.
///
/// Why: the evidence scanner must run over the UNESCAPED pane text, not the
/// compact JSON serialization (which adds `\"`/`}}` framing and escapes newlines,
/// garbling a captured PR URL or test line — issue #1311 review). Pulling the raw
/// string value out lets the scanner see exactly what the session printed.
/// What: returns the first present, non-empty string value among [`PANE_FIELDS`]
/// on the record; `None` when the record carries no recognised pane field (the
/// caller then falls back to [`record_strings`] so evidence elsewhere in the
/// payload is still found).
/// Test: `observe_tests.rs::interpret_evidence_from_raw_pane`,
/// `scan_pr_url_strips_json_framing` (the fallback path stays green).
fn extract_pane_text(record: &Value) -> Option<String> {
    for field in PANE_FIELDS {
        if let Some(s) = record.get(*field).and_then(Value::as_str)
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Every string value in `payload`, one per line, minus values embedding the task.
///
/// Why: with no pane field the scanner still reads the whole payload (issue
/// #1311), but a record field that carries the delivered task (`prompt`, `task`)
/// would let a PR URL in the task verify itself (#10721 W1).
/// What: walks objects and arrays depth-first, keeps raw (unescaped) string
/// values, drops any that contain the whole trimmed task, and joins them with
/// newlines.
/// Test: `observe_tests.rs::interpret_evidence_fallback_to_json_when_no_pane`,
/// `task_field_in_record_is_not_evidence`.
fn record_strings(payload: &Value, task: &str) -> String {
    fn walk<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
        match v {
            Value::String(s) => out.push(s),
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            Value::Object(map) => map.values().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut values = Vec::new();
    walk(payload, &mut values);
    let task = task.trim();
    values
        .into_iter()
        .filter(|s| task.is_empty() || !s.contains(task))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a record `state` string indicates a terminal FAILURE.
///
/// Why: a session that errored/died is a `Failed` task — the goal cannot close on
/// it, and the operator must be told. Centralising the terminal-failure vocabulary
/// keeps `interpret_session` readable.
/// What: returns `true` for `errored`/`dead`/`failed`/`killed` (case already
/// lowered by the caller).
/// Test: `observe_tests.rs::interpret_failed`, `failed_state_with_evidence_is_failed`.
fn is_failed_state(state: &str) -> bool {
    matches!(state, "errored" | "dead" | "failed" | "killed")
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;

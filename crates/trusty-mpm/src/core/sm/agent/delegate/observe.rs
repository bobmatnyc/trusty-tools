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
//! What: [`ObservedState`] (the interpreted state + optional captured evidence),
//! [`interpret_session`] (session JSON → `ObservedState`), and the evidence
//! scanner [`scan_evidence`]. The orchestrator turns an [`ObservedState`] into a
//! goal-store [`SessionUpdate`](crate::core::sm::SessionUpdate).
//! Test: `observe_tests.rs` covers running/failed/verified interpretation and
//! evidence extraction (PR URL, test-pass output) vs. no-evidence.

use serde_json::Value;

use crate::core::sm::goals::SessionTaskState;

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
/// carries observed EVIDENCE (§3.5), never merely because it reports "done".
/// What: reads the nested `session` object (the control surface wraps the record
/// as `{ "session": { … } }`); FIRST scans the RAW pane/output string value for
/// evidence (so JSON escaping never garbles a captured PR URL or test line), and
/// only falls back to scanning the whole compact JSON when no pane/output field is
/// present. If evidence is present → `Verified` + that evidence. Else if the record
/// `state` indicates a terminal failure (`errored`/`dead`/`failed`) → `Failed`.
/// Else if it indicates the session is gone/stopped with no evidence, or still
/// active → `Running` (in flight, not yet verified). A brand-new record with no
/// activity stays `Launched`.
/// Test: `observe_tests.rs::interpret_running`, `interpret_failed`,
/// `interpret_verified_with_pr_url`, `interpret_no_evidence_stays_unverified`,
/// `interpret_evidence_from_raw_pane`.
pub fn interpret_session(session_json: &Value) -> ObservedState {
    // The control surface returns `{ "session": { … } }`; tolerate both shapes.
    let record = session_json.get("session").unwrap_or(session_json);

    // Evidence is scanned over the RAW pane/output string FIRST (issue #1311 review):
    // running the scanner on the escaped compact JSON (`session_json.to_string()`)
    // can garble evidence — e.g. a PR URL whose surrounding JSON adds `\"` / `}}`
    // framing, or a test line with escaped newlines. Pulling the raw string value
    // out of the record and scanning THAT keeps the captured evidence clean. We fall
    // back to the whole compact JSON only when no pane/output field is present, so
    // evidence anywhere in the payload is still found.
    let raw_pane = extract_pane_text(record);
    let haystack = raw_pane.unwrap_or_else(|| session_json.to_string());
    if let Some(evidence) = scan_evidence(&haystack) {
        return ObservedState {
            state: SessionTaskState::Verified,
            evidence: Some(evidence),
        };
    }

    let state_str = record
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();

    let interpreted = if is_failed_state(&state_str) {
        SessionTaskState::Failed
    } else if state_str.is_empty() || state_str == "created" || state_str == "provisioning" {
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
/// caller then falls back to the whole compact JSON so evidence elsewhere in the
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

/// Whether a record `state` string indicates a terminal FAILURE.
///
/// Why: a session that errored/died is a `Failed` task — the goal cannot close on
/// it, and the operator must be told. Centralising the terminal-failure vocabulary
/// keeps `interpret_session` readable.
/// What: returns `true` for `errored`/`dead`/`failed`/`killed` (case already
/// lowered by the caller).
/// Test: `observe_tests.rs::interpret_failed`.
fn is_failed_state(state: &str) -> bool {
    matches!(state, "errored" | "dead" | "failed" | "killed")
}

/// Scan observed pane/record text for gate-satisfying evidence (§3.5).
///
/// Why: the verification gate forbids `Verified` without OBSERVED evidence (a PR
/// URL, a captured test-pass count, a diff/write confirmation). A deterministic
/// scanner means a test can pin exactly what counts as evidence, and the SM can
/// never "claim done" without it.
/// What: returns the FIRST matching evidence snippet found, in priority order:
/// (1) a GitHub/GitLab PR/MR URL; (2) a test-pass summary (`N passed`, `N tests
/// passed`, `test result: ok`); (3) a diff/write confirmation marker. Returns
/// `None` when no evidence pattern matches.
/// Test: `observe_tests.rs::scan_finds_pr_url`, `scan_finds_test_pass`,
/// `scan_finds_diff`, `scan_finds_nothing`.
pub fn scan_evidence(text: &str) -> Option<String> {
    if let Some(url) = find_pr_url(text) {
        return Some(format!("PR opened: {url}"));
    }
    if let Some(pass) = find_test_pass(text) {
        return Some(format!("tests pass: {pass}"));
    }
    if let Some(diff) = find_diff_marker(text) {
        return Some(format!("edit made: {diff}"));
    }
    None
}

/// Find a GitHub/GitLab pull/merge-request URL in `text`.
///
/// Why: "PR opened" evidence is a printed PR URL (§3.5). A token scan keeps this
/// dependency-free (no regex crate) and predictable. Because evidence is scanned
/// over the JSON-serialized session payload, a URL at the END of a JSON string
/// value is followed by `"`/`}`/`]` framing punctuation; the trim must strip ALL
/// of that so the captured URL is clean (not e.g. `…/pull/9"}}`).
/// What: finds the first `http`-scheme span (scanning char-by-char so it works
/// even when the URL has NO surrounding whitespace — e.g. embedded in compact
/// JSON like `"pane":"…/pull/9","state":…`), bounding the URL at the first
/// non-URL character (whitespace OR JSON/sentence framing: quotes, braces,
/// brackets, parens, comma, trailing dot), then returns it iff it carries a
/// `github.com /pull/` or `gitlab /merge_requests/` PR path.
/// Test: `observe_tests.rs::scan_finds_pr_url`, `scan_pr_url_strips_json_framing`.
fn find_pr_url(text: &str) -> Option<String> {
    // A URL boundary: whitespace or framing punctuation that cannot be inside a
    // URL we care about. (A trailing `.` ends a sentence; mid-URL dots are fine
    // because the boundary only fires at the END once a delimiter is hit.)
    let is_boundary = |c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '}' | '{' | ']' | '[' | '(' | ')' | ',')
    };

    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find("http") {
        let start = search_from + rel;
        // The URL runs from `start` to the first boundary char (or end of text).
        let len = text[start..]
            .find(is_boundary)
            .unwrap_or(text.len() - start);
        let end = start + len;
        // Strip a single trailing sentence period (mid-URL dots are kept because
        // the boundary scan already stopped at the first framing char).
        let candidate = text[start..end].trim_end_matches('.');
        if (candidate.contains("github.com") && candidate.contains("/pull/"))
            || (candidate.contains("gitlab") && candidate.contains("/merge_requests/"))
        {
            return Some(candidate.to_string());
        }
        search_from = end.max(start + 1);
    }
    None
}

/// Find a test-pass summary in `text`.
///
/// Why: "tests pass" evidence is a captured run with a pass count (§3.5).
/// What: scans for the cargo `test result: ok.` marker (returning the line) or a
/// `N passed` / `N tests passed` phrase; returns the matched snippet.
/// Test: `observe_tests.rs::scan_finds_test_pass`.
fn find_test_pass(text: &str) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("test result: ok") {
            return Some(line.trim().to_string());
        }
        if (lower.contains(" passed") || lower.contains("passed;"))
            && lower.chars().any(|c| c.is_ascii_digit())
        {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// Find a diff / file-write confirmation marker in `text`.
///
/// Why: "edit made" evidence is a diff or write confirmation in the pane (§3.5).
/// What: returns the first line containing a unified-diff header (`diff --git`),
/// or a write-confirmation phrase (`wrote `/`updated `/`created file`); else
/// `None`.
/// Test: `observe_tests.rs::scan_finds_diff`.
fn find_diff_marker(text: &str) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("diff --git")
            || lower.contains("wrote ")
            || lower.contains("updated file")
            || lower.contains("created file")
        {
            return Some(line.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;

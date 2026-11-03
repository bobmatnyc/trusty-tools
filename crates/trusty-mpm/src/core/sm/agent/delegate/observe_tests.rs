//! Unit tests for the deterministic OBSERVE/VERIFY interpretation.
//!
//! Why: phases 4–5 interpret raw session JSON with NO LLM (§3.4 pane heuristic);
//! these tests pin the verification-state interpretation and — critically — that
//! `Verified` is ONLY ever reached when gate-satisfying evidence is observed
//! (§3.5). The evidence scanner is the trust boundary of the whole gate.
//! What: drives `interpret_session` + `scan_evidence` over representative pane
//! payloads.
//! Test: this is the test module.

use serde_json::json;

use super::{interpret_session, scan_evidence};
use crate::core::sm::goals::SessionTaskState;

/// Why: an active/running session with no evidence is `Running` — in flight, not
/// yet verified (the gate is not satisfied).
/// What: interprets a `running` record and asserts `Running` + no evidence.
/// Test: this is the test.
#[test]
fn interpret_running() {
    let obs = interpret_session(&json!({ "session": { "state": "running" } }));
    assert_eq!(obs.state, SessionTaskState::Running);
    assert!(obs.evidence.is_none());
}

/// Why: a terminal-failure record must interpret as `Failed` so the goal cannot
/// close on it and the operator is told.
/// What: interprets an `errored` record and asserts `Failed`.
/// Test: this is the test.
#[test]
fn interpret_failed() {
    let obs = interpret_session(&json!({ "session": { "state": "errored" } }));
    assert_eq!(obs.state, SessionTaskState::Failed);
    assert!(obs.evidence.is_none());
}

/// Why: a brand-new record with no activity stays `Launched` (nothing observed).
/// What: interprets a `created` record and asserts `Launched`.
/// Test: this is the test.
#[test]
fn interpret_launched() {
    let obs = interpret_session(&json!({ "session": { "state": "created" } }));
    assert_eq!(obs.state, SessionTaskState::Launched);
}

/// Why: the verification gate — a pane carrying a printed PR URL is gate-satisfying
/// EVIDENCE, so the task interprets as `Verified` WITH that evidence captured.
/// What: interprets a record whose pane output contains a PR URL.
/// Test: this is the test.
#[test]
fn interpret_verified_with_pr_url() {
    let obs = interpret_session(&json!({
        "session": {
            "state": "running",
            "pane": "Opened PR https://github.com/acme/repo/pull/42 ready for review"
        }
    }));
    assert_eq!(obs.state, SessionTaskState::Verified);
    assert_eq!(
        obs.evidence.as_deref(),
        Some("PR opened: https://github.com/acme/repo/pull/42")
    );
}

/// Why: without evidence, even a "done"-looking running session is NOT verified —
/// the gate blocks. Proves the SM cannot reach `Verified` on a bare state.
/// What: interprets a `stopped` record with no evidence; asserts `Running` (in
/// flight / not verified), evidence none.
/// Test: this is the test.
#[test]
fn interpret_no_evidence_stays_unverified() {
    let obs = interpret_session(&json!({ "session": { "state": "stopped" } }));
    assert_ne!(obs.state, SessionTaskState::Verified);
    assert!(obs.evidence.is_none());
}

/// Why: the PR-URL scanner is the most common evidence path; it must find a URL
/// even surrounded by prose/punctuation.
/// What: scans a sentence with a trailing-punctuation PR URL.
/// Test: this is the test.
#[test]
fn scan_finds_pr_url() {
    let ev = scan_evidence("All set. See https://github.com/o/r/pull/7).").unwrap();
    assert_eq!(ev, "PR opened: https://github.com/o/r/pull/7");
}

/// Why: regression for the trim bug — evidence is scanned over the JSON-serialized
/// payload, so a PR URL at the END of a string value is wrapped in `"}}` framing;
/// the captured URL must be CLEAN (no trailing JSON punctuation), or the recorded
/// evidence would hold a garbled, unopenable URL.
/// What: interprets a record whose pane field ENDS with the PR URL (so the
/// serialized form is `…pull/9"}}`) and asserts the captured evidence URL has no
/// trailing framing characters.
/// Test: this is the test.
#[test]
fn scan_pr_url_strips_json_framing() {
    let obs = interpret_session(&json!({
        "session": { "state": "running", "pane": "https://github.com/o/r/pull/9" }
    }));
    assert_eq!(
        obs.evidence.as_deref(),
        Some("PR opened: https://github.com/o/r/pull/9"),
        "captured URL must not carry trailing JSON framing"
    );
}

/// Why: the evidence scanner must run over the RAW pane string, not the escaped
/// compact JSON (issue #1311 review). A pane value containing characters JSON would
/// escape (quotes, newlines) must still yield CLEAN evidence — proving the scan
/// reads the unescaped value, not `session_json.to_string()`.
/// What: a pane whose text embeds a quoted phrase + newline THEN a PR URL on its
/// own line; asserts the captured URL is clean (no JSON framing, no escape gunk).
/// Test: this is the test.
#[test]
fn interpret_evidence_from_raw_pane() {
    let pane =
        "Ran the job: \"build\" finished.\nOpened PR https://github.com/acme/repo/pull/13\nDone.";
    let obs = interpret_session(&json!({
        "session": { "state": "running", "pane": pane }
    }));
    assert_eq!(obs.state, SessionTaskState::Verified);
    assert_eq!(
        obs.evidence.as_deref(),
        Some("PR opened: https://github.com/acme/repo/pull/13"),
        "evidence is scanned from the raw pane value, clean of JSON escaping"
    );
}

/// Why: when NO pane/output field is present, the scanner must FALL BACK to the
/// whole compact JSON so evidence in another record field is still found.
/// What: a record with a PR URL in a non-pane field and no pane field; asserts the
/// URL is still captured via the JSON fallback.
/// Test: this is the test.
#[test]
fn interpret_evidence_fallback_to_json_when_no_pane() {
    let obs = interpret_session(&json!({
        "session": { "state": "running", "result_url": "https://github.com/o/r/pull/5" }
    }));
    assert_eq!(obs.state, SessionTaskState::Verified);
    assert_eq!(
        obs.evidence.as_deref(),
        Some("PR opened: https://github.com/o/r/pull/5"),
        "no pane field ⇒ fall back to scanning the whole JSON"
    );
}

/// Why: captured cargo/test output with a pass count is gate-satisfying evidence.
/// What: scans a `test result: ok` line.
/// Test: this is the test.
#[test]
fn scan_finds_test_pass() {
    let ev = scan_evidence("running 12 tests\ntest result: ok. 12 passed; 0 failed").unwrap();
    assert!(ev.starts_with("tests pass:"));
    assert!(ev.contains("ok"));
}

/// Why: a diff / write confirmation is gate-satisfying "edit made" evidence.
/// What: scans a `diff --git` line.
/// Test: this is the test.
#[test]
fn scan_finds_diff() {
    let ev = scan_evidence("diff --git a/src/main.rs b/src/main.rs\n+ added").unwrap();
    assert!(ev.starts_with("edit made:"));
}

/// Why: a pane with no evidence pattern must yield NO evidence — the gate stays
/// closed. This is the single most important negative case.
/// What: scans plain prose with no PR/test/diff markers.
/// Test: this is the test.
#[test]
fn scan_finds_nothing() {
    assert!(scan_evidence("still working on the feature, no results yet").is_none());
}

/// Why: #10721 W1 — a PR URL the delegation never asked for is not proof that
/// this task opened or touched it (Codex repro input, verbatim).
/// What: a pane citing an unrelated PR after a failure; asserts not `Verified`.
/// Test: this is the test.
#[test]
fn pr_url_for_a_different_pr_is_not_verified() {
    let obs = interpret_session(&json!({
        "session": {
            "state": "running",
            "pane": "Task failed; reference PR https://github.com/example/project/pull/123"
        }
    }));
    assert_ne!(obs.state, SessionTaskState::Verified);
    assert!(obs.evidence.is_none(), "got {:?}", obs.evidence);
}

/// Why: #10721 W2 — a run summary with failures, or with zero passes, is never
/// test-pass evidence.
/// What: scans failing and empty run summaries; asserts no evidence for each.
/// Test: this is the test.
#[test]
fn zero_passed_three_failed_is_not_test_evidence() {
    for text in [
        "0 passed, 3 failed",
        "0 passed; 2 failed",
        "3 passed; 2 failed",
        "test result: FAILED. 0 passed; 3 failed; 0 ignored",
        "test result: ok. 0 passed; 0 failed; 0 ignored",
        "0 passed",
    ] {
        assert_eq!(scan_evidence(text), None, "input: {text}");
    }
}

/// Why: #10721 W3 — a negated or empty write claim is not "edit made" evidence.
/// What: scans prose that says nothing was written; asserts no evidence.
/// Test: this is the test.
#[test]
fn wrote_nothing_is_not_write_evidence() {
    for text in [
        "I never wrote the requested file.",
        "wrote nothing",
        "wrote 0 files",
        "I wrote nothing, the task is blocked",
    ] {
        assert_eq!(scan_evidence(text), None, "input: {text}");
    }
}

/// Why: #10721 W4 — a failed session record wins over any evidence line in its
/// pane, so evidence can never turn a failed session into `Verified`.
/// What: a `failed` record whose pane carries a valid passing summary; asserts
/// `Failed` with no evidence.
/// Test: this is the test.
#[test]
fn failed_state_with_evidence_is_failed() {
    let obs = interpret_session(&json!({
        "session": {
            "state": "failed",
            "pane": "test result: ok. 12 passed; 0 failed; 0 ignored"
        }
    }));
    assert_eq!(obs.state, SessionTaskState::Failed);
    assert!(obs.evidence.is_none(), "got {:?}", obs.evidence);
}

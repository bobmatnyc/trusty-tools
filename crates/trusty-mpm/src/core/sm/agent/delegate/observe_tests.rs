//! Unit tests for the deterministic OBSERVE/VERIFY interpretation.
//!
//! Why: phases 4–5 interpret raw session JSON with NO LLM (§3.4 pane heuristic);
//! these tests pin the verification-state interpretation and — critically — that
//! `Verified` is ONLY ever reached when gate-satisfying evidence is observed
//! (§3.5). The evidence scanner is the trust boundary of the whole gate.
//! What: drives `interpret_session` + `scan_evidence` over representative pane
//! payloads.
//! Test: this is the test module.

use serde_json::{Value, json};

use super::{
    ObservedState, TaskExpectation, interpret_session as interpret_for, scan_evidence as scan_for,
};
use crate::core::sm::goals::SessionTaskState;

/// Interpret with no known task: no PR URL can verify (#10721 W1).
fn interpret_session(v: &Value) -> ObservedState {
    interpret_for(v, &TaskExpectation::default())
}

/// Scan with no known task: no PR URL can verify (#10721 W1).
fn scan_evidence(text: &str) -> Option<String> {
    scan_for(text, &TaskExpectation::default())
}

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
/// What: interprets a record whose pane output contains the PR URL its task named
/// (#10721 W1).
/// Test: this is the test.
#[test]
fn interpret_verified_with_pr_url() {
    let expect =
        TaskExpectation::from_task("Address the review on https://github.com/acme/repo/pull/42");
    let obs = interpret_for(
        &json!({
            "session": {
                "state": "running",
                "pane": "Opened PR https://github.com/acme/repo/pull/42 ready for review"
            }
        }),
        &expect,
    );
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
/// What: scans a sentence with a trailing-punctuation PR URL the task named.
/// Test: this is the test.
#[test]
fn scan_finds_pr_url() {
    let expect = TaskExpectation::from_task("Fix CI on https://github.com/o/r/pull/7");
    let ev = scan_for("All set. See https://github.com/o/r/pull/7).", &expect).unwrap();
    assert_eq!(ev, "PR opened: https://github.com/o/r/pull/7");
}

/// Why: regression for the trim bug — evidence is scanned over the JSON-serialized
/// payload, so a PR URL at the END of a string value is wrapped in `"}}` framing;
/// the captured URL must be CLEAN (no trailing JSON punctuation), or the recorded
/// evidence would hold a garbled, unopenable URL.
/// What: interprets a record whose pane field ENDS with the PR URL (so the
/// serialized form is `…pull/9"}}`) and asserts the captured evidence URL has no
/// trailing framing characters. The URL is tied to the session by the
/// `gh pr create` line before it (#10721 W1).
/// Test: this is the test.
#[test]
fn scan_pr_url_strips_json_framing() {
    let pane = "Creating pull request for fix into main in o/r\n\nhttps://github.com/o/r/pull/9";
    let obs = interpret_session(&json!({
        "session": { "state": "running", "pane": pane }
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
    let expect = TaskExpectation::from_task("Rebase https://github.com/acme/repo/pull/13");
    let obs = interpret_for(
        &json!({ "session": { "state": "running", "pane": pane } }),
        &expect,
    );
    assert_eq!(obs.state, SessionTaskState::Verified);
    assert_eq!(
        obs.evidence.as_deref(),
        Some("PR opened: https://github.com/acme/repo/pull/13"),
        "evidence is scanned from the raw pane value, clean of JSON escaping"
    );
}

/// Why: when NO pane/output field is present, the scanner must FALL BACK to the
/// record's other string values so evidence in another record field is found.
/// What: a record with the task's PR URL in a non-pane field and no pane field;
/// asserts the URL is still captured via the fallback.
/// Test: this is the test.
#[test]
fn interpret_evidence_fallback_to_json_when_no_pane() {
    let expect = TaskExpectation::from_task("Land https://github.com/o/r/pull/5");
    let obs = interpret_for(
        &json!({
            "session": { "state": "running", "result_url": "https://github.com/o/r/pull/5" }
        }),
        &expect,
    );
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

/// Why: #10721 W1 — with a task that names a PR, that PR (any case, any sub-path)
/// is evidence and a different PR, repo or host is not.
/// What: scans matching and non-matching PR URLs against one expectation.
/// Test: this is the test.
#[test]
fn pr_url_matching_the_task_pr_is_verified() {
    let expect = TaskExpectation::from_task("Fix review on https://github.com/acme/repo/pull/42");
    assert_eq!(
        scan_for(
            "pushed: https://github.com/Acme/Repo/pull/42/files",
            &expect
        )
        .as_deref(),
        Some("PR opened: https://github.com/Acme/Repo/pull/42/files")
    );
    for other in [
        "https://github.com/acme/repo/pull/43",
        "https://github.com/acme/other/pull/42",
        "https://github.com/example/project/pull/123",
    ] {
        assert_eq!(scan_for(other, &expect), None, "input: {other}");
    }
}

/// Why: #10721 W1 — a PR the session opens has no number the task could name;
/// `gh pr create`'s status line ties the URL to this session and repo.
/// What: a created PR in the named repo verifies; a URL for another repo does not.
/// Test: this is the test.
#[test]
fn pr_created_in_session_is_verified() {
    let pane = "Creating pull request for feat into main in acme/login\n\nhttps://github.com/acme/login/pull/9";
    assert_eq!(
        scan_evidence(pane).as_deref(),
        Some("PR opened: https://github.com/acme/login/pull/9")
    );
    let other = "Creating pull request for feat into main in acme/login\nhttps://github.com/acme/web/pull/9";
    assert_eq!(scan_evidence(other), None);
}

/// Why: #10721 W1 provenance — the SM types the task into the pane, so the task's
/// own PR URL echoed back must not verify the task.
/// What: a pane holding only the echoed task (wrapped, with prompt decoration);
/// asserts not `Verified`.
/// Test: this is the test.
#[test]
fn pr_url_echoed_from_the_task_is_not_verified() {
    let task = "Address the review comments on https://github.com/acme/repo/pull/42 and push";
    let pane =
        "> Address the review comments on\n  https://github.com/acme/repo/pull/42 and push\n";
    let obs = interpret_for(
        &json!({ "session": { "state": "running", "pane": pane } }),
        &TaskExpectation::from_task(task),
    );
    assert_ne!(obs.state, SessionTaskState::Verified);
    assert!(obs.evidence.is_none(), "got {:?}", obs.evidence);
}

/// Why: #10721 W1 — with no pane field, a record field carrying the task text
/// must not let the task's PR URL verify itself.
/// What: a record whose only PR URL is inside its `prompt` field; asserts not
/// `Verified`.
/// Test: this is the test.
#[test]
fn task_field_in_record_is_not_evidence() {
    let task = "Fix review on https://github.com/acme/repo/pull/42";
    let obs = interpret_for(
        &json!({ "session": { "state": "running", "prompt": task } }),
        &TaskExpectation::from_task(task),
    );
    assert_ne!(obs.state, SessionTaskState::Verified);
}

/// Why: a failure marker beats evidence — a failing run in the pane vetoes an
/// otherwise valid passing summary or diff.
/// What: a pane with one failing and one passing summary plus a diff header;
/// asserts no evidence.
/// Test: this is the test.
#[test]
fn failing_summary_vetoes_other_evidence() {
    let pane = "test result: ok. 12 passed; 0 failed\n\
                test result: FAILED. 3 passed; 1 failed\n\
                diff --git a/src/a.rs b/src/a.rs";
    assert_eq!(scan_evidence(pane), None);
}

/// Why: #10721 W3 positive side — a write that names the written path is evidence.
/// What: scans Claude Code's `Wrote N lines to <path>` and a `created file <path>`.
/// Test: this is the test.
#[test]
fn write_naming_a_path_is_write_evidence() {
    for text in [
        "  ⎿  Wrote 42 lines to src/core/thing.rs",
        "created file: docs/notes.md",
        "wrote README.md",
    ] {
        let ev = scan_evidence(text).unwrap_or_else(|| panic!("no evidence for: {text}"));
        assert!(ev.starts_with("edit made:"), "{ev}");
    }
}

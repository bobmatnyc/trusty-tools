//! Unit tests for the SM decision protocol parser (`parse_decision`).
//!
//! Why: the decision parser is the trust boundary between the model's free text
//! and the engine's typed actions — it must extract a valid action from fenced,
//! bare, or prose-wrapped JSON, and it must NEVER turn garbage into a delegation
//! or a direct-work action (the safe fallback is "talk to the operator").
//! What: drives `parse_decision` with each shape and asserts the typed result.
//! Test: this is the test module.

use super::{SmDecision, TaskSpec, parse_decision};

/// Why: the normal model output is a ```json-fenced action block; the parser must
/// strip the fence and yield a `Delegate` with the tasks.
/// What: parses a fenced delegate block and asserts the two tasks survive.
/// Test: this is the test.
#[test]
fn parse_fenced_json() {
    let reply = "Here is my plan:\n```json\n{\"action\":\"delegate\",\"tasks\":[\
        {\"workdir\":\"/repo\",\"prompt\":\"add login\"},\
        {\"workdir\":\"/repo\",\"prompt\":\"write tests\",\"model\":\"sonnet\"}]}\n```\nDone.";
    match parse_decision(reply) {
        SmDecision::Delegate { tasks } => {
            assert_eq!(tasks.len(), 2);
            assert_eq!(tasks[0].workdir, "/repo");
            assert_eq!(tasks[0].prompt, "add login");
            assert_eq!(tasks[1].model.as_deref(), Some("sonnet"));
        }
        other => panic!("expected Delegate, got {other:?}"),
    }
}

/// Why: a bare JSON object (no fence) is also valid model output.
/// What: parses a bare delegate object and asserts the single task.
/// Test: this is the test.
#[test]
fn parse_bare_json() {
    let reply = "{\"action\":\"delegate\",\"tasks\":[{\"workdir\":\"/r\",\"prompt\":\"do it\"}]}";
    assert_eq!(
        parse_decision(reply),
        SmDecision::Delegate {
            tasks: vec![TaskSpec {
                workdir: "/r".to_string(),
                prompt: "do it".to_string(),
                model: None,
            }],
        }
    );
}

/// Why: the model often surrounds the JSON with prose; the first-`{`…last-`}`
/// span extraction must still recover the action.
/// What: parses a prose-wrapped respond action.
/// Test: this is the test.
#[test]
fn parse_prose_wrapped_json() {
    let reply = "I think we should ask first. {\"action\":\"respond\",\
        \"message\":\"which repo?\"} Let me know.";
    assert_eq!(
        parse_decision(reply),
        SmDecision::Respond {
            message: "which repo?".to_string()
        }
    );
}

/// Why: a direct-work attempt must parse to the `DoWork` variant so the engine
/// can refuse + redirect it (the SP guard).
/// What: parses a `do_work` action and asserts the summary.
/// Test: this is the test.
#[test]
fn parse_do_work_action() {
    let reply = "{\"action\":\"do_work\",\"summary\":\"I edited main.rs to add the flag\"}";
    assert_eq!(
        parse_decision(reply),
        SmDecision::DoWork {
            summary: "I edited main.rs to add the flag".to_string()
        }
    );
}

/// Why: unparseable / non-JSON model output must fall back to `Respond` (the SM
/// is talking to the operator) — NEVER to a delegation or to doing work.
/// What: parses plain prose and asserts it becomes a `Respond` echoing the text.
/// Test: this is the test.
#[test]
fn parse_garbage_is_respond() {
    let reply = "I am not sure what you mean — can you clarify the goal?";
    assert_eq!(
        parse_decision(reply),
        SmDecision::Respond {
            message: "I am not sure what you mean — can you clarify the goal?".to_string()
        }
    );
}

/// Why: a delegate block may contain blank/garbage task entries; `is_launchable`
/// lets the engine drop them rather than spawning idle sessions.
/// What: asserts a blank-workdir task is not launchable and a full one is.
/// Test: this is the test.
#[test]
fn task_launchable_predicate() {
    let blank = TaskSpec {
        workdir: "  ".to_string(),
        prompt: "x".to_string(),
        model: None,
    };
    let full = TaskSpec {
        workdir: "/r".to_string(),
        prompt: "x".to_string(),
        model: None,
    };
    assert!(!blank.is_launchable());
    assert!(full.is_launchable());
}

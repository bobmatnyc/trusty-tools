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

/// Why: regression for the fence-ordering bug — a reply containing BOTH a ```json
/// block AND a later bare ``` block must parse the JSON inside the ```json block,
/// not mis-pair the ```json opening with the later bare closing fence (which would
/// swallow prose between them and truncate the object).
/// What: a reply with a ```json delegate block followed by a separate ``` snippet;
/// asserts the delegate action is recovered intact.
/// Test: this is the test.
#[test]
fn parse_both_json_and_bare_fence() {
    let reply = "Plan:\n```json\n{\"action\":\"delegate\",\"tasks\":[\
        {\"workdir\":\"/repo\",\"prompt\":\"add login\"}]}\n```\n\nAside:\n```\nsome shell\n```";
    match parse_decision(reply) {
        SmDecision::Delegate { tasks } => {
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].workdir, "/repo");
            assert_eq!(tasks[0].prompt, "add login");
        }
        other => panic!("expected Delegate from the json fence, got {other:?}"),
    }
}

/// Why: regression for the `rfind('}')` bug — a prose brace AFTER the JSON object
/// (e.g. `{...} see {docs}`) must NOT extend the captured span to the trailing
/// prose brace (which truncated the JSON → a spurious `Respond` fallback that
/// dropped an intended `Delegate`). The balanced scan stops at the FIRST complete
/// object.
/// What: a delegate object followed by a prose `{docs}`; asserts `Delegate`.
/// Test: this is the test.
#[test]
fn parse_prose_brace_after_json() {
    let reply =
        "{\"action\":\"delegate\",\"tasks\":[{\"workdir\":\"/r\",\"prompt\":\"go\"}]} see {docs}";
    match parse_decision(reply) {
        SmDecision::Delegate { tasks } => {
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].prompt, "go");
        }
        other => panic!("expected Delegate despite trailing prose brace, got {other:?}"),
    }
}

/// Why: a prose brace BEFORE the JSON object must not derail the balanced scan —
/// it begins its own (unbalanced) object that never closes, so the scan must move
/// on and still recover the real action object.
/// What: a leading prose `{note}` then a respond object; asserts `Respond`.
/// Test: this is the test.
#[test]
fn parse_prose_brace_before_json() {
    let reply = "Context {see notes}: {\"action\":\"respond\",\"message\":\"hi there\"}";
    assert_eq!(
        parse_decision(reply),
        SmDecision::Respond {
            message: "hi there".to_string()
        }
    );
}

/// Why: braces INSIDE a JSON string value (e.g. a prompt mentioning `{cfg}`) must
/// NOT change brace depth — a naive depth counter that ignored string literals
/// would close the object early and truncate it.
/// What: a delegate task whose prompt contains literal braces; asserts the full
/// prompt (with the inner braces) survives.
/// Test: this is the test.
#[test]
fn parse_brace_inside_string_value() {
    let reply = "{\"action\":\"delegate\",\"tasks\":[\
        {\"workdir\":\"/r\",\"prompt\":\"render {a:{b:1}} and ship\"}]}";
    match parse_decision(reply) {
        SmDecision::Delegate { tasks } => {
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].prompt, "render {a:{b:1}} and ship");
        }
        other => panic!("expected Delegate with braces inside string, got {other:?}"),
    }
}

/// Why: an escaped quote inside a JSON string must not prematurely end string mode
/// (which would expose an inner brace to the depth counter). Proves the escape
/// handling in the balanced scan.
/// What: a respond message containing an escaped quote then a brace; asserts the
/// message round-trips with the literal quote and brace.
/// Test: this is the test.
#[test]
fn parse_escaped_quote_in_string() {
    let reply = r#"{"action":"respond","message":"say \"hi\" and {wave}"}"#;
    assert_eq!(
        parse_decision(reply),
        SmDecision::Respond {
            message: "say \"hi\" and {wave}".to_string()
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

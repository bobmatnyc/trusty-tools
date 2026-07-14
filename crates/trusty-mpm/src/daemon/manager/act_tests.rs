//! Unit tests for the pure `/manager/act` proposal renderer (WI-9, #2586).

use super::*;

#[test]
fn propose_message_launch() {
    let action = ProposedAction::Launch {
        project: "alpha".to_string(),
        task: "fix the flaky auth test".to_string(),
    };
    let msg = propose_message(&action);
    assert!(
        msg.starts_with("Proposed: launch a new session for project 'alpha'"),
        "{msg}"
    );
    assert!(msg.contains("fix the flaky auth test"), "{msg}");
    assert!(msg.contains("\"confirm\": true"), "{msg}");
}

#[test]
fn propose_message_inject() {
    let action = ProposedAction::Inject {
        session: "sess-1".to_string(),
        text: "run the tests".to_string(),
    };
    let msg = propose_message(&action);
    assert!(msg.contains("inject into session 'sess-1'"), "{msg}");
    assert!(msg.contains("run the tests"), "{msg}");
}

#[test]
fn propose_message_summarize() {
    let action = ProposedAction::Summarize {
        session: "sess-2".to_string(),
    };
    let msg = propose_message(&action);
    assert!(
        msg.contains("summarize the recent activity of session 'sess-2'"),
        "{msg}"
    );
}

/// The action round-trips through serde so a proposal response can be echoed back
/// verbatim on the confirming call.
#[test]
fn proposed_action_serde_round_trip() {
    let action = ProposedAction::Launch {
        project: "alpha".to_string(),
        task: "t".to_string(),
    };
    let json = serde_json::to_value(&action).unwrap();
    assert_eq!(json["type"], "launch");
    let back: ProposedAction = serde_json::from_value(json).unwrap();
    assert_eq!(back, action);
}

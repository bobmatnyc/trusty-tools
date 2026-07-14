//! Unit tests for the chat propose→confirm plumbing (WI-9, #2586).

use super::*;

#[test]
fn extract_proposed_action_parses_launch_block() {
    let reply = "I'll route this to alpha.\n\n```manager-action\n\
                 {\"type\":\"launch\",\"project\":\"alpha\",\"task\":\"fix the flaky auth test\"}\n\
                 ```";
    let (text, action) = extract_proposed_action(reply);
    let action = action.expect("proposal parsed");
    assert_eq!(
        action,
        ProposedAction::Launch {
            project: "alpha".to_string(),
            task: "fix the flaky auth test".to_string(),
        }
    );
    assert_eq!(text, "I'll route this to alpha.");
}

#[test]
fn extract_proposed_action_parses_inject_block() {
    let reply = "```manager-action\n{\"type\":\"inject\",\"session\":\"sess-1\",\"text\":\"run tests\"}\n```";
    let (_, action) = extract_proposed_action(reply);
    assert_eq!(
        action.unwrap(),
        ProposedAction::Inject {
            session: "sess-1".to_string(),
            text: "run tests".to_string(),
        }
    );
}

#[test]
fn extract_proposed_action_no_block_returns_none() {
    let reply = "Everything looks fine, nothing blocked.";
    let (text, action) = extract_proposed_action(reply);
    assert!(action.is_none());
    assert_eq!(text, reply);
}

#[test]
fn extract_proposed_action_malformed_json_returns_none() {
    let reply = "```manager-action\nnot valid json at all\n```";
    let (text, action) = extract_proposed_action(reply);
    assert!(action.is_none());
    // Malformed proposals are treated as "no proposal" — the raw text passes
    // through unchanged rather than being silently mangled.
    assert_eq!(text, reply);
}

#[test]
fn extract_proposed_action_keeps_leading_and_trailing_prose() {
    let reply = "before text\n```manager-action\n{\"type\":\"summarize\",\"session\":\"s1\"}\n```\nafter text";
    let (text, action) = extract_proposed_action(reply);
    assert!(action.is_some());
    assert_eq!(text, "before text\nafter text");
}

#[test]
fn is_confirmation_accepts_documented_phrases() {
    for phrase in ["confirm", "Confirm", "CONFIRM", "confirm.", "confirm!", "  confirm  ", "yes", "Yes.", "y", "confirmed"] {
        assert!(is_confirmation(phrase), "expected confirmation: {phrase:?}");
    }
}

#[test]
fn is_confirmation_rejects_partial_or_unrelated_text() {
    for phrase in [
        "please confirm this",
        "confirm the launch of alpha",
        "no",
        "",
        "  ",
        "yesish",
        "y not",
    ] {
        assert!(!is_confirmation(phrase), "expected NOT a confirmation: {phrase:?}");
    }
}

#[test]
fn proposal_store_take_is_consume_on_read() {
    let store = ProposalStore::new();
    let action = ProposedAction::Summarize {
        session: "s1".to_string(),
    };
    store.set("conv-a", action.clone());

    // First take returns and consumes it.
    assert_eq!(store.take("conv-a"), Some(action));
    // Second take (the "later turn") finds nothing — it already expired.
    assert_eq!(store.take("conv-a"), None);
}

#[test]
fn proposal_store_distinct_keys_are_isolated() {
    let store = ProposalStore::new();
    store.set(
        "conv-a",
        ProposedAction::Summarize {
            session: "s1".to_string(),
        },
    );
    // A different conversation key has nothing pending.
    assert_eq!(store.take("conv-b"), None);
    // conv-a's proposal is still there (unaffected by conv-b's empty take).
    assert!(store.take("conv-a").is_some());
}

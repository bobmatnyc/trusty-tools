//! Unit tests for the chat prompt builder (WI-4, #2581).
//!
//! Why: the prompt must ground each reply in the live snapshot AND replay prior
//! turns in order, and it must carry NO tools (the read-only guarantee is
//! structural). Prove the message assembly directly.
//! What: covers [`super::build_chat_messages`] with a history and an empty
//! snapshot.
//! Test: this file IS the test module.

use super::super::chat_store::{ChatTurn, TurnRole};
use super::super::status::aggregate_portfolio_status;
use super::build_chat_messages;

/// Why: the assembled prompt must be system(persona+snapshot) → replayed history
/// → new user message, in that order, with the read-only persona pinned.
/// Test: itself.
#[test]
fn build_chat_messages_includes_context_and_history() {
    let status = aggregate_portfolio_status(&[], &[], &[], &[]);
    let history = vec![
        ChatTurn {
            role: TurnRole::User,
            content: "what's blocked?".to_string(),
        },
        ChatTurn {
            role: TurnRole::Assistant,
            content: "nothing right now".to_string(),
        },
    ];
    let messages = build_chat_messages(&status, &history, "and now?");
    assert_eq!(messages.len(), 4, "system + 2 history + new user");
    assert_eq!(messages[0].role, "system");
    let system = messages[0].content.as_deref().unwrap_or_default();
    assert!(system.contains("read-only"), "persona pinned read-only");
    assert!(system.contains("\"project_count\": 0"), "snapshot embedded");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content.as_deref(), Some("what's blocked?"));
    assert_eq!(messages[2].role, "assistant");
    assert_eq!(messages[3].role, "user");
    assert_eq!(messages[3].content.as_deref(), Some("and now?"));
}

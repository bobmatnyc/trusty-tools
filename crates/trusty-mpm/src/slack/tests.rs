//! Unit tests for the Slack adapter's pure logic.
//!
//! Why: the adapter's correctness-critical pieces — Socket-Mode envelope parsing,
//! the bot-message guard (no reply loops), the rolling-history cap, the action
//! footer, and dotenv token resolution — are all pure and must be tested without
//! a live Slack socket or the daemon (the live WebSocket loop is deferred to a
//! real Slack app; see the PR body).
//! What: covers `parse_envelope`, `ack_frame`, `record_chat_turn`,
//! `action_footer`, and `resolve_token`.
//! Test: this IS the test module.

use super::*;

#[test]
fn parse_envelope_slash_command() {
    let raw = r#"{
        "type": "slash_commands",
        "envelope_id": "env-1",
        "payload": {
            "command": "/fleet",
            "text": "",
            "channel_id": "C123"
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::SlashCommand {
            envelope_id: "env-1".into(),
            command: "/fleet".into(),
            text: String::new(),
            channel: "C123".into(),
        }
    );
}

#[test]
fn parse_envelope_slash_command_with_text() {
    let raw = r#"{
        "type": "slash_commands",
        "envelope_id": "env-2",
        "payload": { "command": "/msend", "text": "sess-1 build now", "channel_id": "C9" }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::SlashCommand {
            envelope_id: "env-2".into(),
            command: "/msend".into(),
            text: "sess-1 build now".into(),
            channel: "C9".into(),
        }
    );
}

#[test]
fn parse_envelope_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-3",
        "payload": {
            "event": { "type": "message", "text": "spin up a session", "channel": "C7", "user": "U1" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Message {
            envelope_id: "env-3".into(),
            text: "spin up a session".into(),
            channel: "C7".into(),
        }
    );
}

#[test]
fn parse_envelope_ignores_bot_message() {
    // A message carrying a bot_id is the bot's own post — ignore it (no reply
    // loops) but still surface the envelope id so the loop ACKs it.
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-4",
        "payload": {
            "event": { "type": "message", "text": "hi", "channel": "C7", "bot_id": "B1" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-4".into())
        }
    );
}

#[test]
fn parse_envelope_ignores_subtype_bot_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-5",
        "payload": {
            "event": { "type": "message", "subtype": "bot_message", "text": "hi", "channel": "C7" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-5".into())
        }
    );
}

#[test]
fn parse_envelope_ignores_empty_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-6",
        "payload": { "event": { "type": "message", "text": "   ", "channel": "C7" } }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-6".into())
        }
    );
}

#[test]
fn parse_envelope_hello_is_ignored() {
    // The `hello` envelope (sent on connect) needs no ACK and no reply.
    let raw = r#"{ "type": "hello" }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored { envelope_id: None }
    );
}

#[test]
fn parse_envelope_disconnect_carries_no_id() {
    let raw = r#"{ "type": "disconnect", "reason": "refresh_requested" }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored { envelope_id: None }
    );
}

#[test]
fn parse_envelope_garbage_is_ignored() {
    assert_eq!(
        parse_envelope("not json"),
        SlackEvent::Ignored { envelope_id: None }
    );
}

#[test]
fn ack_frame_shape() {
    assert_eq!(ack_frame("env-1"), r#"{"envelope_id":"env-1"}"#);
}

#[test]
fn envelope_id_of_extracts_each_variant() {
    assert_eq!(
        envelope_id_of(&SlackEvent::SlashCommand {
            envelope_id: "a".into(),
            command: "/x".into(),
            text: String::new(),
            channel: "C".into(),
        }),
        Some("a".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Message {
            envelope_id: "b".into(),
            text: "hi".into(),
            channel: "C".into(),
        }),
        Some("b".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Ignored {
            envelope_id: Some("c".into())
        }),
        Some("c".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Ignored { envelope_id: None }),
        None
    );
}

#[test]
fn record_chat_turn_caps_history() {
    let mut entry: Vec<ChatMessage> = Vec::new();
    // Push more than the cap; each call adds a user + assistant turn (2 msgs).
    for i in 0..(MAX_CHAT_HISTORY_TURNS) {
        record_chat_turn(&mut entry, &format!("u{i}"), &format!("a{i}"));
    }
    // Length is clamped to the cap, oldest dropped.
    assert!(entry.len() <= MAX_CHAT_HISTORY_TURNS);
    // The most recent user message survived.
    let last_user = entry
        .iter()
        .rev()
        .find_map(|m| (m.role == "user").then(|| m.content.clone()));
    assert_eq!(
        last_user.as_deref(),
        Some(&format!("u{}", MAX_CHAT_HISTORY_TURNS - 1)[..])
    );
}

#[test]
fn record_chat_turn_skips_empty_reply() {
    let mut entry: Vec<ChatMessage> = Vec::new();
    record_chat_turn(&mut entry, "hello", "");
    // Only the user turn is recorded; an empty assistant reply is skipped.
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].role, "user");
}

#[test]
fn action_footer_lists_verbs() {
    let footer = action_footer(Some(&["sessions.list".into(), "sessions.health".into()]));
    assert_eq!(
        footer.as_deref(),
        Some("\n\n_ran: sessions.list, sessions.health_")
    );
}

#[test]
fn action_footer_absent_when_empty() {
    assert_eq!(action_footer(None), None);
    let empty: Vec<String> = Vec::new();
    assert_eq!(action_footer(Some(&empty)), None);
}

#[test]
fn resolve_token_reads_dotenv() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".env.local");
    let mut f = std::fs::File::create(&path).expect("create dotenv");
    writeln!(f, "# a comment").unwrap();
    writeln!(f, "SLACK_BOT_TOKEN=\"xoxb-secret\"").unwrap();
    drop(f);

    // read_dotenv_key is the testable core (resolve_token reads from the cwd).
    assert_eq!(
        read_dotenv_key(&path, "SLACK_BOT_TOKEN").as_deref(),
        Some("xoxb-secret")
    );
    assert_eq!(read_dotenv_key(&path, "MISSING"), None);
}

#[test]
fn resolve_token_missing_is_none() {
    let missing = std::path::Path::new("/nonexistent/.env.local");
    assert_eq!(read_dotenv_key(missing, "SLACK_BOT_TOKEN"), None);
}

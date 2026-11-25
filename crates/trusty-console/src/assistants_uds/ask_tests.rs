//! Reply parsing and the default bounds of the ask client (#11748).
//!
//! The socket paths — answer, error, no daemon, stale socket, slow daemon,
//! oversize reply, early EOF — run through the real router in
//! `tests/assistants_ask.rs`.

use std::time::Duration;

use serde_json::json;

use super::ReplyErrorKind;
use super::ask::{
    ASK_TIMEOUT, AskFailure, AskLimits, CONNECT_TIMEOUT, MAX_REPLY_BYTES, parse_reply,
};

fn line(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("json");
    bytes.push(b'\n');
    bytes
}

#[test]
fn an_answer_parses() {
    let reply = line(&json!({"protocol": 1, "daemon_pid": 9, "outcome": "answer", "answer": "hi"}));
    assert_eq!(parse_reply(&reply).expect("answer"), "hi");
}

#[test]
fn a_daemon_error_is_a_failure() {
    let reply = line(&json!({
        "protocol": 1, "daemon_pid": 9, "outcome": "error", "kind": "bad_request", "message": "m"
    }));
    assert!(matches!(
        parse_reply(&reply),
        Err(AskFailure::Daemon {
            kind: ReplyErrorKind::BadRequest,
            ..
        })
    ));
}

#[test]
fn a_protocol_mismatch_is_a_failure() {
    for value in [
        json!({"protocol": 2, "daemon_pid": 9, "outcome": "answer", "answer": "hi"}),
        json!({"daemon_pid": 9, "outcome": "answer", "answer": "hi"}),
    ] {
        assert!(
            matches!(
                parse_reply(&line(&value)),
                Err(AskFailure::ProtocolMismatch { .. })
            ),
            "{value}"
        );
    }
}

#[test]
fn a_malformed_reply_is_a_failure() {
    for bytes in [
        b"nope\n".to_vec(),
        line(&json!({"protocol": 1, "outcome": "answer"})),
    ] {
        assert!(matches!(parse_reply(&bytes), Err(AskFailure::BadReply(_))));
    }
}

/// The documented production bounds.
#[test]
fn the_default_limits_are_the_documented_consts() {
    let limits = AskLimits::default();
    assert_eq!(limits.connect_timeout, CONNECT_TIMEOUT);
    assert_eq!(limits.ask_timeout, ASK_TIMEOUT);
    assert_eq!(limits.max_reply_bytes, MAX_REPLY_BYTES);
    assert!(CONNECT_TIMEOUT < ASK_TIMEOUT);
    assert!(
        ASK_TIMEOUT >= Duration::from_secs(60),
        "a model answer takes tens of seconds"
    );
}

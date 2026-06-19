//! Unit tests for the action-chat decision protocol (#1283).
//!
//! Why: the inline-action loop is only as safe as its parser and dispatch — a
//! verb-call must parse to a verb, a final answer must terminate, prose must
//! fall back to a final answer (never a spurious verb), the advertised verb list
//! must come from the catalog SoT, and each verb must map onto the right
//! `SessionControl` method. These tests pin all of that deterministically with a
//! recording mock control.
//! What: exercises [`parse_action`], [`action_instructions`], and [`execute_verb`]
//! against the shared `MockSessionControl`.
//! Test: this is the test module.

use std::sync::Arc;

use serde_json::json;

use super::{Decided, action_instructions, execute_verb, parse_action};
use crate::core::sm::agent::delegate::mock_control::MockSessionControl;
use crate::core::sm::control::SessionControl;

/// Why: a bare verb-call JSON must parse to a `Verb` with the name + args.
/// What: parses `{"action":"sessions.list","args":{}}`.
/// Test: this is the test.
#[test]
fn parse_verb_call() {
    let d = parse_action(r#"{"action":"sessions.list","args":{}}"#);
    match d {
        Decided::Verb { verb, .. } => assert_eq!(verb, "sessions.list"),
        other => panic!("expected verb, got {other:?}"),
    }
}

/// Why: the `final` action must terminate the loop, carrying the message.
/// What: parses `{"action":"final","message":"all good"}`.
/// Test: this is the test.
#[test]
fn parse_final_answer() {
    let d = parse_action(r#"{"action":"final","message":"all good"}"#);
    assert_eq!(
        d,
        Decided::Final {
            message: "all good".to_string()
        }
    );
}

/// Why: plain prose (no JSON) must be treated as the final answer — never a
/// spurious verb — so an unparseable reply is the always-safe terminal.
/// What: parses a prose string and asserts it becomes the final message verbatim.
/// Test: this is the test.
#[test]
fn parse_prose_is_final() {
    let d = parse_action("I checked and everything looks fine.");
    assert_eq!(
        d,
        Decided::Final {
            message: "I checked and everything looks fine.".to_string()
        }
    );
}

/// Why: FIX 4 — `next_balanced_object`'s `depth` decrement must not underflow on a
/// stray unmatched `}` (a `saturating_sub` guard). The parser must never panic and
/// must still extract a valid object when one follows the stray brace.
/// What: parses inputs with leading/trailing stray `}`; asserts no panic and that a
/// real object is still recovered (else the prose fallback).
/// Test: this is the test.
#[test]
fn parse_handles_stray_closing_brace_without_panic() {
    // A bare stray brace + prose: no object → prose fallback, no panic.
    let d = parse_action("} stray");
    assert_eq!(
        d,
        Decided::Final {
            message: "} stray".to_string()
        }
    );

    // A valid object followed by a stray `}`: the object must still be extracted.
    let d = parse_action(r#"{"action":"final","message":"ok"} junk }"#);
    assert_eq!(
        d,
        Decided::Final {
            message: "ok".to_string()
        }
    );
}

/// Why: the model often wraps its JSON in a ```json fence; the parser must still
/// extract the verb-call.
/// What: parses a fenced verb-call and asserts the verb name.
/// Test: this is the test.
#[test]
fn parse_fenced_verb() {
    let reply = "Let me look.\n```json\n{\"action\":\"sessions.get\",\"args\":{\"session_id\":\"s1\"}}\n```";
    match parse_action(reply) {
        Decided::Verb { verb, args } => {
            assert_eq!(verb, "sessions.get");
            assert_eq!(args.get("session_id").unwrap(), "s1");
        }
        other => panic!("expected verb, got {other:?}"),
    }
}

/// Why: the advertised verb list must be rendered from the catalog single source
/// of truth, so the prompt can never drift from the executable surface.
/// What: asserts the instruction block lists every catalog verb + both shapes.
/// Test: this is the test.
#[test]
fn prompt_lists_every_catalog_verb() {
    let prompt = action_instructions();
    for verb in [
        "sessions.launch",
        "sessions.list",
        "sessions.get",
        "sessions.send",
        "sessions.stop",
        "sessions.resume",
        "sessions.kill",
    ] {
        assert!(prompt.contains(verb), "prompt missing `{verb}`");
    }
    assert!(
        prompt.contains("\"action\":\"final\""),
        "missing final shape"
    );
    assert!(prompt.contains("INLINE"), "must state verbs run inline");
}

/// Why: the PRIMARY fix — the action loop must ADVERTISE `sessions.health` so the
/// self-aware coordinator chat knows it can run a health check inline (#1496).
/// What: asserts the instruction block lists the ops `sessions.health` verb.
/// Test: this is the test.
#[test]
fn prompt_lists_health_ops_verb() {
    let prompt = action_instructions();
    assert!(
        prompt.contains("sessions.health"),
        "action prompt must advertise the executable `sessions.health` verb"
    );
}

/// Why: the PRIMARY fix — `sessions.health` must EXECUTE inline in the action loop
/// (not just be advertised), synthesizing a fleet summary from the control surface.
/// What: dispatches `sessions.health` against the mock and asserts the synthesized
/// reachable=true / status=ok / zero-count shape.
/// Test: this is the test.
#[tokio::test]
async fn execute_health_reports_fleet() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let out = execute_verb(&control, "sessions.health", &json!({}))
        .await
        .expect("health dispatches");
    assert_eq!(out.get("reachable").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("ok"));
    assert_eq!(out.get("managed_total").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(
        out.get("managed_pending_decisions")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
}

/// Why: the unknown-verb error must now name `sessions.health` among the valid set
/// so a model that mistypes it can recover by picking the real ops verb.
/// What: triggers the unknown-verb error and asserts `sessions.health` is listed.
/// Test: this is the test.
#[tokio::test]
async fn unknown_verb_error_lists_health() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.bogus", &json!({}))
        .await
        .expect_err("unknown verb errors");
    assert!(err.to_string().contains("sessions.health"));
}

/// Why: `sessions.list` must dispatch to `SessionControl::list`.
/// What: executes the verb and asserts the mock's list body comes back.
/// Test: this is the test.
#[tokio::test]
async fn execute_dispatches_list() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let out = execute_verb(&control, "sessions.list", &json!({}))
        .await
        .expect("list dispatches");
    assert!(out.get("sessions").is_some());
}

/// Why: `sessions.send` must read `session_id` + `text` and reach `send`.
/// What: executes the verb and asserts the mock recorded the send.
/// Test: this is the test.
#[tokio::test]
async fn execute_send_reads_args() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    execute_verb(
        &control,
        "sessions.send",
        &json!({"session_id":"abc","text":"hello"}),
    )
    .await
    .expect("send dispatches");
    let sends = mock.sends();
    assert_eq!(sends, vec![("abc".to_string(), "hello".to_string())]);
}

/// Why: `sessions.get` without an id must error (fed back to the model), not panic.
/// What: executes `sessions.get` with empty args and asserts an error.
/// Test: this is the test.
#[tokio::test]
async fn execute_get_requires_id() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.get", &json!({}))
        .await
        .expect_err("missing id errors");
    assert!(err.to_string().contains("session_id"));
}

/// Why: an unknown verb must be a recoverable error fed back to the model, never
/// a panic or a silent no-op.
/// What: executes a bogus verb and asserts the error names the valid set.
/// Test: this is the test.
#[tokio::test]
async fn execute_unknown_verb_errors() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.frobnicate", &json!({}))
        .await
        .expect_err("unknown verb errors");
    assert!(err.to_string().contains("unknown verb"));
    assert!(err.to_string().contains("sessions.list"));
}

//! Unit tests for the §7.1 rolling-context data model.
//!
//! Why: the data types are the on-disk schema for the §7.4 state file and the
//! input to the §7.2 token-estimate trigger; their field accounting and serde
//! round-trip must be pinned so a schema change can't silently break persistence
//! or the trigger math.
//! What: exercises [`ToolTrace`], [`Round`], and [`SmConversation`] constructors,
//! char accounting, and JSON round-trips with a fixed timestamp.
//! Test: this is the test module.

use super::*;
use chrono::TimeZone;

/// A fixed, deterministic timestamp for every model test.
///
/// Why: tests must never read the wall clock; a constant instant keeps serde
/// round-trips and field assertions reproducible.
/// What: returns `2026-01-02T03:04:05Z`.
/// Test: used by the tests below.
fn fixed_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
        .single()
        .expect("valid ts")
}

/// Why: `ToolTrace::new` + serde must preserve both fields exactly.
/// What: builds a trace, round-trips it through JSON, asserts equality.
/// Test: this is the test.
#[test]
fn tool_trace_serde_roundtrip() {
    let t = ToolTrace::new("session_new", "spawned s-123 for goal g-7");
    let json = serde_json::to_string(&t).expect("serialise trace");
    let back: ToolTrace = serde_json::from_str(&json).expect("deserialise trace");
    assert_eq!(back, t);
    assert_eq!(
        back.char_len(),
        "session_new".len() + "spawned s-123 for goal g-7".len()
    );
}

/// Why: the constructor must place each argument in the right field.
/// What: asserts every field of a freshly-built round.
/// Test: this is the test.
#[test]
fn round_new_sets_fields() {
    let ts = fixed_ts();
    let r = Round::new("hi", "hello", ts, vec![ToolTrace::new("t", "s")]);
    assert_eq!(r.user, "hi");
    assert_eq!(r.assistant, "hello");
    assert_eq!(r.ts, ts);
    assert_eq!(r.tool_calls.len(), 1);
}

/// Why: the token estimate depends on a faithful per-round char count that
/// includes tool traces, not just the prose.
/// What: builds a round with two traces and asserts `char_len` equals the sum of
/// user + assistant + both traces.
/// Test: this is the test.
#[test]
fn round_char_len_sums_all_parts() {
    let r = Round::new(
        "user-text",
        "assistant-text",
        fixed_ts(),
        vec![ToolTrace::new("ab", "cd"), ToolTrace::new("ef", "gh")],
    );
    let expected = "user-text".len() + "assistant-text".len() + 2 + 2 + 2 + 2;
    assert_eq!(r.char_len(), expected);
}

/// Why: rounds persist to the §7.4 state file; serde must be lossless.
/// What: round-trips a populated round through JSON and asserts equality.
/// Test: this is the test.
#[test]
fn round_serde_roundtrip() {
    let r = Round::new("u", "a", fixed_ts(), vec![ToolTrace::new("n", "s")]);
    let json = serde_json::to_string(&r).expect("serialise round");
    let back: Round = serde_json::from_str(&json).expect("deserialise round");
    assert_eq!(back, r);
}

/// Why: a new conversation must start empty/zero so the first round and trigger
/// math start from a known baseline.
/// What: asserts every field of `SmConversation::new`.
/// Test: this is the test.
#[test]
fn conversation_default_is_empty() {
    let c = SmConversation::new();
    assert!(c.compressed_context.is_empty());
    assert_eq!(c.window_len(), 0);
    assert_eq!(c.total_rounds, 0);
    assert_eq!(c.token_estimate, 0);
}

/// Why: the whole conversation persists atomically (§7.4); a serde round-trip
/// must reconstruct an identical value (the persistence acceptance test relies
/// on this at the engine level).
/// What: populates a conversation, round-trips it through JSON, asserts equality.
/// Test: this is the test.
#[test]
fn conversation_serde_roundtrip() {
    let mut c = SmConversation::new();
    c.compressed_context = "earlier summary".to_string();
    c.recent_rounds
        .push_back(Round::new("u", "a", fixed_ts(), Vec::new()));
    c.total_rounds = 42;
    c.token_estimate = 1234;
    let json = serde_json::to_string(&c).expect("serialise conversation");
    let back: SmConversation = serde_json::from_str(&json).expect("deserialise conversation");
    assert_eq!(back, c);
}

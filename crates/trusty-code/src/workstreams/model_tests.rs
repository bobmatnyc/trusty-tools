//! Tests for `workstreams::model` (DOC-48 §2, AC-1.1, AC-4.1, AC-4.2).

use super::*;

#[test]
fn workstream_id_round_trip() {
    let id = WorkstreamId::new();
    let json = serde_json::to_string(&id).expect("serialize");
    let back: WorkstreamId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(id, back);
    assert_eq!(id.as_uuid(), back.as_uuid());
}

#[test]
fn new_workstream_starts_idle_with_no_sessions() {
    let ws = Workstream::new("Token rotation hardening");
    assert_eq!(ws.name, "Token rotation hardening");
    assert!(ws.session_ids.is_empty());
    assert!(ws.metadata.is_empty());
    assert_eq!(ws.created_at, ws.updated_at);
    // AC-4.1/AC-4.2: with no active pointer set anywhere, a fresh workstream
    // is idle, never active or closed.
    assert_eq!(ws.state(None), WorkstreamState::Idle);
}

#[test]
fn workstream_serde_round_trip() {
    let ws = Workstream::new("Auth refactoring");
    let json = serde_json::to_string(&ws).expect("serialize");
    let back: Workstream = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.id, ws.id);
    assert_eq!(back.name, ws.name);
    assert_eq!(back.session_ids, ws.session_ids);
    assert_eq!(back.created_at, ws.created_at);
    assert_eq!(back.updated_at, ws.updated_at);
}

#[test]
fn workstream_without_metadata_key_deserializes_empty() {
    // Why: a record persisted before `metadata` existed (or a hand-authored
    // fixture) must still deserialize with an empty map rather than failing.
    let legacy = serde_json::json!({
        "id": WorkstreamId::new(),
        "name": "legacy",
        "session_ids": [],
        "created_at": Utc::now().to_rfc3339(),
        "updated_at": Utc::now().to_rfc3339(),
    });
    let ws: Workstream = serde_json::from_value(legacy).expect("deserialize legacy");
    assert!(ws.metadata.is_empty());
    assert!(!ws.is_closed());
}

#[test]
fn state_is_active_iff_pointer_matches() {
    let ws = Workstream::new("A");
    let other = WorkstreamId::new();

    assert_eq!(ws.state(Some(ws.id)), WorkstreamState::Active);
    assert_eq!(ws.state(Some(other)), WorkstreamState::Idle);
    assert_eq!(ws.state(None), WorkstreamState::Idle);
}

#[test]
fn state_defaults_to_idle() {
    let ws = Workstream::new("B");
    assert_eq!(ws.state(None), WorkstreamState::Idle);
}

#[test]
fn state_is_closed_when_metadata_flag_set() {
    let mut ws = Workstream::new("C");
    ws.metadata.insert("closed".to_string(), Value::Bool(true));
    assert!(ws.is_closed());
    assert_eq!(ws.state(None), WorkstreamState::Closed);
}

#[test]
fn active_pointer_beats_closed_flag() {
    // Why: §3.2 checks the active pointer FIRST — a workstream whose
    // id happens to still equal the persisted pointer reports Active even if
    // it was also marked closed (an inconsistent state a future
    // `workstream.close` verb must avoid producing, but the read-side
    // inference must still resolve deterministically).
    let mut ws = Workstream::new("D");
    ws.metadata.insert("closed".to_string(), Value::Bool(true));
    assert_eq!(ws.state(Some(ws.id)), WorkstreamState::Active);
}

#[test]
fn closed_flag_false_is_not_closed() {
    let mut ws = Workstream::new("E");
    ws.metadata.insert("closed".to_string(), Value::Bool(false));
    assert!(!ws.is_closed());
    assert_eq!(ws.state(None), WorkstreamState::Idle);
}

#[test]
fn touch_updates_timestamp() {
    let mut ws = Workstream::new("F");
    let before = ws.updated_at;
    std::thread::sleep(std::time::Duration::from_millis(2));
    ws.touch();
    assert!(ws.updated_at > before);
    assert_eq!(ws.created_at, before, "created_at must never change");
}

#[test]
fn mark_closed_sets_flag_and_touches() {
    let mut ws = Workstream::new("G");
    let before = ws.updated_at;
    std::thread::sleep(std::time::Duration::from_millis(2));

    ws.mark_closed();

    assert!(ws.is_closed());
    assert_eq!(ws.state(None), WorkstreamState::Closed);
    assert!(ws.updated_at > before);
}

#[test]
fn state_display_matches_wire_string() {
    assert_eq!(WorkstreamState::Active.to_string(), "active");
    assert_eq!(WorkstreamState::Idle.to_string(), "idle");
    assert_eq!(WorkstreamState::Closed.to_string(), "closed");
    assert_eq!(
        serde_json::to_value(WorkstreamState::Active).unwrap(),
        serde_json::json!("active")
    );
}

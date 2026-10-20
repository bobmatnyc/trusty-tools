//! Tests for the hook-to-bus projection (#6849).
//!
//! Why: the projection is pure, so the named-hook table, the `parent_id`
//! forest and the value-level privacy rule are all provable without a socket.
//! Test: this file; the socket-level checks live in `tests.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use trusty_common::control_bus::{ActionEvent, Actor, EventId, HarnessEvent, HarnessPayload};

use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;

/// The recorded two-session hook stream (shapes from the #2864 capture).
const RECORDED: &str = include_str!("../../../testdata/hook_session_6849.ndjson");

fn recorded() -> Vec<HookEventRecord> {
    RECORDED
        .lines()
        .map(|l| serde_json::from_str(l).expect("fixture line"))
        .collect()
}

fn project_all(records: &[HookEventRecord]) -> Vec<HarnessEvent> {
    let mut threader = Threader::new(Some(PathBuf::from("/Users/dev")));
    records.iter().flat_map(|r| threader.project(r)).collect()
}

/// `domain:kind:phase` for an action, `hook:<Name>` for the untyped arm.
fn label(ev: &HarnessEvent) -> String {
    let v = serde_json::to_value(&ev.payload).expect("payload");
    match v["domain"].as_str() {
        Some("action") => format!(
            "{}:{}",
            v["event"]["kind"].as_str().unwrap_or("?"),
            v["event"]["phase"].as_str().unwrap_or("?")
        ),
        Some("hook") => format!("hook:{}", v["event"]["kind"].as_str().unwrap_or("?")),
        other => format!("other:{other:?}"),
    }
}

fn call_id(ev: &HarnessEvent) -> Option<&str> {
    match &ev.payload {
        HarnessPayload::Action(ActionEvent::Tool { call_id, .. }) => Some(call_id),
        _ => None,
    }
}

/// What the projection must turn each hook into. The issue names nine; the
/// rest stay on the untyped `Hook` arm under their own name.
fn expected(event: HookEvent) -> String {
    match event {
        HookEvent::PreToolUse => "tool:started".into(),
        HookEvent::PostToolUse => "tool:finished".into(),
        // #6849: the third phase of the same call, so a failed call closes.
        HookEvent::PostToolUseFailure => "tool:errored".into(),
        HookEvent::SessionStart => "session:started".into(),
        HookEvent::SessionEnd => "session:done".into(),
        HookEvent::FileChanged => "file:written".into(),
        HookEvent::WorktreeCreate => "workflow:start".into(),
        HookEvent::WorktreeRemove => "workflow:stop".into(),
        other => format!("hook:{}", other.wire_name()),
    }
}

fn record(session: SessionId, event: HookEvent, payload: Value) -> HookEventRecord {
    HookEventRecord::now(session, event, payload)
}

/// AC: "a projection test feeds all 32 HookEvent variants and asserts each
/// named one projects, every other stays Hook".
#[test]
fn every_hook_event_variant_projects_to_its_kind_6849() {
    assert_eq!(HookEvent::ALL.len(), 32, "the AC's count");
    let payload = json!({
        "cwd": "/w",
        "tool": "Bash",
        "tool_use_id": "toolu_1",
        "path": "/w/a.rs",
        "worktree_path": "/w/.claude/worktrees/x",
    });
    for event in HookEvent::ALL {
        let mut threader = Threader::new(None);
        let session = SessionId::new();
        if event != HookEvent::SessionStart {
            threader.project(&record(
                session,
                HookEvent::SessionStart,
                json!({"cwd": "/w"}),
            ));
        }
        let out = threader.project(&record(session, event, payload.clone()));
        assert_eq!(out.len(), 1, "{event:?} emits one event: {out:?}");
        assert_eq!(label(&out[0]), expected(event), "{event:?}");
        assert_eq!(
            out[0].session.as_deref(),
            Some(session.0.to_string().as_str())
        );
    }
}

/// AC: "events from one recorded tm session form a forest (no cycles, only
/// deliberate roots)". A parent must be emitted before its child, which rules
/// out cycles and dangling edges at once; the only roots are one
/// `session:started` per session.
#[test]
fn a_recorded_session_projects_to_a_forest_6849() {
    let records = recorded();
    assert_eq!(records.len(), 20);
    let events = project_all(&records);
    // Session B joined mid-stream, so it gains one inferred root.
    assert_eq!(
        events.len(),
        21,
        "{:#?}",
        events.iter().map(label).collect::<Vec<_>>()
    );

    let mut seen: HashMap<EventId, &HarnessEvent> = HashMap::new();
    let mut roots = Vec::new();
    for ev in &events {
        assert!(!seen.contains_key(&ev.id), "duplicate id {}", ev.id);
        match ev.parent_id {
            None => roots.push(ev),
            Some(parent) => {
                let p = seen.get(&parent).unwrap_or_else(|| {
                    panic!("{} names a parent not emitted before it", label(ev))
                });
                assert_eq!(p.session, ev.session, "an edge crossed sessions");
            }
        }
        seen.insert(ev.id, ev);
    }
    assert_eq!(roots.len(), 2, "one root per session");
    for root in &roots {
        assert_eq!(label(root), "session:started");
    }
    let sessions: HashSet<_> = roots.iter().map(|r| r.session.clone()).collect();
    assert_eq!(sessions.len(), 2);
}

/// DOC-73 §5.2: a closing event's parent is its opening event, so the tree
/// can close the node; a close with no open falls back to the session root.
#[test]
fn terminals_parent_their_opening_event_6849() {
    let events = project_all(&recorded());
    let started: HashMap<&str, EventId> = events
        .iter()
        .filter(|e| label(e) == "tool:started")
        .filter_map(|e| call_id(e).map(|c| (c, e.id)))
        .collect();
    let root_a = events[0].id;
    let mut closed = 0;
    for ev in events
        .iter()
        .filter(|e| matches!(label(e).as_str(), "tool:finished" | "tool:errored"))
    {
        let id = call_id(ev).expect("tool event");
        let want = started.get(id).copied().unwrap_or(root_a);
        assert_eq!(ev.parent_id, Some(want), "{id}");
        closed += 1;
    }
    assert_eq!(closed, 5);
    let start = events
        .iter()
        .find(|e| label(e) == "workflow:start")
        .expect("start");
    let stop = events
        .iter()
        .find(|e| label(e) == "workflow:stop")
        .expect("stop");
    assert_eq!(stop.parent_id, Some(start.id));
    let done = events
        .iter()
        .find(|e| label(e) == "session:done")
        .expect("done");
    assert_eq!(done.parent_id, Some(root_a));
    // A second SessionStart (resume, compact) is a status change, not a root.
    let again: Vec<_> = events
        .iter()
        .filter(|e| label(e) == "session:status_changed")
        .collect();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].parent_id, Some(root_a));
}

/// DOC-73 §3.2: an event names its object by reference. Nothing from a
/// prompt, a tool input or response, a transcript path or a message survives,
/// and no absolute home path does either.
#[test]
fn projected_events_carry_identifiers_not_payload_content_6849() {
    let events = project_all(&recorded());
    let text = serde_json::to_string(&events).expect("serialize");
    assert!(!text.contains("SENTINEL"), "payload content leaked: {text}");
    assert!(
        !text.contains("/Users/dev"),
        "absolute home path leaked: {text}"
    );

    let edit = events
        .iter()
        .find(|e| call_id(e) == Some("toolu_A3"))
        .expect("the subagent's Edit");
    match &edit.payload {
        HarnessPayload::Action(action) => assert_eq!(
            action.meta().actor,
            Actor::Agent {
                name: "rust-engineer".into(),
                agent_id: "a403cdbc078b5c474".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    let file = events
        .iter()
        .find(|e| label(e) == "file:written")
        .expect("file");
    match &file.payload {
        HarnessPayload::Action(ActionEvent::File { path, .. }) => {
            assert_eq!(path.path, "src/lib.rs")
        }
        other => panic!("{other:?}"),
    }
    let start = events
        .iter()
        .find(|e| label(e) == "workflow:start")
        .expect("start");
    match &start.payload {
        HarnessPayload::Action(ActionEvent::Workflow { object, .. }) => {
            assert_eq!(object.id, ".claude/worktrees/feat-x");
            assert_eq!(object.label, "feat-x");
        }
        other => panic!("{other:?}"),
    }
}

/// The threader's state is bounded: an evicted open call closes under the
/// root, and an evicted session gets a fresh inferred root.
#[test]
fn threading_state_stays_bounded_6849() {
    let mut threader = Threader::new(None);
    let s = SessionId::new();
    threader.project(&record(s, HookEvent::SessionStart, json!({})));
    let mut first = None;
    let mut last = None;
    for i in 0..(MAX_OPEN_PER_SESSION + 10) {
        let out = threader.project(&record(
            s,
            HookEvent::PreToolUse,
            json!({"tool": "Bash", "tool_use_id": format!("t{i}")}),
        ));
        first.get_or_insert(out[0].id);
        last = Some(out[0].id);
    }
    let close = |threader: &mut Threader, id: &str| {
        threader.project(&record(
            s,
            HookEvent::PostToolUse,
            json!({"tool_use_id": id}),
        ))[0]
            .parent_id
    };
    let newest = format!("t{}", MAX_OPEN_PER_SESSION + 9);
    assert_eq!(close(&mut threader, &newest), last);
    assert_ne!(close(&mut threader, "t0"), first, "t0 was evicted");

    for _ in 0..MAX_SESSIONS {
        threader.project(&record(
            SessionId::new(),
            HookEvent::SessionStart,
            json!({}),
        ));
    }
    let out = threader.project(&record(s, HookEvent::Stop, json!({})));
    assert_eq!(out.len(), 2, "the evicted session is re-rooted");
    assert_eq!(label(&out[0]), "session:started");
    assert_eq!(out[1].parent_id, Some(out[0].id));
}

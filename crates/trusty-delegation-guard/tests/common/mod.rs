//! Shared builders for the trusty-delegation-guard integration tests.
#![allow(dead_code)]

use std::path::PathBuf;

use serde::Deserialize;
use trusty_delegation_guard::{
    Authority, Boundary, BundleBuilder, CaptureInput, DgEvent, DispatchPath, Inherit, InheritVia,
    PolicyBundle, ProjectId, RawIds, RuleScope, RuleSource, RuleStatus, Sha256Hex, SourceKind,
    WorkerState,
};

pub const SESSION_ID: &str = "sess-7f3a9c-synthetic";
pub const TOOL_USE_ID: &str = "toolu_01SYNTHETICabc";

pub fn project(id: &str) -> ProjectId {
    ProjectId::new(id).expect("valid project id")
}

pub fn source<'a>(
    scope: &str,
    rule_id: &'a str,
    source_ref: &'a str,
    text: &'a str,
) -> RuleSource<'a> {
    RuleSource {
        scope: RuleScope::Project(project(scope)),
        rule_id,
        source_kind: SourceKind::Ledger,
        source_ref,
        span_start: 0,
        span_text: text,
        authority: Authority::OwnerVerified,
        effective: true,
        status: RuleStatus::Active,
    }
}

pub fn alpha_bundle() -> PolicyBundle {
    let mut b = BundleBuilder::new(project("alpha"));
    b.add_rule(source(
        "alpha",
        "alpha-cap-1",
        "ledger/alpha/0001",
        "Each child agent may start at most 2 nested workers.",
    ))
    .expect("own rule");
    b.build().expect("bundle")
}

pub fn input<'a>(boundary: Boundary, prompt: &'a str) -> CaptureInput<'a> {
    CaptureInput {
        ids: RawIds {
            session_id: SESSION_ID,
            tool_use_id: TOOL_USE_ID,
        },
        boundary,
        dispatch_path: DispatchPath::NativeAgent,
        host_id: "host-synthetic-1",
        runtime: "claude",
        tm_version: "0.0.0-test",
        captured_at: 1_700_000_000_000,
        owner: "owner-synthetic",
        agent_id: Some("agent-parent-1"),
        agent_type: Some("engineer"),
        parent_chain: vec!["agent-root".to_owned()],
        description: "Index the parser module",
        subagent_type: Some("engineer"),
        model: None,
        run_in_background: false,
        prompt,
        inherits: Vec::new(),
        worker_state: match boundary {
            Boundary::Launch => Some(worker_state(1)),
            Boundary::Contract => None,
        },
    }
}

pub fn worker_state(running: u32) -> WorkerState {
    WorkerState {
        snapshot_age_ms: 100,
        registry_epoch: 1,
        complete: true,
        running,
        queued: 0,
        background: 0,
        depth: 1,
        tree_sha256: Sha256Hex::digest(b"tree-synthetic"),
    }
}

pub fn alpha_event(boundary: Boundary) -> DgEvent {
    DgEvent::capture(
        &project("alpha"),
        &alpha_bundle(),
        input(
            boundary,
            "Index the parser module. At most 2 nested workers.",
        ),
    )
    .expect("event")
}

/// Edits one JSON field path of a serialized record and returns the new text.
pub fn edit_json(json: &str, f: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut v: serde_json::Value = serde_json::from_str(json).expect("json");
    f(&mut v);
    v.to_string()
}

// ---- A1 fixtures: packets and labels live in separate directories. ----

pub fn fixtures_dir(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(sub)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketRule {
    pub rule_id: String,
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub span_start: u64,
    pub text: String,
    pub authority: Authority,
    pub status: RuleStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketInherit {
    pub rule_id: String,
    pub via: InheritVia,
    pub verified: bool,
    pub evidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketWorkers {
    pub snapshot_age_ms: u64,
    pub registry_epoch: u64,
    pub complete: bool,
    pub running: u32,
    pub queued: u32,
    pub background: u32,
    pub depth: u32,
    pub tree: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    pub case_id: String,
    pub project_id: String,
    pub boundary: Boundary,
    pub description: String,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub run_in_background: bool,
    pub prompt: String,
    pub rules: Vec<PacketRule>,
    pub inherits: Vec<PacketInherit>,
    pub worker_state: Option<PacketWorkers>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub case_id: String,
    pub expected_verdict: String,
    pub binding_rule_ids: Vec<String>,
    pub label_note: String,
}

fn read_dir_json<T: for<'de> Deserialize<'de>>(sub: &str) -> Vec<T> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir(sub))
        .expect("fixture dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).expect("read")).expect("parse"))
        .collect()
}

pub fn packets() -> Vec<Packet> {
    read_dir_json("packets")
}

/// Test-only: labels never reach an event or a request.
pub fn labels() -> Vec<Label> {
    read_dir_json("labels")
}

/// Builds the event a packet describes, through the public constructors only.
pub fn packet_event(p: &Packet) -> DgEvent {
    let pid = project(&p.project_id);
    let mut b = BundleBuilder::new(pid.clone());
    for r in &p.rules {
        b.add_rule(RuleSource {
            scope: RuleScope::Project(pid.clone()),
            rule_id: &r.rule_id,
            source_kind: r.source_kind,
            source_ref: &r.source_ref,
            span_start: r.span_start,
            span_text: &r.text,
            authority: r.authority,
            effective: true,
            status: r.status,
        })
        .expect("packet rule");
    }
    let bundle = b.build().expect("packet bundle");
    let mut inp = input(p.boundary, &p.prompt);
    inp.description = &p.description;
    inp.subagent_type = p.subagent_type.as_deref();
    inp.model = p.model.as_deref();
    inp.run_in_background = p.run_in_background;
    inp.inherits = p
        .inherits
        .iter()
        .map(|i| Inherit {
            rule_id: i.rule_id.clone(),
            via: i.via,
            verified: i.verified,
            evidence_sha256: Sha256Hex::digest(i.evidence.as_bytes()),
        })
        .collect();
    inp.worker_state = p.worker_state.as_ref().map(|w| WorkerState {
        snapshot_age_ms: w.snapshot_age_ms,
        registry_epoch: w.registry_epoch,
        complete: w.complete,
        running: w.running,
        queued: w.queued,
        background: w.background,
        depth: w.depth,
        tree_sha256: Sha256Hex::digest(w.tree.as_bytes()),
    });
    DgEvent::capture(&pid, &bundle, inp).expect("packet event")
}

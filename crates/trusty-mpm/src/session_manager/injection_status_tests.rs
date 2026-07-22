//! Unit tests for `SessionRecord::injection_status` (#2364).
//!
//! Why: split out of `record.rs` (which was pushed over the 500-SLOC
//! production cap by the `injection_status` field addition) rather than grown
//! further, mirroring the established `set_deliverable_id_tests.rs`/
//! `set_source_id_tests.rs` sibling-test-file pattern. The pure
//! [`InjectionStatus`] enum itself (`Display`, `Default`) is tested in
//! `injection_status::tests`; this file covers its integration as a
//! `SessionRecord` field — backward-compat deserialization and serde
//! round-tripping.
//! What: `record_without_injection_status_field_defaults_to_not_applicable`
//! (legacy-record backward compat) and `record_round_trips_injection_status`
//! (serde round trip after mutation).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use chrono::Utc;

use super::injection_status::InjectionStatus;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

#[test]
fn record_without_injection_status_field_defaults_to_not_applicable() {
    // Why (#2364): every record persisted before this field existed has no
    // `injection_status` key; it MUST deserialize as `NotApplicable` — no
    // session before this field existed ever tracked delivery status.
    let legacy_json = serde_json::json!({
        "id": ManagedSessionId::new(),
        "tmux_name": "tmpm-legacy",
        "cwd": "/tmp",
        "task": "legacy task",
        "state": "active",
        "created_at": Utc::now().to_rfc3339(),
        "last_activity_at": null,
        "workspace_path": null,
        "repo_url": null,
        "branch": null,
        "pending_decision": null,
        "proposed_default": null
    })
    .to_string();
    let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
    assert_eq!(back.injection_status, InjectionStatus::NotApplicable);
}

#[test]
fn record_round_trips_injection_status() {
    let mut record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-injected".into(),
        cwd: PathBuf::from("/tmp"),
        task: "task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: InjectionStatus::Pending,
        worktree_owner: None,
    };
    record.injection_status = InjectionStatus::Success;
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.injection_status, InjectionStatus::Success);
}

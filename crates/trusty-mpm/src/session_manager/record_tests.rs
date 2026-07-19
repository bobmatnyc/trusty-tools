//! Unit tests for [`super`]'s session-record types (extracted from
//! `record.rs` to keep that production file under the 500-SLOC cap).
//!
//! Test: this file IS the test module for `session_manager::record`.

use super::*;

#[test]
fn managed_session_id_round_trip() {
    let id = ManagedSessionId::new();
    let json = serde_json::to_string(&id).expect("serialize");
    let back: ManagedSessionId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(id, back);
    assert_eq!(id.as_uuid(), back.as_uuid());
}

#[test]
fn state_display() {
    assert_eq!(
        ManagedSessionState::Provisioning.to_string(),
        "provisioning"
    );
    assert_eq!(ManagedSessionState::Active.to_string(), "active");
    assert_eq!(ManagedSessionState::Stopped.to_string(), "stopped");
    assert_eq!(ManagedSessionState::Errored.to_string(), "errored");
    assert_eq!(
        ManagedSessionState::Decommissioned.to_string(),
        "decommissioned"
    );
    assert_eq!(ManagedSessionState::Deleted.to_string(), "deleted");
}

#[test]
fn record_serde_round_trip() {
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-quiet-falcon".into(),
        cwd: PathBuf::from("/tmp/project"),
        task: "implement feature X".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: Some(Utc::now()),
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
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.id, record.id);
    assert_eq!(back.tmux_name, record.tmux_name);
    assert_eq!(back.state, record.state);
}

#[test]
fn stopped_state_survives_serde() {
    // Why: reconciliation persists Stopped state; this guards the serde
    // round-trip for the new variant.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-test".into(),
        cwd: PathBuf::from("/tmp"),
        task: "task".into(),
        state: ManagedSessionState::Stopped,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(PathBuf::from("/tmp/ws")),
        repo_url: Some("https://github.com/owner/repo".into()),
        branch: Some("main".into()),
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
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.state, ManagedSessionState::Stopped);
    assert_eq!(back.workspace_path, record.workspace_path);
}

#[test]
fn decommissioned_state_survives_serde() {
    // Why: tombstone records for decommissioned sessions must survive restarts.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-gone".into(),
        cwd: PathBuf::from("/tmp"),
        task: "task".into(),
        state: ManagedSessionState::Decommissioned,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None, // removed from disk
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
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.state, ManagedSessionState::Decommissioned);
    assert!(back.workspace_path.is_none());
}

#[test]
fn record_without_runtime_field_defaults_to_claude_code() {
    // Why: #1203 added `runtime` with `#[serde(default)]`; records persisted
    // before this field existed (no `runtime` key) must still deserialize
    // and resume on the pre-#1203 default (claude-code).
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
    assert_eq!(back.runtime, crate::runtime::RuntimeKind::ClaudeCode);
}

#[test]
fn record_round_trips_tcode_runtime() {
    // Why: a tcode-backed session must persist its runtime so `resume`
    // re-spawns on tcode, not claude-code.
    let mut record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-tcode".into(),
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
        injection_status: Default::default(),
    };
    record.runtime = crate::runtime::RuntimeKind::Tcode;
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.runtime, crate::runtime::RuntimeKind::Tcode);
}

#[test]
fn record_without_ephemeral_field_defaults_to_false() {
    // Why (#1508): the 239 legacy records — and every other pre-#1508 record —
    // have no `ephemeral` key; they MUST deserialize as non-ephemeral so the
    // automatic teardown/auto-reap paths never touch them. This pins the
    // `#[serde(default)]` → false backward-compat contract.
    let legacy_json = serde_json::json!({
        "id": ManagedSessionId::new(),
        "tmux_name": "tmpm-legacy",
        "cwd": "/tmp",
        "task": "legacy task",
        "state": "stopped",
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
    assert!(
        !back.ephemeral,
        "a record with no `ephemeral` key must default to false (non-ephemeral)"
    );
}

#[test]
fn record_round_trips_ephemeral_true() {
    // Why (#1508): a session tagged ephemeral at creation must persist the flag
    // so the bulk-teardown + age-based reap paths can later target it.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-ephemeral".into(),
        cwd: PathBuf::from("/tmp"),
        task: "throwaway".into(),
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
        ephemeral: true,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert!(back.ephemeral, "ephemeral=true must round-trip");
}

#[test]
fn record_without_workspace_owned_field_defaults_to_false() {
    // Why (#1511): every pre-#1511 record has no `workspace_owned` key; they
    // MUST deserialize as unowned (false) so the decommission path never
    // auto-deletes a workspace it did not provision. "Prefer not deleting" is
    // the safe direction — a lost workspace can be cleaned up manually.
    let legacy_json = serde_json::json!({
        "id": ManagedSessionId::new(),
        "tmux_name": "tmpm-legacy",
        "cwd": "/tmp",
        "task": "legacy task",
        "state": "stopped",
        "created_at": Utc::now().to_rfc3339(),
        "last_activity_at": null,
        "workspace_path": "/tmp/some-workspace",
        "repo_url": null,
        "branch": null,
        "pending_decision": null,
        "proposed_default": null
    })
    .to_string();
    let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
    assert!(
        !back.workspace_owned,
        "a record with no `workspace_owned` key must default to false (unowned — safe)"
    );
}

#[test]
fn record_without_scrollback_fields_defaults_to_none() {
    // Why (#1816): pre-#1816 records have no `scrollback_path` or `last_cwd`
    // keys; they MUST deserialize with both as `None` so resume continues to
    // work from workspace_path/cwd as before — zero behavior change.
    let legacy_json = serde_json::json!({
        "id": ManagedSessionId::new(),
        "tmux_name": "tmpm-legacy",
        "cwd": "/tmp",
        "task": "legacy task",
        "state": "stopped",
        "created_at": Utc::now().to_rfc3339(),
        "last_activity_at": null,
        "workspace_path": "/tmp/ws",
        "repo_url": null,
        "branch": null,
        "pending_decision": null,
        "proposed_default": null
    })
    .to_string();
    let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
    assert!(
        back.scrollback_path.is_none(),
        "scrollback_path must default to None for legacy records"
    );
    assert!(
        back.last_cwd.is_none(),
        "last_cwd must default to None for legacy records"
    );
}

#[test]
fn record_round_trips_scrollback_fields() {
    // Why (#1816): records written after idle auto-stop must persist both
    // scrollback_path and last_cwd so resume can restore context.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-snap".into(),
        cwd: PathBuf::from("/home/user/project"),
        task: "add feature".into(),
        state: ManagedSessionState::Stopped,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(PathBuf::from("/managed/ws")),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: true,
        source_id: None,
        claude_session_id: None,
        scrollback_path: Some(PathBuf::from("/managed/ws/.trusty-mpm/scrollback.txt")),
        last_cwd: Some(PathBuf::from("/managed/ws/src")),
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.scrollback_path,
        Some(PathBuf::from("/managed/ws/.trusty-mpm/scrollback.txt"))
    );
    assert_eq!(back.last_cwd, Some(PathBuf::from("/managed/ws/src")));
}

#[test]
fn record_round_trips_workspace_owned_true() {
    // Why (#1511): a clone-provisioned session must persist workspace_owned=true
    // so decommission knows it is safe to remove the workspace.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-clone".into(),
        cwd: PathBuf::from("/managed/root/owner/repo/abc"),
        task: "fix bug".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(PathBuf::from("/managed/root/owner/repo/abc")),
        repo_url: Some("https://github.com/owner/repo".into()),
        branch: Some("fix/thing".into()),
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: true,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert!(back.workspace_owned, "workspace_owned=true must round-trip");
}

#[test]
fn record_without_deliverable_id_field_defaults_to_none() {
    // Why (#2379): every record persisted before this field existed has no
    // `deliverable_id` key; it MUST deserialize as `None` (unbound) — no
    // session created before the Deliverable layer existed was ever bound
    // to one. This pins the `#[serde(default)]` back-compat contract that
    // lets an old store load cleanly under the new binary, and (by the
    // same additive-field contract) lets an OLD binary reading a NEWER
    // store simply ignore the extra key it does not know about.
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
    assert!(
        back.deliverable_id.is_none(),
        "a record with no `deliverable_id` key must default to None (unbound)"
    );
}

#[test]
fn record_round_trips_deliverable_id() {
    // Why (#2379): a session bound via `tm sessions new --deliverable <id>`
    // must persist the link so `resume`/`ls`/`status` all see it.
    let did = crate::deliverable::DeliverableId::new();
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-bound".into(),
        cwd: PathBuf::from("/tmp"),
        task: "implement WI-13".into(),
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
        deliverable_id: Some(did),
        pane_id: None,
        injection_status: Default::default(),
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.deliverable_id, Some(did));
}

#[test]
fn is_terminal_covers_tombstones() {
    // Terminal tombstones — must never be offered for resume/attach/restart.
    assert!(ManagedSessionState::Decommissioned.is_terminal());
    assert!(ManagedSessionState::Deleted.is_terminal());
    // Live / resumable states are NOT terminal.
    assert!(!ManagedSessionState::Provisioning.is_terminal());
    assert!(!ManagedSessionState::Active.is_terminal());
    assert!(!ManagedSessionState::Stopped.is_terminal());
    assert!(!ManagedSessionState::Errored.is_terminal());
}

#[test]
fn from_wire_round_trips_every_variant() {
    // `from_wire` is the inverse of Display for every variant, and rejects junk.
    for state in [
        ManagedSessionState::Provisioning,
        ManagedSessionState::Active,
        ManagedSessionState::Stopped,
        ManagedSessionState::Errored,
        ManagedSessionState::Decommissioned,
        ManagedSessionState::Deleted,
    ] {
        assert_eq!(
            ManagedSessionState::from_wire(&state.to_string()),
            Some(state.clone()),
            "from_wire must round-trip {state:?}"
        );
    }
    assert_eq!(ManagedSessionState::from_wire("bogus"), None);
    assert_eq!(ManagedSessionState::from_wire(""), None);
}

//! Unit tests for managed-routes serializers.
//!
//! Why: isolating these tests into a `tests.rs` sibling keeps the production
//! file under the 500-SLOC cap while giving the test module the generous
//! 1500-SLOC budget.
//! What: focused assertions on `record_to_json` and `record_to_summary`.
//! Test: this file.

use std::path::PathBuf;

use chrono::Utc;

use super::{record_to_json, record_to_summary};
use crate::session_manager::{
    InjectionStatus, ManagedSessionId, ManagedSessionState, SessionRecord,
};

/// Build a minimal [`SessionRecord`] suitable for serialization tests.
fn make_record(source_id: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-test-session".into(),
        cwd: PathBuf::from("/tmp/test"),
        task: "test task".into(),
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
        source_id: source_id.map(String::from),
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    }
}

/// Why: both the MCP path (`record_to_json`) and the HTTP path
/// (`record_to_summary`) must expose `source_id` so callers on either
/// transport can filter or reconnect by project identity (#1733).
/// What: asserts that a record with `source_id = Some("owner/repo")` is
/// reflected by both serializers, and that a record with `source_id = None`
/// serializes as JSON `null` from `record_to_json` and as `None` from
/// `record_to_summary`.
/// Test: this test.
#[test]
fn serializers_include_source_id() {
    // ── Case 1: source_id is set ──────────────────────────────────────────
    let r = make_record(Some("owner/repo"));

    // record_to_json (MCP path) must include source_id as a string.
    let json = record_to_json(&r);
    assert_eq!(
        json["source_id"].as_str(),
        Some("owner/repo"),
        "record_to_json must serialize source_id when present"
    );

    // record_to_summary (HTTP path) must include source_id.
    let summary = record_to_summary(&r);
    assert_eq!(
        summary.source_id.as_deref(),
        Some("owner/repo"),
        "record_to_summary must include source_id when present"
    );

    // ── Case 2: source_id is None ─────────────────────────────────────────
    let r_none = make_record(None);

    // record_to_json must serialize source_id as JSON null, not omit it.
    let json_none = record_to_json(&r_none);
    assert!(
        json_none["source_id"].is_null(),
        "record_to_json must serialize source_id as null when absent, got {:?}",
        json_none["source_id"]
    );

    // record_to_summary must carry None.
    let summary_none = record_to_summary(&r_none);
    assert_eq!(
        summary_none.source_id, None,
        "record_to_summary must carry None source_id when absent"
    );
}

/// Why: `record_to_json` (MCP path) and `record_to_summary` (HTTP path) must
/// both surface `deliverable_id` (DOC-35 §10.6, #2379) so `tm sessions ls`/
/// `status` and the MCP tools can show which Deliverable a session is bound
/// to, exactly as they already do for `source_id`.
/// What: asserts a bound session serializes the stringified id on both paths,
/// and an unbound session serializes JSON `null` / `None` respectively.
/// Test: this test.
#[test]
fn serializers_include_deliverable_id() {
    use crate::deliverable::DeliverableId;

    // ── Case 1: deliverable_id is set ─────────────────────────────────────
    let mut r = make_record(None);
    let did = DeliverableId::new();
    r.deliverable_id = Some(did);

    let json = record_to_json(&r);
    assert_eq!(
        json["deliverable_id"].as_str(),
        Some(did.to_string().as_str()),
        "record_to_json must serialize deliverable_id when present"
    );
    let summary = record_to_summary(&r);
    assert_eq!(
        summary.deliverable_id.as_deref(),
        Some(did.to_string().as_str()),
        "record_to_summary must include deliverable_id when present"
    );

    // ── Case 2: deliverable_id is None (the common, unbound case) ─────────
    let r_none = make_record(None);
    let json_none = record_to_json(&r_none);
    assert!(
        json_none["deliverable_id"].is_null(),
        "record_to_json must serialize deliverable_id as null when absent"
    );
    let summary_none = record_to_summary(&r_none);
    assert_eq!(
        summary_none.deliverable_id, None,
        "record_to_summary must carry None deliverable_id when absent"
    );
}

/// Verify that `DecommissionResponse.workspace_removed` correctly reflects
/// workspace ownership rather than being inferred from the post-call filesystem.
///
/// Why: the TOCTOU issue (#1788 review) means a post-hoc `!p.exists()` check
/// gives `workspace_removed=true` even for workspaces that were ALREADY absent
/// before decommission ran. The response struct must be populated from the value
/// returned by `decommission_with_root` (which tracks whether `remove_dir_all`
/// actually ran), not from a filesystem re-check.
/// What: validates the `DecommissionResponse` serde contract — `workspace_removed`
/// is a bool present in the JSON, and `workspace_path_was` is an optional string
/// present only for owned sessions.
/// Test: this test; the runtime path is covered by
/// `manager_decommission_removes_workspace` (owned=true → workspace_removed=true)
/// and `manager_decommission_unowned_skips_deletion` (owned=false → false).
#[test]
fn decommission_workspace_removed_reflects_ownership() {
    use super::{DecommissionResponse, SessionSummary};

    // ── owned session: workspace_removed = true, workspace_path_was = Some ──
    let owned_summary = SessionSummary {
        id: "abc-123".into(),
        name: "tmpm-test".into(),
        state: "decommissioned".into(),
        workspace_path: None, // cleared in tombstone
        repo_url: None,
        branch: None,
        created_at: "2025-06-28T00:00:00Z".into(),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
    };
    let resp_owned = DecommissionResponse {
        summary: owned_summary,
        workspace_removed: true,
        workspace_path_was: Some("/workspaces/trusty-mpm/session-abc".into()),
    };
    let json = serde_json::to_value(&resp_owned).unwrap();
    assert_eq!(json["workspace_removed"], true, "owned: must be true");
    assert_eq!(
        json["workspace_path_was"].as_str(),
        Some("/workspaces/trusty-mpm/session-abc"),
        "owned: workspace_path_was must be present"
    );

    // ── unowned session: workspace_removed = false, workspace_path_was absent ──
    let unowned_summary = SessionSummary {
        id: "xyz-456".into(),
        name: "tmpm-adopted".into(),
        state: "decommissioned".into(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: "2025-06-28T00:00:00Z".into(),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
    };
    let resp_unowned = DecommissionResponse {
        summary: unowned_summary,
        workspace_removed: false,
        workspace_path_was: None,
    };
    let json2 = serde_json::to_value(&resp_unowned).unwrap();
    assert_eq!(json2["workspace_removed"], false, "unowned: must be false");
    assert!(
        json2.get("workspace_path_was").is_none(),
        "unowned: workspace_path_was must be absent (skip_serializing_if = None)"
    );
}

/// Why (#2364): a session injection was never attempted for must omit the
/// `injection_status` key entirely on both wire paths, rather than emitting
/// the literal string `"not_applicable"` — the field should stay silent for
/// the (common) case where turnkey injection never applies to a session.
/// What: asserts `record_to_json` serializes `injection_status` as JSON
/// `null` and `record_to_summary` carries `None` when the record's
/// `injection_status` is the `NotApplicable` default.
/// Test: this test.
#[test]
fn injection_status_wire_omits_not_applicable() {
    let r = make_record(None);
    assert_eq!(r.injection_status, InjectionStatus::NotApplicable);

    let json = record_to_json(&r);
    assert!(
        json["injection_status"].is_null(),
        "record_to_json must serialize NotApplicable as null, got {:?}",
        json["injection_status"]
    );

    let summary = record_to_summary(&r);
    assert_eq!(
        summary.injection_status, None,
        "record_to_summary must carry None for NotApplicable"
    );
}

/// Why (#2364): callers polling delivery status need every non-trivial
/// [`InjectionStatus`] variant to round-trip through BOTH wire paths as its
/// snake_case string form, so `tm session info`/`tm sessions ls` can surface
/// exactly `pending`/`success`/`failed_timeout`/`failed_session_died`.
/// What: for each non-`NotApplicable` variant, asserts `record_to_json`
/// serializes the matching string and `record_to_summary` carries
/// `Some(<string>)`.
/// Test: this test.
#[test]
fn injection_status_wire_stringifies_other_variants() {
    let cases = [
        (InjectionStatus::Pending, "pending"),
        (InjectionStatus::Success, "success"),
        (InjectionStatus::FailedTimeout, "failed_timeout"),
        (InjectionStatus::FailedSessionDied, "failed_session_died"),
    ];
    for (status, expected) in cases {
        let mut r = make_record(None);
        r.injection_status = status;

        let json = record_to_json(&r);
        assert_eq!(
            json["injection_status"].as_str(),
            Some(expected),
            "record_to_json must serialize {status:?} as {expected:?}"
        );

        let summary = record_to_summary(&r);
        assert_eq!(
            summary.injection_status.as_deref(),
            Some(expected),
            "record_to_summary must carry {expected:?} for {status:?}"
        );
    }
}

// `unresumable_response`'s header-tagging behavior is unit-tested beside its
// definition in `resume_error.rs` (#2577 review) — see
// `resume_error::tests::unresumable_response_tags_reason_header_per_failure_class`.

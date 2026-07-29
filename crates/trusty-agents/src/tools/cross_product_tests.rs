//! Tests for `tools::cross_product` — the propose-only envelope (#4028),
//! epic #4021's bridge track.
//!
//! Why: every assertion here pins an owner ruling or a spec rule that a future
//! edit could silently relax: DOC-41 §5.5's absolute propose-not-authorize
//! rule and #2809's 4 KiB handoff cap.
//! What: unit tests over `HandoffContext` and `ProposalEnvelope`. No I/O, no
//! subprocesses — the dispatch-side fail-closed behaviour (nothing dispatched
//! on denial) is pinned in `pm_bridge_tests.rs` where a recording backend can
//! observe it, and the allow-set's own coverage moved to
//! `subagent_allow_tests` when ADR-0024 decision 4 made the gate shared.

use super::*;

// =====================================================================
// HandoffContext — #2809-shaped, 4 KiB cap (#4028, OQ-6)
// =====================================================================

/// A handoff within the cap validates.
#[test]
fn handoff_at_cap_is_accepted() {
    let handoff = HandoffContext {
        summary: Some("x".repeat(64)),
        relevant_state: None,
        constraints: vec!["stay read-only".to_string()],
    };
    assert!(handoff.validate().is_ok());
    assert!(serde_json::to_vec(&handoff).unwrap().len() <= HANDOFF_MAX_BYTES);
}

/// An oversized handoff is a recoverable error carrying the actual size.
#[test]
fn handoff_over_cap_is_rejected() {
    let handoff = HandoffContext {
        summary: Some("x".repeat(HANDOFF_MAX_BYTES + 1)),
        relevant_state: None,
        constraints: Vec::new(),
    };
    let err = handoff.validate().expect_err("over-cap handoff must fail");
    assert!(err > HANDOFF_MAX_BYTES, "reported size {err} not over cap");
}

/// An all-empty handoff renders nothing, so the no-handoff path is unchanged.
#[test]
fn empty_handoff_renders_nothing() {
    assert!(HandoffContext::default().is_empty());
    assert!(HandoffContext::default().render_preamble().is_none());
}

/// A populated handoff renders a labelled preamble naming only set fields.
#[test]
fn handoff_renders_into_the_task_preamble() {
    let handoff = HandoffContext {
        summary: Some("backlog triage".to_string()),
        relevant_state: None,
        constraints: vec!["do not close anything".to_string()],
    };
    let rendered = handoff
        .render_preamble()
        .expect("non-empty handoff renders");
    assert!(rendered.contains("backlog triage"));
    assert!(rendered.contains("do not close anything"));
    assert!(
        !rendered.contains("Relevant state"),
        "unset field must not be rendered: {rendered}"
    );
}

// =====================================================================
// ProposalEnvelope — propose-not-authorize (#4028, DOC-41 §5.5)
// =====================================================================

/// DOC-41 §5.5 line 1398, absolute: a cross-product result is a PROPOSAL,
/// never an action — for BOTH caller authority tiers.
#[test]
fn cross_product_result_is_always_a_proposal() {
    for authority in [CallerAuthority::Standard, CallerAuthority::UserAuthority] {
        let env = ProposalEnvelope::for_cross_product("assistant", "ticketing", authority, "ok");
        assert_eq!(env.disposition, Disposition::Proposal);
        assert_ne!(env.disposition, Disposition::Action);
    }
}

/// #3078/AUTH-5 mirrored to the bridge: holding `user_authority` is RECORDED
/// on the envelope but never upgrades the target's output out of proposal
/// status — the caller must act in its own turn, under its own identity.
#[test]
fn envelope_records_caller_authority_without_upgrading_disposition() {
    let env = ProposalEnvelope::for_cross_product(
        "assistant",
        "research",
        CallerAuthority::UserAuthority,
        "findings",
    );
    assert_eq!(env.authority, CallerAuthority::UserAuthority);
    assert_eq!(env.disposition, Disposition::Proposal);
}

/// The envelope's serialized shape carries every field #4028 requires:
/// origin agent, target agent, authority tier, proposal-vs-action marker.
#[test]
fn envelope_json_shape() {
    let env = ProposalEnvelope::for_cross_product(
        "assistant",
        "ticketing",
        CallerAuthority::Standard,
        "drafted ISS-1",
    );
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&env).expect("envelope serializes"))
            .expect("envelope is valid json");

    assert_eq!(value["origin_agent"], "assistant");
    assert_eq!(value["target_agent"], "ticketing");
    assert_eq!(value["authority"], "standard");
    assert_eq!(value["disposition"], "proposal");
    assert_eq!(value["result"], "drafted ISS-1");

    let rendered = env.render();
    assert!(rendered.contains("PROPOSAL"), "rendered: {rendered}");
    assert!(rendered.contains("\"disposition\": \"proposal\""));
}

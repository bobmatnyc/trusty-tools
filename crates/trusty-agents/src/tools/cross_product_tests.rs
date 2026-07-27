//! Tests for `tools::cross_product` — the bridge-layer allow-set (#4026) and
//! the propose-only envelope (#4028), epic #4021's bridge track.
//!
//! Why: every assertion here pins an owner ruling or a spec rule that a future
//! edit could silently relax: OQ-7's fail-closed bridge enforcement, the
//! EMPTY-by-default allow-set (no silent capability grant), DOC-41 §5.5's
//! absolute propose-not-authorize rule, and #2809's 4 KiB handoff cap.
//! What: unit tests over `SubagentAllowSet`, `HandoffContext`, and
//! `ProposalEnvelope`. No I/O, no subprocesses — the dispatch-side
//! fail-closed behaviour (nothing dispatched on denial) is pinned in
//! `pm_bridge_tests.rs` where a recording backend can observe it.

use super::*;

// =====================================================================
// Allow-set — fail-closed resolution (#4026, OQ-7)
// =====================================================================

/// The DEFAULT allow-set grants nothing: an agent with no `[subagents]`
/// section cannot reach ANY cross-product specialist, not even a non-coding
/// one. Pins "no silent capability grant".
#[test]
fn empty_default_allow_set_denies_everything() {
    let set = SubagentAllowSet::empty();
    assert!(set.is_empty());
    for name in NON_CODING_TARGETS {
        assert_eq!(
            set.resolve(name),
            Err(TargetDenied::NotGranted((*name).to_string())),
            "empty allow-set must deny the non-coding target '{name}'"
        );
    }
}

/// `Default` and `empty()` are the same posture — a `SubagentAllowSet` that
/// materializes through `#[derive(Default)]` must not be permissive.
#[test]
fn default_impl_is_the_empty_allow_set() {
    assert!(SubagentAllowSet::default().is_empty());
}

/// An absent `[subagents].allowed` list (serde `None`) is the empty set.
#[test]
fn from_allowed_none_is_empty() {
    assert!(SubagentAllowSet::from_allowed(None).is_empty());
}

/// Entries are trimmed, lowercased and de-duplicated; blanks are dropped.
#[test]
fn from_allowed_normalizes_entries() {
    let raw = vec![
        "  Research ".to_string(),
        "research".to_string(),
        "   ".to_string(),
        "TICKETING".to_string(),
    ];
    let set = SubagentAllowSet::from_allowed(Some(&raw));
    assert_eq!(set.resolve("research").unwrap(), "research");
    assert_eq!(set.resolve("  TiCkEtInG  ").unwrap(), "ticketing");
}

/// A named, granted, non-coding target resolves.
#[test]
fn allow_set_accepts_a_named_non_coding_target() {
    let set = SubagentAllowSet::from_allowed(Some(&["research".to_string()]));
    assert_eq!(set.resolve("research").unwrap(), "research");
    // ...but a non-coding target the caller did NOT grant is still denied.
    assert_eq!(
        set.resolve("ticketing"),
        Err(TargetDenied::NotGranted("ticketing".to_string()))
    );
}

/// #4027: the ported ticketing agent is reachable once granted.
#[test]
fn allow_set_accepts_ticketing_once_granted() {
    let set = SubagentAllowSet::from_allowed(Some(&["ticketing".to_string()]));
    assert_eq!(set.resolve("ticketing").unwrap(), "ticketing");
    assert!(NON_CODING_TARGETS.contains(&"ticketing"));
}

/// OQ-7's core ruling: the BRIDGE hard-denies a coding target even when the
/// calling agent's own config explicitly lists it. Caller configuration can
/// only ever NARROW the non-coding floor, never widen it.
#[test]
fn non_coding_floor_rejects_a_coding_target_even_when_config_allows_it() {
    let permissive = vec![
        "rust-engineer".to_string(),
        "engineer".to_string(),
        "pm".to_string(),
        "research".to_string(),
    ];
    let set = SubagentAllowSet::from_allowed(Some(&permissive));
    for coding in ["rust-engineer", "engineer", "pm"] {
        assert_eq!(
            set.resolve(coding),
            Err(TargetDenied::NotNonCoding(coding.to_string())),
            "bridge floor must deny coding target '{coding}' despite config"
        );
    }
    // The one legitimate entry in the same list still resolves.
    assert_eq!(set.resolve("research").unwrap(), "research");
}

/// A blank/whitespace name is rejected before any lookup.
#[test]
fn blank_target_is_rejected() {
    let set = SubagentAllowSet::from_allowed(Some(&["research".to_string()]));
    assert_eq!(set.resolve("   "), Err(TargetDenied::Blank));
    assert_eq!(set.resolve(""), Err(TargetDenied::Blank));
}

/// Both denial reasons render the SAME generic message so a caller cannot
/// probe the floor list by diffing error text.
#[test]
fn denial_messages_do_not_distinguish_floor_from_grant() {
    let floor = TargetDenied::NotNonCoding("engineer".to_string()).to_string();
    let grant = TargetDenied::NotGranted("engineer".to_string()).to_string();
    assert_eq!(floor, grant);
    assert!(floor.contains("not available"), "unexpected text: {floor}");
}

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

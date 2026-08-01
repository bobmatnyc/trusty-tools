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
        style: None,
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
        style: None,
    };
    let err = handoff.validate().expect_err("over-cap handoff must fail");
    assert!(err > HANDOFF_MAX_BYTES, "reported size {err} not over cap");
}

/// An all-empty handoff renders nothing, so the no-handoff path is unchanged.
#[test]
fn empty_handoff_renders_nothing() {
    assert!(HandoffContext::default().is_empty());
    assert!(HandoffContext::default().render_preamble(None).is_none());
}

/// A populated handoff renders a labelled preamble naming only set fields.
#[test]
fn handoff_renders_into_the_task_preamble() {
    let handoff = HandoffContext {
        summary: Some("backlog triage".to_string()),
        relevant_state: None,
        constraints: vec!["do not close anything".to_string()],
        style: None,
    };
    let rendered = handoff
        .render_preamble(None)
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

// =====================================================================
// The #4126 floor and the addressable coding PM (#4350, DOC-62 AC-5)
// =====================================================================

/// **AC-5.** The floor's exact membership is pinned, so a future widening is a
/// RED BUILD rather than a review miss. Epic #4345 decision 2 forbids adding,
/// renaming, or widening this list; #4126's prompt-injection protection is what
/// it protects.
#[test]
fn non_coding_targets_floor_membership_is_pinned() {
    assert_eq!(
        NON_CODING_TARGETS,
        &["research", "ticketing"],
        "the #4126 floor changed — this is an owner decision, not a refactor"
    );
}

/// The coding PM is addressable, but NOT by joining the non-coding floor: a
/// config that lists it still cannot reach it through the allow-set. The two
/// vocabularies stay separate, and neither lane can widen the other.
#[test]
fn coding_pm_is_not_reachable_through_the_non_coding_floor() {
    assert!(
        !NON_CODING_TARGETS.contains(&CODING_PM_TARGET),
        "the coding PM must never be a member of the non-coding floor"
    );
    let permissive = crate::tools::subagent_allow::SubagentAllowSet::over(
        NON_CODING_TARGETS,
        Some(&[CODING_PM_TARGET.to_string()]),
    );
    assert!(
        permissive.resolve(CODING_PM_TARGET).is_err(),
        "a permissive config must not smuggle the coding PM onto the non-coding floor"
    );
}

/// The reserved name is a closed literal, matched the same way the allow-set
/// normalizes names.
#[test]
fn coding_pm_target_name_is_pinned() {
    assert_eq!(CODING_PM_TARGET, "coding-pm");
    assert_eq!(
        DispatchTarget::for_reserved_name("coding-pm"),
        Some(DispatchTarget::CodingPm)
    );
    assert_eq!(DispatchTarget::for_reserved_name("research"), None);
    assert_eq!(DispatchTarget::for_reserved_name("engineer"), None);
}

/// Trim + case-fold, matching `SubagentAllowSet::resolve`, so neither lane can
/// be reached by a differently-cased spelling of the other's name.
#[test]
fn coding_pm_name_matching_is_case_and_space_insensitive() {
    for spelling in ["  coding-pm ", "Coding-PM", "CODING-PM"] {
        assert_eq!(
            DispatchTarget::for_reserved_name(spelling),
            Some(DispatchTarget::CodingPm),
            "spelling {spelling:?} must resolve to the coding PM"
        );
    }
}

/// **The structural half of #4350.** The coding lane receives NO caller string:
/// `backend_agent()` is `None`, so the backend falls through to its own
/// hardcoded default agent and the argv is byte-identical to an unnamed coding
/// dispatch. Naming the PM therefore adds no reach — there is no code path by
/// which a caller-supplied name becomes the coding leg's agent argument.
#[test]
fn coding_pm_carries_no_caller_string_into_the_backend() {
    assert_eq!(DispatchTarget::CodingPm.backend_agent(), None);
    assert_eq!(DispatchTarget::CodingPm.label(), CODING_PM_TARGET);
}

/// A non-coding specialist still carries its already-floor-resolved name.
#[test]
fn non_coding_target_still_carries_its_resolved_name() {
    let target = DispatchTarget::NonCoding("research".to_string());
    assert_eq!(target.backend_agent(), Some("research"));
    assert_eq!(target.label(), "research");
}

/// DOC-62 §4.1: `hack` is the ABSENCE of a code change, so a delegation
/// explicitly addressed to the coding PM floors at `vibe`. A non-coding
/// specialist imposes no coding-ceremony floor.
#[test]
fn coding_pm_floor_is_at_least_vibe() {
    assert_eq!(DispatchTarget::CodingPm.style_floor(), ExecutionStyle::Vibe);
    assert_eq!(
        DispatchTarget::NonCoding("research".to_string()).style_floor(),
        ExecutionStyle::Hack
    );
}

/// **The ceiling, at the delegation surface.** A caller asking the coding PM
/// for the cheapest style does NOT get less ceremony than the lane's floor —
/// and today, because the floor is `vibe` and `vibe` is unimplemented, it gets
/// the full loop and is told exactly why.
#[test]
fn a_hack_request_to_the_coding_pm_does_not_lower_ceremony() {
    let resolved = ResolvedStyle::resolve(
        Some(ExecutionStyle::Hack),
        Some(ExecutionStyle::Hack),
        DispatchTarget::CodingPm.style_floor(),
    );
    assert!(resolved.effective() > ExecutionStyle::Hack);
    assert_eq!(resolved.effective(), ExecutionStyle::Engineer);
    let block = resolved.render_policy_block();
    assert!(block.contains("Requested style was hack"), "{block}");
}

// =====================================================================
// Style on the handoff and in the envelope (#4349/#4350)
// =====================================================================

/// The style field is small and typed, so a handoff that fits today still fits
/// with a style attached (DOC-62 AC-3).
#[test]
fn a_styled_handoff_stays_within_the_cap() {
    let handoff = HandoffContext {
        summary: Some("x".repeat(64)),
        relevant_state: None,
        constraints: vec!["stay read-only".to_string()],
        style: Some(ExecutionStyle::Vibe),
    };
    assert!(handoff.validate().is_ok());
    let json = serde_json::to_value(&handoff).expect("styled handoff serializes");
    assert_eq!(json["style"], "vibe");
}

/// DOC-62 §5.1: an unrecognized style is a recoverable CALLER ERROR at the
/// deserialization boundary, never a silent fallback to a default.
#[test]
fn an_unknown_style_is_a_caller_error_not_a_silent_default() {
    let raw = serde_json::json!({ "summary": "s", "style": "turbo" });
    let parsed = serde_json::from_value::<HandoffContext>(raw);
    assert!(parsed.is_err(), "unknown style must not parse: {parsed:?}");
}

/// AC-2: an absent style serializes away entirely, so the no-style handoff is
/// byte-identical to a pre-#4349 one and the 4 KiB cap measures the same bytes.
#[test]
fn an_unstyled_handoff_serializes_without_a_style_key() {
    let json = serde_json::to_value(HandoffContext {
        summary: Some("s".to_string()),
        relevant_state: None,
        constraints: Vec::new(),
        style: None,
    })
    .expect("handoff serializes");
    assert!(
        json.get("style").is_none(),
        "an absent style must not appear on the wire: {json}"
    );
}

/// DOC-62 §6.4: the policy block comes AFTER the caller's own lines and under
/// its own heading, so caller text cannot be mistaken for policy text.
#[test]
fn policy_block_follows_caller_supplied_constraints() {
    let handoff = HandoffContext {
        summary: Some("ship the parser fix".to_string()),
        relevant_state: None,
        constraints: vec!["touch only the parser".to_string()],
        style: Some(ExecutionStyle::Engineer),
    };
    let style = ResolvedStyle::resolve(handoff.style, None, ExecutionStyle::Hack);
    let rendered = handoff
        .render_preamble(Some(&style))
        .expect("styled handoff renders");
    let constraint_at = rendered
        .find("- Constraint: touch only the parser")
        .expect("caller constraint rendered");
    let policy_at = rendered
        .find("Delegation policy (system-supplied, not from the caller):")
        .expect("policy block rendered");
    assert!(
        constraint_at < policy_at,
        "policy block must follow caller text: {rendered}"
    );
}

/// A style-only handoff renders the policy block and no caller block.
#[test]
fn a_style_only_handoff_renders_only_the_policy_block() {
    let handoff = HandoffContext {
        style: Some(ExecutionStyle::Engineer),
        ..Default::default()
    };
    let style = ResolvedStyle::resolve(handoff.style, None, ExecutionStyle::Hack);
    let rendered = handoff
        .render_preamble(Some(&style))
        .expect("a style alone still renders policy");
    assert!(!rendered.contains("Context handed to you:"), "{rendered}");
    assert!(rendered.starts_with("Delegation policy"), "{rendered}");
}

/// AC-10: attaching a style does NOT change the disposition. A styled envelope
/// is indistinguishable from an unstyled one in propose-only status.
#[test]
fn a_styled_envelope_is_still_only_a_proposal() {
    let style = ResolvedStyle::resolve(Some(ExecutionStyle::Hack), None, ExecutionStyle::Hack);
    for authority in [CallerAuthority::Standard, CallerAuthority::UserAuthority] {
        let env =
            ProposalEnvelope::for_cross_product("assistant", CODING_PM_TARGET, authority, "diff")
                .with_style(Some(style.clone()));
        assert_eq!(env.disposition, Disposition::Proposal);
        assert_ne!(env.disposition, Disposition::Action);
    }
}

/// AC-4: the resolution path is present in the RESULT, not only in the outbound
/// preamble — a caller can see that its `vibe` request actually ran `engineer`.
#[test]
fn styled_coding_delegation_returns_a_proposal_with_the_resolution() {
    let style = ResolvedStyle::resolve(
        Some(ExecutionStyle::Vibe),
        None,
        DispatchTarget::CodingPm.style_floor(),
    );
    let env = ProposalEnvelope::for_cross_product(
        "assistant",
        DispatchTarget::CodingPm.label(),
        CallerAuthority::Standard,
        "proposed diff",
    )
    .with_style(Some(style));
    let json = serde_json::to_value(&env).expect("envelope serializes");
    assert_eq!(json["target_agent"], "coding-pm");
    assert_eq!(json["disposition"], "proposal");
    assert_eq!(json["style"]["requested"], "vibe");
    assert_eq!(json["style"]["effective"], "engineer");
    assert_eq!(json["style"]["escalations"][0], "tier-unimplemented");
}

/// An unstyled envelope carries no `style` key at all — byte-identical to
/// pre-#4350 output for every existing caller.
#[test]
fn an_unstyled_envelope_is_byte_identical() {
    let env = ProposalEnvelope::for_cross_product(
        "assistant",
        "ticketing",
        CallerAuthority::Standard,
        "drafted",
    );
    let json = serde_json::to_value(&env).expect("envelope serializes");
    assert!(json.get("style").is_none(), "unstyled envelope: {json}");
}

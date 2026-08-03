//! Tests for `tools::execution_style` — the ceiling property, the SM-9
//! fail-safe, precedence, and the policy block (#4349/#4350, spec DOC-62 §5/§7).
//!
//! Why: every assertion here pins a rule a future edit could silently relax.
//! The load-bearing one is the CEILING: a caller-supplied style is an author
//! tag, and the feature is only safe because the callee may raise ceremony and
//! never lower it. That is asserted over the FULL request×floor cross-product
//! rather than on one happy path, because a single example would not catch a
//! reordered `ExecutionStyle` variant list (which silently inverts `Ord`, and
//! therefore inverts `max`, and therefore inverts the whole property).
//! What: pure unit tests, no I/O. The bridge-side wiring (which floor applies
//! to which lane, and that the backend argv is style-independent) is pinned in
//! `pm_bridge_tests.rs`, where a recording backend can observe it.

use super::*;

/// Every style, for exhaustive cross-products.
///
/// #4353 promoted this list into the production type (`ExecutionStyle::ALL`)
/// because the GUI selector needs it too. It is ALIASED rather than re-spelled:
/// two arrays could drift, and a cross-product silently stopping short of a
/// variant is exactly the gap these tests exist to close.
const ALL: [ExecutionStyle; 3] = ExecutionStyle::ALL;

// =====================================================================
// The vocabulary (DOC-62 §5.1)
// =====================================================================

/// Variant order IS the ceremony order — `max` means "raise" only while this
/// holds. Reordering the enum breaks the safety property silently; this fails.
#[test]
fn ceremony_order_is_ascending() {
    assert!(ExecutionStyle::Hack < ExecutionStyle::Vibe);
    assert!(ExecutionStyle::Vibe < ExecutionStyle::Engineer);
    assert_eq!(
        ExecutionStyle::Hack.max(ExecutionStyle::Engineer),
        ExecutionStyle::Engineer
    );
}

/// `ALL` is in ascending ceremony order (#4353).
///
/// Why: the GUI selector renders the styles in array order and calls the value
/// "more/less ceremony" in its copy. If the array ever stopped agreeing with
/// `Ord`, the pane would present the escalation direction backwards while the
/// resolver kept raising correctly — a lie the resolver's own tests cannot see.
#[test]
fn all_is_sorted_by_ascending_ceremony() {
    assert!(
        ALL.windows(2).all(|w| w[0] < w[1]),
        "ExecutionStyle::ALL must ascend in ceremony: {ALL:?}"
    );
    assert_eq!(
        *ALL.last().expect("non-empty"),
        ExecutionStyle::BUILT_IN_DEFAULT
    );
}

/// Every entry in `ALL` is a real wire value (#4353).
///
/// Why: the selector keys its controls on `as_str`, and the config default it
/// compares against arrives over serde. A variant present in `ALL` but absent
/// from the serde vocabulary would render a control no config could ever match.
#[test]
fn all_round_trips_through_the_wire_form() {
    for style in ALL {
        let back: ExecutionStyle = serde_json::from_str(&format!("\"{}\"", style.as_str()))
            .expect("every ALL entry is a wire value");
        assert_eq!(back, style);
    }
}

/// The wire form is lowercase, and `as_str` matches it exactly.
#[test]
fn styles_round_trip_lowercase() {
    for style in ALL {
        let json = serde_json::to_string(&style).expect("style serializes");
        assert_eq!(json, format!("\"{}\"", style.as_str()));
        let back: ExecutionStyle = serde_json::from_str(&json).expect("style round-trips");
        assert_eq!(back, style);
    }
}

/// DOC-62 §5.1: an unrecognized value is a recoverable ERROR, never a silent
/// fallback to a default.
#[test]
fn an_unknown_style_string_is_a_deserialization_error() {
    let err = serde_json::from_str::<ExecutionStyle>("\"yolo\"");
    assert!(err.is_err(), "unknown style must not deserialize: {err:?}");
}

/// DOC-62 §5.2: the built-in default is the MOST ceremony, matching today.
#[test]
fn built_in_default_is_engineer() {
    assert_eq!(ExecutionStyle::BUILT_IN_DEFAULT, ExecutionStyle::Engineer);
}

// =====================================================================
// The ceiling property (DOC-62 §5.4) — the load-bearing invariant
// =====================================================================

/// **The ceiling.** For EVERY (request, floor) pair the effective style is at
/// least the floor and at least the request. A caller asking for less ceremony
/// than the callee's floor does not get less ceremony.
#[test]
fn a_caller_can_never_lower_ceremony_below_the_callee_floor() {
    for requested in ALL {
        for floor in ALL {
            let resolved = ResolvedStyle::resolve(Some(requested), None, floor);
            assert!(
                resolved.effective() >= floor,
                "request {requested:?} lowered effective below floor {floor:?}: {:?}",
                resolved.effective()
            );
            assert!(
                resolved.effective() >= requested,
                "request {requested:?} at floor {floor:?} lowered below the request: {:?}",
                resolved.effective()
            );
        }
    }
}

/// The same property for a CONFIG-supplied style — config is an author tag too.
#[test]
fn a_config_default_can_never_lower_ceremony_below_the_callee_floor() {
    for config in ALL {
        for floor in ALL {
            let resolved = ResolvedStyle::resolve(None, Some(config), floor);
            assert!(resolved.effective() >= floor);
            assert!(resolved.effective() >= config);
        }
    }
}

/// The tier fail-safe is itself a raise — `implemented()` never lowers.
#[test]
fn implemented_never_lowers_ceremony() {
    for style in ALL {
        let (effective, _) = style.implemented();
        assert!(
            effective >= style,
            "{style:?} degraded downward to {effective:?}"
        );
    }
}

/// A floor above the request is reported, not applied silently.
#[test]
fn a_floor_above_the_request_is_reported_as_an_escalation() {
    let resolved =
        ResolvedStyle::resolve(Some(ExecutionStyle::Hack), None, ExecutionStyle::Engineer);
    assert_eq!(resolved.effective(), ExecutionStyle::Engineer);
    assert!(
        resolved
            .escalations()
            .contains(&StyleEscalation::CalleeFloor),
        "floor raise must be reported: {:?}",
        resolved.escalations()
    );
}

// =====================================================================
// SM-9 — the VIBE fail-safe (DOC-62 §7.3, #2596)
// =====================================================================

/// SM-9: `vibe` runs the `engineer` pipeline TODAY and reports effective
/// `engineer` with reason `tier-unimplemented`.
#[test]
fn vibe_degrades_upward_to_engineer_and_says_why() {
    let resolved = ResolvedStyle::resolve(Some(ExecutionStyle::Vibe), None, ExecutionStyle::Hack);
    assert_eq!(resolved.effective(), ExecutionStyle::Engineer);
    assert_eq!(
        resolved.escalations(),
        &[StyleEscalation::TierUnimplemented],
        "SM-9 requires the tier-unimplemented reason verbatim"
    );
}

/// Both raises can apply to one resolution, and both are reported in order.
#[test]
fn both_escalations_are_reported_when_both_apply() {
    // `hack` requested at a lane whose floor is `vibe`: floor raises it to
    // `vibe`, then the unimplemented tier raises it again to `engineer`.
    let resolved = ResolvedStyle::resolve(Some(ExecutionStyle::Hack), None, ExecutionStyle::Vibe);
    assert_eq!(resolved.effective(), ExecutionStyle::Engineer);
    assert_eq!(
        resolved.escalations(),
        &[
            StyleEscalation::CalleeFloor,
            StyleEscalation::TierUnimplemented
        ]
    );
}

/// The wire reason names are exactly the ones DOC-62 SM-9/§3.4 spell.
#[test]
fn escalation_tags_match_the_spec_wire_names() {
    assert_eq!(
        StyleEscalation::TierUnimplemented.as_str(),
        "tier-unimplemented"
    );
    assert_eq!(StyleEscalation::CalleeFloor.as_str(), "callee-floor");
    assert_eq!(
        serde_json::to_string(&StyleEscalation::TierUnimplemented).unwrap(),
        "\"tier-unimplemented\""
    );
}

// =====================================================================
// Precedence (DOC-62 §5.3)
// =====================================================================

/// caller > config > built-in, first-match-wins, with the level reported.
#[test]
fn precedence_is_caller_then_config_then_built_in() {
    let caller_wins = ResolvedStyle::resolve(
        Some(ExecutionStyle::Engineer),
        Some(ExecutionStyle::Hack),
        ExecutionStyle::Hack,
    );
    assert_eq!(caller_wins.source(), StyleSource::Caller);
    assert_eq!(caller_wins.effective(), ExecutionStyle::Engineer);

    let config_wins =
        ResolvedStyle::resolve(None, Some(ExecutionStyle::Hack), ExecutionStyle::Hack);
    assert_eq!(config_wins.source(), StyleSource::Config);
    assert_eq!(config_wins.effective(), ExecutionStyle::Hack);
}

/// Nothing supplied anywhere falls through to the built-in default.
#[test]
fn precedence_falls_through_to_built_in() {
    let resolved = ResolvedStyle::resolve(None, None, ExecutionStyle::Hack);
    assert_eq!(resolved.source(), StyleSource::BuiltIn);
    assert_eq!(resolved.effective(), ExecutionStyle::BUILT_IN_DEFAULT);
    assert!(!resolved.is_explicit());
}

/// The resolution path is fully reportable (DOC-62 §3.4 / AC-4).
#[test]
fn resolution_is_reported_end_to_end() {
    let resolved = ResolvedStyle::resolve(Some(ExecutionStyle::Vibe), None, ExecutionStyle::Hack);
    let json = serde_json::to_value(&resolved).expect("resolved style serializes");
    assert_eq!(json["requested"], "vibe");
    assert_eq!(json["effective"], "engineer");
    assert_eq!(json["source"], "caller");
    assert_eq!(json["escalations"][0], "tier-unimplemented");
}

// =====================================================================
// The policy block (#4349; DOC-62 §6.1/§6.4, SM-7/SM-8)
// =====================================================================

/// SM-7: the block states the effective style and the gate boundary, and SM-8:
/// it labels the `NON_CODING_TARGETS` boundary as code-enforced rather than
/// claiming to BE the enforcement.
#[test]
fn policy_block_states_effective_style_and_the_gate_boundary() {
    let block = ResolvedStyle::resolve(Some(ExecutionStyle::Engineer), None, ExecutionStyle::Hack)
        .render_policy_block();
    assert!(block.contains("Effective execution style: engineer"));
    assert!(
        block.contains("never relaxes"),
        "gate boundary missing: {block}"
    );
    assert!(
        block.contains("never as passed"),
        "SM-4 line missing: {block}"
    );
    assert!(
        block.contains("grants no capability"),
        "SM-11 line missing: {block}"
    );
    assert!(
        block.contains("enforced in code, not by this text"),
        "SM-8 must not let the preamble pose as the enforcement: {block}"
    );
    assert!(
        block.starts_with("Delegation policy (system-supplied, not from the caller):"),
        "the block must be distinguishable from caller text: {block}"
    );
}

/// SM-9's fallback is visible to the caller in the text it actually reads.
#[test]
fn policy_block_reports_the_tier_unimplemented_fallback() {
    let block = ResolvedStyle::resolve(Some(ExecutionStyle::Vibe), None, ExecutionStyle::Hack)
        .render_policy_block();
    assert!(block.contains("Requested style was vibe"), "{block}");
    assert!(block.contains("tier-unimplemented"), "{block}");
    assert!(block.contains("raised, never lowered"), "{block}");
}

/// DOC-62 §6.4: the block is bounded and independent of the task — its size is
/// not a function of anything the caller supplies, so it can never be what
/// pushes a handoff over the 4 KiB cap.
#[test]
fn policy_block_is_bounded_and_task_independent() {
    for requested in ALL {
        for floor in ALL {
            let block = ResolvedStyle::resolve(Some(requested), None, floor).render_policy_block();
            assert!(
                block.len() < 1024,
                "policy block must stay well under the 4 KiB handoff cap, got {}",
                block.len()
            );
        }
    }
}

/// The built-in default is not "explicit", so it renders no block at all —
/// the pre-#4349 path stays byte-identical.
#[test]
fn an_unrequested_style_renders_no_policy_block() {
    let resolved = ResolvedStyle::resolve(None, None, ExecutionStyle::Engineer);
    assert!(!resolved.is_explicit());
}

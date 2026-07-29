//! Tests for `tools::subagent_allow` — the ONE floor-narrowing gate both
//! delegation mechanisms share (#4026 bridge floor; ADR-0024 decision 4
//! in-process whitelist).
//!
//! Why: every assertion here pins an owner ruling a future edit could silently
//! relax — OQ-7's fail-closed bridge enforcement, ADR-0024 decision 4's
//! fail-closed absent-whitelist ruling, and the invariant BOTH rest on: config
//! can only ever NARROW a server-owned floor, never widen it. The
//! floor-beats-config tests are duplicated across the two floors on purpose:
//! the whole point of parameterizing the type was that neither mechanism
//! inherits the other's vocabulary, and only a per-floor assertion proves it.
//! What: unit tests over `SubagentAllowSet` and `narrow_to_floor`. No I/O.
//! The dispatch-side behaviour (nothing dispatched on denial) is pinned in
//! `pm_bridge_tests.rs` and `delegate_tests.rs`, where a recording runner can
//! observe it.

use super::*;
use crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS;
use crate::tools::cross_product::NON_CODING_TARGETS;

// =====================================================================
// Fail-closed resolution (#4026 OQ-7; ADR-0024 decision 4 sub-answer (a))
// =====================================================================

/// The DEFAULT allow-set grants nothing: an agent with no config list cannot
/// reach ANY specialist, not even one on the floor. Pins "no silent capability
/// grant" for the cross-product mechanism.
#[test]
fn empty_default_allow_set_denies_everything() {
    let set = SubagentAllowSet::empty_over(NON_CODING_TARGETS);
    assert!(set.is_empty());
    for name in NON_CODING_TARGETS {
        assert_eq!(
            set.resolve(name),
            Err(TargetDenied::NotGranted((*name).to_string())),
            "empty allow-set must deny the non-coding target '{name}'"
        );
    }
}

/// ADR-0024 decision 4 sub-answer (a), at the gate: an assistant whose config
/// declares NO in-process whitelist reaches NOTHING — not the floor, not a
/// legacy role scan. The seeded bundled-persona defaults are what keep that
/// from being a silent capability loss on rollout, pinned by
/// `bundled_assistant_personas_seed_the_reachable_subagent_whitelist`.
#[test]
fn empty_over_grants_nothing_on_either_floor() {
    for floor in [NON_CODING_TARGETS, ASSISTANT_REACHABLE_SUBAGENTS] {
        let set = SubagentAllowSet::empty_over(floor);
        assert!(set.is_empty());
        for name in floor {
            assert!(
                set.resolve(name).is_err(),
                "absent config must deny '{name}' (fail-closed)"
            );
        }
    }
}

/// An absent config list (serde `None`) is the empty set.
#[test]
fn from_allowed_none_is_empty() {
    assert!(SubagentAllowSet::over(NON_CODING_TARGETS, None).is_empty());
    assert!(SubagentAllowSet::over(ASSISTANT_REACHABLE_SUBAGENTS, None).is_empty());
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
    let set = SubagentAllowSet::over(NON_CODING_TARGETS, Some(&raw));
    assert_eq!(set.resolve("research").unwrap(), "research");
    assert_eq!(set.resolve("  TiCkEtInG  ").unwrap(), "ticketing");
}

/// A named, granted, non-coding target resolves.
#[test]
fn allow_set_accepts_a_named_non_coding_target() {
    let set = SubagentAllowSet::over(NON_CODING_TARGETS, Some(&["research".to_string()]));
    assert_eq!(set.resolve("research").unwrap(), "research");
    // ...but a floor target the caller did NOT grant is still denied.
    assert_eq!(
        set.resolve("ticketing"),
        Err(TargetDenied::NotGranted("ticketing".to_string()))
    );
}

/// #4027: the ported ticketing agent is reachable once granted.
#[test]
fn allow_set_accepts_ticketing_once_granted() {
    let set = SubagentAllowSet::over(NON_CODING_TARGETS, Some(&["ticketing".to_string()]));
    assert_eq!(set.resolve("ticketing").unwrap(), "ticketing");
    assert!(NON_CODING_TARGETS.contains(&"ticketing"));
}

/// ADR-0024 decision 4, the seeded default: the in-process floor's own two
/// names resolve once a persona declares them.
#[test]
fn delegate_allow_set_accepts_a_seeded_floor_target() {
    let seed: Vec<String> = ASSISTANT_REACHABLE_SUBAGENTS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let set = SubagentAllowSet::over(ASSISTANT_REACHABLE_SUBAGENTS, Some(&seed));
    for name in ASSISTANT_REACHABLE_SUBAGENTS {
        assert_eq!(set.resolve(name).unwrap(), *name);
    }
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
    let set = SubagentAllowSet::over(NON_CODING_TARGETS, Some(&permissive));
    for coding in ["rust-engineer", "engineer", "pm"] {
        assert_eq!(
            set.resolve(coding),
            Err(TargetDenied::NotOnFloor(coding.to_string())),
            "bridge floor must deny coding target '{coding}' despite config"
        );
    }
    // The one legitimate entry in the same list still resolves.
    assert_eq!(set.resolve("research").unwrap(), "research");
}

/// The SAME invariant on the ADR-0024 floor, which is the security-relevant
/// half of decision 4: a hand-edited (or GUI-written) `[subagents]
/// .delegate_allowed` naming a coding agent must not make it reachable. This
/// is the LAST line of defence — `narrow_to_floor` should have rejected the
/// write long before — and it must hold on its own.
#[test]
fn delegate_floor_rejects_an_engineer_even_when_config_allows_it() {
    let permissive = vec![
        "engineer".to_string(),
        "python-engineer".to_string(),
        "qa-agent".to_string(),
        "docs-agent".to_string(),
        "local-ops-agent".to_string(),
        "plan-agent".to_string(),
        "pm".to_string(),
        "ctrl".to_string(),
        "research-agent".to_string(),
    ];
    let set = SubagentAllowSet::over(ASSISTANT_REACHABLE_SUBAGENTS, Some(&permissive));
    for coding in [
        "engineer",
        "python-engineer",
        "qa-agent",
        "docs-agent",
        "local-ops-agent",
        "plan-agent",
        "pm",
        "ctrl",
    ] {
        assert_eq!(
            set.resolve(coding),
            Err(TargetDenied::NotOnFloor(coding.to_string())),
            "the in-process floor must deny '{coding}' despite config"
        );
    }
    assert_eq!(set.resolve("research-agent").unwrap(), "research-agent");
}

/// The two floors are independent vocabularies — the whole reason the type is
/// floor-parameterized rather than sharing one constant. A cross-product name
/// is not an in-process agent name and vice versa.
#[test]
fn the_two_floors_share_no_name() {
    for name in ASSISTANT_REACHABLE_SUBAGENTS {
        assert!(
            !NON_CODING_TARGETS.contains(name),
            "'{name}' appears on both floors; the two mechanisms' target \
             vocabularies must stay disjoint (see agent_subagents' payload)"
        );
    }
}

/// A blank/whitespace name is rejected before any lookup.
#[test]
fn blank_target_is_rejected() {
    let set = SubagentAllowSet::over(NON_CODING_TARGETS, Some(&["research".to_string()]));
    assert_eq!(set.resolve("   "), Err(TargetDenied::Blank));
    assert_eq!(set.resolve(""), Err(TargetDenied::Blank));
}

/// Both denial reasons render the SAME generic message so a caller cannot
/// probe the floor list by diffing error text.
#[test]
fn denial_messages_do_not_distinguish_floor_from_grant() {
    let floor = TargetDenied::NotOnFloor("engineer".to_string()).to_string();
    let grant = TargetDenied::NotGranted("engineer".to_string()).to_string();
    assert_eq!(floor, grant);
    assert!(floor.contains("not available"), "unexpected text: {floor}");
}

/// The floor is reported verbatim for the config panes, with no second copy of
/// the constant on the read path.
#[test]
fn floor_is_reported_verbatim() {
    assert_eq!(
        SubagentAllowSet::empty_over(ASSISTANT_REACHABLE_SUBAGENTS).floor(),
        ASSISTANT_REACHABLE_SUBAGENTS
    );
    assert_eq!(
        SubagentAllowSet::empty_over(NON_CODING_TARGETS).floor(),
        NON_CODING_TARGETS
    );
}

// =====================================================================
// The WRITE-path floor (ADR-0024 decision 4 sub-answer (b))
// =====================================================================

/// A write that NARROWS is accepted, and comes back normalized.
#[test]
fn narrow_to_floor_accepts_a_subset() {
    let requested = vec!["research-agent".to_string()];
    assert_eq!(
        narrow_to_floor(ASSISTANT_REACHABLE_SUBAGENTS, &requested),
        Ok(vec!["research-agent".to_string()])
    );
    // The empty list is a legitimate narrowing: "reach nothing".
    assert_eq!(
        narrow_to_floor(ASSISTANT_REACHABLE_SUBAGENTS, &[]),
        Ok(Vec::new())
    );
}

/// THE security test for decision 4 sub-answer (b): a write may not WIDEN past
/// the floor. Deliberately shaped like the escalation it prevents — a GUI (or
/// any API client) PATCHing a coding/orchestrator agent into an assistant's
/// reachable set. Every offender is reported; nothing is silently accepted.
#[test]
fn narrow_to_floor_rejects_a_widening() {
    let requested = vec![
        "research-agent".to_string(),
        "engineer".to_string(),
        "pm".to_string(),
    ];
    let err = narrow_to_floor(ASSISTANT_REACHABLE_SUBAGENTS, &requested)
        .expect_err("a widening write must be refused, not partially applied");
    assert_eq!(err, vec!["engineer".to_string(), "pm".to_string()]);
}

/// Normalization matches `SubagentAllowSet::over` so a write and the gate that
/// later reads it agree on what a name IS.
#[test]
fn narrow_to_floor_normalizes_and_dedups() {
    let requested = vec![
        "  Research-Agent ".to_string(),
        "research-agent".to_string(),
        "TICKETING-AGENT".to_string(),
    ];
    assert_eq!(
        narrow_to_floor(ASSISTANT_REACHABLE_SUBAGENTS, &requested),
        Ok(vec![
            "research-agent".to_string(),
            "ticketing-agent".to_string()
        ])
    );
}

/// A blank entry is an offender, not a silently-dropped one — a write is an
/// explicit act and a malformed list should be reported back.
#[test]
fn narrow_to_floor_rejects_blank() {
    let requested = vec!["research-agent".to_string(), "   ".to_string()];
    let err = narrow_to_floor(ASSISTANT_REACHABLE_SUBAGENTS, &requested)
        .expect_err("a blank entry must be reported");
    assert_eq!(err, vec![String::new()]);
}

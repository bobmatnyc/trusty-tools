//! Tests for the bundled-fallback PM instruction package (#4183).
//!
//! The load-bearing test here is
//! [`composed_package_is_byte_identical_to_the_legacy_bundled_fallback`]: the
//! re-sourced composition must reproduce today's delivered prompt exactly, not
//! approximately. Everything else exists to localise a failure of that test —
//! a cut marker that stopped matching, an asset that no longer reassembles, a
//! join that changed.

use super::*;
use crate::core::instruction_overrides::{assemble_sections, delegation_with_roster};
use crate::core::instruction_package::ValidationError;

/// A fixed, deterministic roster — the composition-time input the byte-equality
/// gate holds constant.
///
/// Why: the real roster is scanned from disk and varies per machine, which
/// would make the gate environment-dependent. Nothing about the comparison
/// depends on the roster's *content*, only that both sides receive the same
/// bytes; a literal makes the test hermetic.
const FIXED_ROSTER: &str = "## Delegation Authority\n\n\
     ### ticketing\n\nHandles ticketing work. Model: sonnet.\n\n\
     ### rust-engineer\n\nHandles Rust work. Model: sonnet.";

/// A fixed stack-profile block, standing in for `stack_profile_section`.
const FIXED_STACK: &str =
    "## Project Stack Profile\n\nDetected stack: Rust. Route implementation to `rust-engineer`.";

/// A fixed `.trusty-mpm/INSTRUCTIONS.md` addendum.
const FIXED_ADDENDUM: &str = "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK";

/// Today's legacy assembly for the bundled-fallback configuration.
///
/// Calls the same production [`assemble_sections`] / [`delegation_with_roster`]
/// pair that `resolve_pm_prompt` still uses for configurations 2 and 3, so the
/// oracle cannot rot into a private reimplementation of what it is checking.
fn legacy_bundled_fallback(stack: &str, roster: &str, addendum: Option<&str>) -> String {
    assemble_sections(
        stack.to_string(),
        None,
        WORKFLOW.trim().to_string(),
        delegation_with_roster(Some(roster)),
        addendum.map(str::to_string),
    )
}

/// Assert two prompts are byte-identical, reporting the first divergence.
///
/// Why: a bare `assert_eq!` on two ~40 KB strings prints both in full and tells
/// the reader nothing about where they parted company. This reports the byte
/// offset plus a window of context on each side, which is what a reviewer needs
/// to see which join or cut moved.
fn assert_byte_identical(expected: &str, actual: &str) {
    if expected == actual {
        return;
    }

    let (e, a) = (expected.as_bytes(), actual.as_bytes());
    let at = e
        .iter()
        .zip(a.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| e.len().min(a.len()));

    /// Bytes of context shown either side of the divergence.
    const WINDOW: usize = 120;
    let from = at.saturating_sub(WINDOW);
    let window =
        |s: &[u8]| String::from_utf8_lossy(&s[from..s.len().min(at + WINDOW)]).replace('\n', "\\n");

    panic!(
        "composed prompt is NOT byte-identical to the legacy assembly.\n\
         first difference at byte offset {at} (expected {} bytes, actual {} bytes)\n\
         expected [{from}..]: {}\n\
         actual   [{from}..]: {}\n",
        e.len(),
        a.len(),
        window(e),
        window(a),
    );
}

#[test]
fn composed_package_is_byte_identical_to_the_legacy_bundled_fallback() {
    // THE ACCEPTANCE GATE (#4183). Nothing resolves after this point: the agent
    // roster is fixed at session launch and Claude Code cannot change the agent
    // set afterwards, so the composed string IS the delivered prompt. Strict
    // byte-equality is therefore the correct gate, not a reviewed diff — any
    // divergence here is a change to what every PM receives.
    let composed =
        compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None).expect("package composes");
    let legacy = legacy_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None);

    assert_byte_identical(&legacy, &composed);
}

#[test]
fn composed_package_is_byte_identical_with_a_project_addendum() {
    // The additive `.trusty-mpm/INSTRUCTIONS.md` rules are the one project input
    // the bundled-fallback configuration still carries; they ride a
    // `ProjectAddendum` generator block and must land in the same position with
    // the same separator as the legacy assembly.
    let composed = compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, Some(FIXED_ADDENDUM))
        .expect("package composes");
    let legacy = legacy_bundled_fallback(FIXED_STACK, FIXED_ROSTER, Some(FIXED_ADDENDUM));

    assert_byte_identical(&legacy, &composed);
    assert!(composed.contains("ALWAYS_RUN_MAKE_CHECK"));
}

#[test]
fn shipped_assets_build_and_validate() {
    // The package built from the compiled-in assets must satisfy every
    // structural invariant. This is what makes the `tracing::error!` fallback in
    // `resolve_pm_prompt` unreachable rather than routine.
    let package = bundled_fallback_package().expect("shipped assets build a package");
    assert_eq!(package.validate(), Ok(()));
    assert_eq!(package.package_id, PACKAGE_ID);
    assert!(!package.trailing_newline);

    // Every canonical section is declared exactly once, in canonical order.
    let ids: Vec<SectionId> = package.sections.iter().map(|s| s.id).collect();
    assert_eq!(ids, SectionId::CANONICAL.to_vec());

    // Floor sections are `fixed`; the five content sections are `project`,
    // matching the override files `BASE_PM.md` advertises.
    for section in &package.sections {
        let expected = if section.id.is_floor() {
            CustomizationTier::Fixed
        } else {
            CustomizationTier::Project
        };
        assert_eq!(
            section.customization_tier, expected,
            "section {:?} declares the wrong tier",
            section.id
        );
    }
}

#[test]
fn package_round_trips_through_json() {
    // The composed prompt must survive serialization: a package that cannot be
    // written out and read back is not a source format, and the JSON form is
    // what #4183's authoring work will edit.
    let package = bundled_fallback_package().expect("build");
    let json = package.to_json().expect("serialize");
    let parsed = InstructionPackage::from_json(&json).expect("deserialize");
    assert_eq!(parsed, package);

    let inputs = CompositionInputs {
        agent_roster: FIXED_ROSTER.to_string(),
        stack_profile: Some(FIXED_STACK.to_string()),
        project_addendum: None,
    };
    assert_eq!(
        parsed.compose(&inputs).expect("compose round-tripped"),
        package.compose(&inputs).expect("compose original"),
    );
}

#[test]
fn pm_instructions_cuts_reassemble_the_asset() {
    // Cutting an asset must not move a byte: concatenating the pieces with the
    // joins the splitter derived reproduces the trimmed asset exactly. This is
    // the mechanism the byte-equality gate rests on, asserted directly so a
    // failure names the asset instead of a 40 KB diff.
    let blocks = split_asset(
        "PM_INSTRUCTIONS.md",
        PM_INSTRUCTIONS,
        PM_INSTRUCTIONS_CUTS,
        Join::Rule,
    )
    .expect("cuts resolve");

    assert_eq!(blocks.len(), PM_INSTRUCTIONS_CUTS.len());
    assert_eq!(reassemble(&blocks), PM_INSTRUCTIONS.trim());

    // The asset's own `## Identity` heading stays in `Core`. Attributing it to
    // `SectionId::Identity` would open the floor at block 0 and make every later
    // block "overridable after the floor".
    let sections: Vec<SectionId> = blocks.iter().map(|b| b.section).collect();
    assert_eq!(
        sections,
        vec![
            SectionId::Core,
            SectionId::Memory,
            SectionId::Search,
            SectionId::Core
        ]
    );
}

#[test]
fn base_pm_cuts_reassemble_the_asset() {
    // Same guarantee for the floor, whose block stream is a canonical-order
    // inversion (Identity, NonOverridable, FrameworkConventions, NonOverridable)
    // because `## Trusty Tool Priority` sits last in the asset.
    let blocks =
        split_asset("BASE_PM.md", BASE_PM, BASE_PM_CUTS, Join::Rule).expect("cuts resolve");

    assert_eq!(reassemble(&blocks), BASE_PM.trim());
    let sections: Vec<SectionId> = blocks.iter().map(|b| b.section).collect();
    assert_eq!(
        sections,
        vec![
            SectionId::Identity,
            SectionId::NonOverridableRules,
            SectionId::FrameworkGuaranteedConventions,
            SectionId::NonOverridableRules
        ]
    );
    assert!(blocks.iter().all(|b| b.section.is_floor()));
}

#[test]
fn floor_blocks_are_the_contiguous_tail() {
    // The floor's guarantee is that it has the last word. `validate` enforces
    // it, but asserting the shape here localises a regression to block ordering
    // rather than to a validation error message.
    let package = bundled_fallback_package().expect("build");
    let first_floor = package
        .blocks
        .iter()
        .position(|b| b.section.is_floor())
        .expect("the floor is present");
    assert!(
        package.blocks[first_floor..]
            .iter()
            .all(|b| b.section.is_floor()),
        "nothing overridable may follow the framework floor"
    );
    assert_eq!(package.blocks.len() - first_floor, BASE_PM_CUTS.len());
}

#[test]
fn missing_cut_marker_is_an_error() {
    // An asset edited so a cut marker no longer exists must fail loudly. The
    // silent alternative — re-sectioning whatever text happens to be there — is
    // the #4196 shape: composition reports success, the prompt is wrong.
    let err = split_asset(
        "PM_INSTRUCTIONS.md",
        "# Rewritten asset\n\nNo cut markers here at all.",
        PM_INSTRUCTIONS_CUTS,
        Join::Rule,
    )
    .expect_err("a missing marker must not compose");

    assert_eq!(
        err,
        PackageError::MarkerNotFound {
            asset: "PM_INSTRUCTIONS.md",
            marker: "## Context-First Protocol",
        }
    );
}

#[test]
fn roster_is_required_and_never_droppable() {
    // #4196 regression gate at the package level: the computed roster must reach
    // the composed prompt. An empty roster is a hard error, not a quiet drop.
    let package = bundled_fallback_package().expect("build");
    assert_ne!(package.validate(), Err(ValidationError::RosterNotConsumed));

    let roster_block = package
        .blocks
        .iter()
        .find(|b| {
            matches!(
                b.body,
                BlockBody::Generated {
                    generator: Generator::AgentRoster
                }
            )
        })
        .expect("a block consumes the roster");
    assert!(
        !roster_block.optional,
        "the roster block must not be optional"
    );

    let err = compose_bundled_fallback(FIXED_STACK, "   \n\t", None)
        .expect_err("an empty roster must not compose");
    assert!(
        matches!(
            err,
            PackageError::Compose(CompositionError::MissingGeneratedInput {
                generator: Generator::AgentRoster,
                ..
            })
        ),
        "expected a missing-roster composition error, got {err:?}"
    );
}

#[test]
fn composed_prompt_carries_the_live_roster_and_the_precedence_note() {
    // The delivered prompt must contain the doctrine, the note that resolves the
    // doctrine-vs-roster contradiction, and the roster — in that order (#4069).
    let composed =
        compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None).expect("package composes");

    let doctrine = composed
        .find("# Agent Delegation Routing")
        .expect("doctrine");
    let note = composed.find("trust the roster").expect("note");
    let roster = composed.find("### ticketing").expect("roster");
    let floor = composed.find("# BASE_PM Framework Floor").expect("floor");
    assert!(doctrine < note && note < roster && roster < floor);
}

#[test]
fn empty_stack_profile_is_dropped_without_a_dangling_rule() {
    // The legacy `join_sections` filters empty sections so a missing one never
    // leaves a bare `---`. The stack-profile block is `optional` precisely to
    // reproduce that, and the two must still agree byte-for-byte.
    let composed = compose_bundled_fallback("", FIXED_ROSTER, None).expect("package composes");
    let legacy = legacy_bundled_fallback("", FIXED_ROSTER, None);

    assert_byte_identical(&legacy, &composed);
    assert!(!composed.contains("\n\n---\n\n---\n\n"));
}

/// Concatenate blocks with their declared joins, as `compose` would.
///
/// Only valid for text-only block slices — generated blocks have no body until
/// composition time.
fn reassemble(blocks: &[InstructionBlock]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            out.push_str(block.join_before.as_str());
        }
        match &block.body {
            BlockBody::Text { text } => out.push_str(text),
            BlockBody::Generated { generator } => {
                panic!("reassemble() saw a generated block for {generator:?}")
            }
        }
    }
    out
}

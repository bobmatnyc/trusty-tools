//! Tests for the bundled-fallback PM instruction package (#4183).
//!
//! Two gates live here, and they cover different things.
//!
//! * [`composed_package_is_byte_identical_to_the_legacy_bundled_fallback`] holds
//!   the TWO COMPOSERS together. It is not a freeze on content — #4183's
//!   sourcing swap changes the delivered prompt deliberately — but on the
//!   mechanism: whatever the sections say, the packaged path and the legacy
//!   override assembly must say it identically, or a project with a
//!   `WORKFLOW.md` override starts receiving different instructions from a
//!   project without one.
//! * the tier tests hold the SCHEMA to the CONTENT: a section declared
//!   `project` must be one the floor actually advertises an override file for,
//!   and a `fixed` section must refuse every override tier.
//!
//! The content itself is gated by the committed snapshots in
//! `pm_prompt_golden_tests.rs`, which is what replaces #4249's
//! byte-equality-against-the-old-prompt gate.

use super::*;
use crate::core::instruction_overrides::{
    FILE_AGENT_DELEGATION, FILE_INSTRUCTIONS, FILE_MEMORY, FILE_WORKFLOW, OVERRIDE_DIR_NAME,
    PromptSource, assemble_sections, delegation_with_roster, resolve_pm_prompt,
    resolve_pm_prompt_with_roster, resolve_pm_prompt_with_source,
};
use crate::core::instruction_package::{OverrideTier, ValidationError};
use crate::core::stack_profile::stack_profile_section;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

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
fn shipped_sections_build_and_validate() {
    // The package built from the compiled-in assets must satisfy every
    // structural invariant. This is what makes the `tracing::error!` fallback in
    // `resolve_pm_prompt` unreachable rather than routine.
    let package = bundled_fallback_package();
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
    let package = bundled_fallback_package();
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
fn floor_blocks_are_the_contiguous_tail() {
    // The floor's guarantee is that it has the last word. `validate` enforces
    // it, but asserting the shape here localises a regression to block ordering
    // rather than to a validation error message.
    let package = bundled_fallback_package();
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
    assert_eq!(
        package.blocks.len() - first_floor,
        3,
        "the floor is exactly its three authored sections"
    );
}

// ---------------------------------------------------------------------------
// The gate at the production entry point (#4183 review, HIGH-1).
//
// Everything above compares `compose_bundled_fallback` against
// `assemble_sections` given hand-supplied arguments. That proves the two
// composers agree, but NOT that `resolve_pm_prompt` hands the package the same
// inputs it would have handed the legacy assembly — and the wiring is where a
// delivered-prompt regression actually lives. The review demonstrated two live
// mutations at that seam surviving a fully green suite:
//
//   * `addendum.as_deref()` → `None`, silently dropping every project's
//     `.trusty-mpm/INSTRUCTIONS.md` from the delivered prompt;
//   * deleting the `workflow_override.is_none() && memory_override.is_none()`
//     filter, silently discarding a `WORKFLOW.md` / `MEMORY.md` override.
//
// Both survived because the composed branch requires a roster, and no
// `resolve_pm_prompt` test deployed an agent — so on a clean CI runner every one
// of them took the LEGACY branch. The tests below deploy a project-tier agent
// first, which makes `deployed_roster_section` return `Some` regardless of the
// machine's `~/.claude/agents`, so the composed branch is a property of the test
// rather than of the developer's `$HOME`. No `$HOME` manipulation is involved,
// deliberately — the project tier is sufficient, and scoping `$HOME` would
// reintroduce the cross-test `HOME_LOCK` race for no added coverage.
// ---------------------------------------------------------------------------

/// Deploy a composed agent into the PROJECT tier (`<project>/.claude/agents`).
///
/// The project tier is the highest-precedence roster source and the one the
/// daemon managed-spawn path deploys into, so writing here guarantees
/// `deployed_roster_section` returns `Some` without depending on — or
/// forbidding — the machine's `~/.claude/agents`.
fn deploy_agent(project: &Path, name: &str) {
    let dir = project.join(".claude").join("agents");
    fs::create_dir_all(&dir).expect("create .claude/agents");
    fs::write(
        dir.join(format!("{name}.md")),
        format!(
            "---\nname: {name}\nrole: {name}\ndescription: Handles {name} work.\n\
             model: sonnet\n---\n\n# {name}\n"
        ),
    )
    .expect("write agent");
}

/// Write `<project>/.trusty-mpm/<name>`.
fn write_override(project: &Path, name: &str, content: &str) {
    let dir = project.join(OVERRIDE_DIR_NAME);
    fs::create_dir_all(&dir).expect("create .trusty-mpm");
    fs::write(dir.join(name), content).expect("write override");
}

/// A project with one deployed agent, so the composed branch is guaranteed.
fn project_with_roster() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    deploy_agent(tmp.path(), "ticketing");
    tmp
}

#[test]
fn resolve_pm_prompt_takes_the_package_path_when_a_roster_is_deployed() {
    // Without this, every assertion below would still pass if the package branch
    // were deleted outright — the two composers are byte-identical, so the
    // delivered string cannot reveal which one ran. This is the positive
    // statement that the composed path is the one under test.
    let tmp = project_with_roster();
    let (_, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(
        source,
        PromptSource::Package,
        "a deployed roster with no section override must compose via InstructionPackage"
    );
}

/// Run the entry-point gate for one project, with the roster pinned.
///
/// Why: `deployed_roster_section` unions three tiers on every call, two of them
/// machine-global and rewritten by live `tm` sessions, so scanning once per side
/// of a byte comparison is a race — observed red 1 run in 4, with a message
/// indistinguishable from a real prompt regression. Both sides here take
/// [`FIXED_ROSTER`], so the only thing that can differ is the wiring, which is
/// what the gate is for. No `$HOME` scoping, hence no `HOME_LOCK` serialisation.
fn assert_entry_point_gate(project: &Path, addendum: Option<&str>) {
    let (composed, source) =
        resolve_pm_prompt_with_roster(project, || Some(FIXED_ROSTER.to_string()));
    assert_eq!(
        source,
        PromptSource::Package,
        "the gate must run against the COMPOSED path, or it proves nothing"
    );

    let legacy = legacy_bundled_fallback(&stack_profile_section(project), FIXED_ROSTER, addendum);
    assert_byte_identical(&legacy, &composed);
}

#[test]
fn resolve_pm_prompt_is_byte_identical_to_the_legacy_assembly() {
    // THE ACCEPTANCE GATE, at the layer that ships the prompt (#4183). What is
    // under test is the WIRING — that `resolve_pm_prompt` hands the package
    // exactly the inputs the legacy assembly would have received.
    let tmp = TempDir::new().expect("tempdir");
    assert_entry_point_gate(tmp.path(), None);
}

#[test]
fn resolve_pm_prompt_is_byte_identical_with_a_project_addendum() {
    // Kills the `addendum.as_deref()` → `None` mutation: the legacy oracle takes
    // the addendum, so dropping it at the call site diverges the two.
    let tmp = TempDir::new().expect("tempdir");
    write_override(
        tmp.path(),
        FILE_INSTRUCTIONS,
        "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
    );

    let (composed, _) =
        resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert!(
        composed.contains("ALWAYS_RUN_MAKE_CHECK"),
        "the project addendum must reach the delivered prompt on the composed path"
    );

    assert_entry_point_gate(tmp.path(), Some("# Project Rules\n\nALWAYS_RUN_MAKE_CHECK"));
}

#[test]
fn gate_is_deterministic_across_repeated_runs() {
    // The gate's own flake guard. The previous revision scanned the ambient agent
    // tiers once per side and compared the results; this asserts the replacement
    // is stable under repetition, and that the composed prompt for a fixed
    // (project, roster) pair is a pure function of its inputs.
    let tmp = TempDir::new().expect("tempdir");
    let first = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string())).0;
    for _ in 0..32 {
        assert_entry_point_gate(tmp.path(), None);
        let again = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string())).0;
        assert_byte_identical(&first, &again);
    }
}

#[test]
fn injected_roster_does_not_change_which_path_runs() {
    // The seam must not become a back door that alters routing: injecting a
    // roster changes only WHICH roster, never whether the composed branch is
    // eligible. With a delegation override the roster is irrelevant and the
    // legacy path must still win.
    let tmp = TempDir::new().expect("tempdir");
    write_override(tmp.path(), FILE_AGENT_DELEGATION, "# Custom Routing\n\nX\n");
    let (_, source) = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert_eq!(source, PromptSource::Legacy);

    // And a `None` roster is not composable, injected or scanned.
    let bare = TempDir::new().expect("tempdir");
    let (_, source) = resolve_pm_prompt_with_roster(bare.path(), || None);
    assert_eq!(source, PromptSource::Legacy);
}

#[test]
fn injected_roster_is_lazy_for_the_override_configurations() {
    // The roster source stays a closure so an `AGENT_DELEGATION.md` override
    // short-circuits before any tier scan — the pre-PR behaviour, which a plain
    // `Option<String>` parameter would have silently regressed into an
    // unconditional scan on every launch.
    use std::cell::Cell;
    let scans = Cell::new(0usize);

    let tmp = TempDir::new().expect("tempdir");
    write_override(tmp.path(), FILE_AGENT_DELEGATION, "# Custom Routing\n\nX\n");
    let _ = resolve_pm_prompt_with_roster(tmp.path(), || {
        scans.set(scans.get() + 1);
        Some(FIXED_ROSTER.to_string())
    });
    assert_eq!(
        scans.get(),
        0,
        "a delegation override must not scan the tiers"
    );

    let plain = TempDir::new().expect("tempdir");
    let _ = resolve_pm_prompt_with_roster(plain.path(), || {
        scans.set(scans.get() + 1);
        Some(FIXED_ROSTER.to_string())
    });
    assert_eq!(scans.get(), 1, "the bundled fallback scans exactly once");
}

#[test]
fn workflow_override_forces_the_legacy_path_even_with_a_roster() {
    // Kills half the "delete the override filter" mutation. With the filter gone
    // the composed branch would run and silently substitute the BUNDLED
    // workflow, discarding the project's override.
    let tmp = project_with_roster();
    write_override(
        tmp.path(),
        FILE_WORKFLOW,
        "# Custom Workflow\n\nTWO_PHASE_ONLY\n",
    );

    let (prompt, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(
        source,
        PromptSource::Legacy,
        "a WORKFLOW.md override is not expressible in the package; it must stay legacy"
    );
    assert!(prompt.contains("TWO_PHASE_ONLY"));
    assert!(
        !prompt.contains("# PM Workflow Configuration"),
        "the bundled workflow must not survive a WORKFLOW.md override"
    );
}

#[test]
fn memory_override_forces_the_legacy_path_even_with_a_roster() {
    // The other half. A MEMORY.md override slots a delimited block after
    // PM_INSTRUCTIONS, which the package model has no generator for.
    let tmp = project_with_roster();
    write_override(
        tmp.path(),
        FILE_MEMORY,
        "Recall from the `team` palace first.\n",
    );

    let (prompt, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(source, PromptSource::Legacy);
    assert!(prompt.contains("## Memory Behavior (project override)"));
    assert!(prompt.contains("Recall from the `team` palace first."));
}

#[test]
fn delegation_override_forces_the_legacy_path_even_with_a_roster() {
    // Configuration 2 (#4247): the override replaces the whole section, so the
    // roster is deliberately NOT re-appended and the package — which requires
    // the roster to be consumed — cannot express it.
    let tmp = project_with_roster();
    write_override(
        tmp.path(),
        FILE_AGENT_DELEGATION,
        "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER\n",
    );

    let (prompt, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(source, PromptSource::Legacy);
    assert!(prompt.contains("ROUTE_ALL_TO_ENGINEER"));
    assert!(
        !prompt.contains("### ticketing"),
        "an override replaces the section outright — the roster is not re-appended"
    );
}

#[test]
fn deployed_prompt_override_forces_the_legacy_path_even_with_a_roster() {
    // Configuration 3 (#4247): a full body replacement contributes no delegation
    // section at all.
    let tmp = project_with_roster();
    write_override(
        tmp.path(),
        crate::core::instruction_overrides::FILE_PM_DEPLOYED,
        "# Wholly Custom PM\n\nDO_EXACTLY_THIS\n",
    );

    let (prompt, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(source, PromptSource::Legacy);
    assert!(prompt.contains("DO_EXACTLY_THIS"));
    assert!(prompt.contains("# BASE_PM Framework Floor"));
}

#[test]
fn resolve_pm_prompt_wrapper_matches_the_source_reporting_form() {
    // The public entry point must return exactly what the seam returns — the
    // logging wrapper may not alter the prompt.
    //
    // Uses a delegation override deliberately: both calls below scan the ambient
    // agent tiers independently, and on this configuration the override replaces
    // the whole delegation section, so no roster byte reaches either output and
    // the comparison cannot race a concurrent agent redeploy.
    let tmp = TempDir::new().expect("tempdir");
    write_override(
        tmp.path(),
        FILE_AGENT_DELEGATION,
        "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER\n",
    );

    let (with_source, source) = resolve_pm_prompt_with_source(tmp.path());
    assert_eq!(source, PromptSource::Legacy);
    assert_byte_identical(&resolve_pm_prompt(tmp.path()), &with_source);
}

#[test]
fn roster_is_required_and_never_droppable() {
    // #4196 regression gate at the package level: the computed roster must reach
    // the composed prompt. An empty roster is a hard error, not a quiet drop.
    let package = bundled_fallback_package();
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
            CompositionError::MissingGeneratedInput {
                generator: Generator::AgentRoster,
                ..
            }
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

// ---------------------------------------------------------------------------
// Section sourcing (#4183 Track A).
//
// The blocks must come from the per-section files, not from offsets into a
// monolith. With the cut machinery gone there is no marker to relocate, so the
// property to assert is simply that each block IS its file — which also makes
// "a section is an editable unit" checkable rather than asserted in prose.
// ---------------------------------------------------------------------------

#[test]
fn every_authored_block_is_exactly_its_section_source() {
    // Kills the regression this PR exists to prevent from coming back: a block
    // whose text is assembled, excerpted or re-derived rather than being the
    // file. Any such block fails here even though the composed bytes may be fine.
    let package = bundled_fallback_package();
    let expected: Vec<(SectionId, &str)> = vec![
        (SectionId::Core, SECTION_CORE),
        (SectionId::Memory, SECTION_MEMORY),
        (SectionId::Search, SECTION_SEARCH),
        (SectionId::Workflow, WORKFLOW),
        (SectionId::AgentDelegation, AGENT_DELEGATION),
        (SectionId::Identity, SECTION_IDENTITY),
        (
            SectionId::NonOverridableRules,
            SECTION_NON_OVERRIDABLE_RULES,
        ),
        (
            SectionId::FrameworkGuaranteedConventions,
            SECTION_FRAMEWORK_CONVENTIONS,
        ),
    ];

    for (section, source) in expected {
        let found = package.blocks.iter().any(|b| {
            b.section == section
                && matches!(&b.body, BlockBody::Text { text } if text == source.trim())
        });
        assert!(
            found,
            "section {section:?} must contribute its source file verbatim"
        );
    }
}

#[test]
fn memory_and_search_are_independently_overridable() {
    // The constraint the runtime-cut model recorded and could not fix (#4247):
    // Memory and Search declared tier `project` while being two halves of one
    // numbered list, so replacing Memory alone left Search opening on a bare
    // `2.` and orphaned a sentence that spoke for both. Authored sources are
    // what make the declared tier honest, so assert the shape that proves it —
    // each block stands alone, with its own heading and no dangling list index.
    let package = bundled_fallback_package();
    for section in [SectionId::Memory, SectionId::Search] {
        let text = package
            .blocks
            .iter()
            .find_map(|b| match (&b.body, b.section == section) {
                (BlockBody::Text { text }, true) => Some(text.as_str()),
                _ => None,
            })
            .expect("section contributes a text block");
        assert!(
            text.starts_with("## "),
            "{section:?} must open with its own heading, got {:?}",
            &text[..text.len().min(40)]
        );
        assert!(
            !text.contains("Both tools are"),
            "{section:?} must not speak for the other section"
        );
    }
}

#[test]
fn floor_carries_the_tool_priority_mandate() {
    // The one floor reordering this PR makes: `## Trusty Tool Priority` moved
    // from after the framework conventions to inside the non-overridable rules
    // it belongs to. It must still be IN the floor — that is what makes the
    // mandate non-overridable — so assert membership, not position.
    let package = bundled_fallback_package();
    let rules = package
        .blocks
        .iter()
        .find(|b| b.section == SectionId::NonOverridableRules)
        .expect("the rules section contributes a block");
    assert!(rules.section.is_floor());
    match &rules.body {
        BlockBody::Text { text } => {
            assert!(text.contains("## Trusty Tool Priority (Non-Overridable)"));
            assert!(text.contains("mcp__trusty-search__search"));
        }
        other => panic!("the rules block must be authored text, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tier precedence: fixed > project > user (#4183).
// ---------------------------------------------------------------------------

#[test]
fn floor_sections_refuse_every_override_tier() {
    // INVALID-OVERRIDE BEHAVIOUR. A `fixed` section admits no override from any
    // tier — that is the entire content of the floor guarantee, and the check
    // #4247's resolver will consult. Stated over the SHIPPED package so it is a
    // fact about what we deliver, not about a hand-built fixture.
    let package = bundled_fallback_package();
    for id in SectionId::CANONICAL.into_iter().filter(|id| id.is_floor()) {
        let tier = package.section(id).expect("declared").customization_tier;
        assert_eq!(tier, CustomizationTier::Fixed, "{id:?} must be fixed");
        for from in [OverrideTier::Project, OverrideTier::User] {
            assert!(
                !tier.permits(from),
                "{id:?} is fixed and must refuse a {from:?}-tier override"
            );
        }
    }
}

#[test]
fn content_sections_admit_project_but_not_user_overrides() {
    // The middle rung, which is the one that is easy to get wrong: every
    // advertised override file is PROJECT-scoped, so a `project`-tier section
    // must accept a project override and REJECT a user-tier one. Getting this
    // backwards is how one operator's machine config leaks into a shared repo.
    let package = bundled_fallback_package();
    for id in SectionId::CANONICAL.into_iter().filter(|id| !id.is_floor()) {
        let tier = package.section(id).expect("declared").customization_tier;
        assert_eq!(tier, CustomizationTier::Project, "{id:?} must be project");
        assert!(tier.permits(OverrideTier::Project), "{id:?}");
        assert!(
            !tier.permits(OverrideTier::User),
            "{id:?} must not admit a user-tier override"
        );
    }
}

#[test]
fn every_advertised_override_file_maps_to_an_overridable_section() {
    // Binds the SCHEMA to the CONTENT. The floor advertises a "Customizing PM
    // Behavior" table naming the `.trusty-mpm/` files a project may drop in; if
    // a file listed there mapped to a `fixed` section the framework would be
    // promising an override it must refuse. Reading the advertisement out of the
    // shipped floor text — rather than restating it — is what makes the two
    // unable to drift.
    let package = bundled_fallback_package();
    let floor = match &package
        .blocks
        .iter()
        .find(|b| b.section == SectionId::NonOverridableRules)
        .expect("rules block")
        .body
    {
        BlockBody::Text { text } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };

    for (file, section) in [
        (FILE_WORKFLOW, SectionId::Workflow),
        (FILE_AGENT_DELEGATION, SectionId::AgentDelegation),
        (FILE_MEMORY, SectionId::Memory),
    ] {
        assert!(
            floor.contains(&format!(".trusty-mpm/{file}")),
            "{file} must stay advertised in the customization table"
        );
        assert!(
            package
                .section(section)
                .expect("declared")
                .customization_tier
                .permits(OverrideTier::Project),
            "{file} is advertised, so {section:?} must admit a project override"
        );
    }
}

#[test]
fn the_delivered_prompt_teaches_sprint_then_harden() {
    // Owner content requirement on #4183: the composed instructions must teach a
    // sprint-then-harden workflow, not a blended one — including the causal
    // claim (slow release CAUSES WIP), the hard line that survives going fast,
    // and the close-and-fold rule. Asserted on the COMPOSED prompt, because a
    // doctrine that lives in a file no composer emits teaches nobody.
    let composed =
        compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None).expect("package composes");

    for marker in [
        "## Sprint, then Harden",
        "feature-complete on a local version",
        "no CI iteration loops",
        "no critic round on narrow changes",
        "full suite, critic, release gates",
        "Publish only after that",
        "*causes* too many things in flight",
        "never turn red green by deleting coverage",
        "3+ review rounds is evidence to close and fold",
    ] {
        assert!(
            composed.contains(marker),
            "the delivered prompt must carry the sprint/harden doctrine: {marker:?}"
        );
    }
}

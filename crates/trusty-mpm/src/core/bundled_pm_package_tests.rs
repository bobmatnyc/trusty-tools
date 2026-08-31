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
    PromptSource, assemble_sections, delegation_with_roster, resolve_pm_prompt_with_roster,
    resolve_pm_prompt_with_source,
};
use crate::core::instruction_package::{
    BlockBody, CustomizationTier, Generator, InstructionBlock, OverrideTier, SCHEMA_VERSION,
    ValidationError,
};
use crate::core::instruction_pipeline::{
    AGENT_DELEGATION, SECTION_CORE, SECTION_ENFORCEMENT, SECTION_FRAMEWORK_CONVENTIONS,
    SECTION_IDENTITY, SECTION_MEMORY, SECTION_NON_OVERRIDABLE_RULES, SECTION_SEARCH,
    SECTION_SOURCES, WORKFLOW, section_source, workflow_section,
};
use crate::core::stack_profile::stack_profile_section;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// The shipped manifest, parsed — the subject of nearly every test here.
///
/// Why: [`bundled_fallback_package`] returns a `Result` since #4318 because the
/// package is now a parsed artifact rather than Rust code. Unwrapping once behind
/// a named helper keeps that plumbing out of every assertion, and
/// `bundled_manifest_parses_and_validates` is the test that gives the unwrap its
/// licence.
fn package_ref() -> &'static InstructionPackage {
    bundled_fallback_package().expect("the bundled manifest parses and validates")
}

/// The authored markdown a block carries, or `None` for a generated block.
fn authored(block: &InstructionBlock) -> Option<&str> {
    block
        .body
        .authored()
        .map(|body| body.expect("source resolves"))
}

/// The no-override composition, which every gate below is written against.
///
/// Why: #4286 gave the production composer an `overrides` parameter. Keeping
/// the zero-override call behind its original name is not cosmetic — it means
/// none of the byte-equality gates in this file were touched by that change, so
/// they still compare exactly what they compared before it.
/// What: [`compose_bundled_fallback_with_overrides`] with no overrides.
fn compose_bundled_fallback(
    stack: &str,
    roster: &str,
    addendum: Option<&str>,
) -> Result<String, CompositionError> {
    compose_bundled_fallback_with_overrides(stack, roster, addendum, &[]).0
}

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
        workflow_section().to_string(),
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
fn bundled_manifest_parses_and_validates() {
    // THE LICENCE FOR EVERY UNWRAP IN THIS FILE, and the reason the degradation in
    // `resolve_pm_prompt` is unreachable rather than routine (#4318): the shipped
    // JSON manifest must parse, validate, and compose. A manifest that fails any of
    // the three is a red CI run, never a shipped prompt.
    let package = InstructionPackage::from_json(PM_PACKAGE_JSON)
        .expect("the shipped manifest is valid schema-v2 JSON");
    assert_eq!(package.validate(), Ok(()));
    assert_eq!(package.schema_version, SCHEMA_VERSION);
    assert_eq!(&package, package_ref());
    assert!(
        compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None).is_ok(),
        "the shipped manifest must compose"
    );
}

#[test]
fn manifest_prose_lives_in_markdown_not_in_the_json() {
    // The point of the v2 `file` body kind. Inlining the sections would turn
    // `core.md` into a single 23 KB JSON line and move the floor text out of the
    // files `scripts/check_instruction_floor.sh` pins, so the manifest must stay
    // small and must reference its bulk prose by path. The inline `text` blocks it
    // DOES carry are short authored rules, not lifted section bodies.
    assert!(
        PM_PACKAGE_JSON.len() < SECTION_CORE.len(),
        "the manifest ({} bytes) must be smaller than core.md ({} bytes) — prose \
         belongs in markdown",
        PM_PACKAGE_JSON.len(),
        SECTION_CORE.len()
    );
    for (path, _) in SECTION_SOURCES {
        assert!(
            PM_PACKAGE_JSON.contains(path),
            "{path} must be referenced by a `file` body"
        );
    }
    for block in &package_ref().blocks {
        if let BlockBody::Text { text } = &block.body {
            assert!(
                text.len() < 1_500,
                "inline manifest text must be a short authored rule, not lifted prose \
                 ({} bytes in {:?})",
                text.len(),
                block.section
            );
        }
    }
}

#[test]
fn every_section_source_resolves() {
    // The `file` body table is the only thing standing between a renamed section
    // and an empty block, so assert both directions: every table key resolves, and
    // a path outside the table does not.
    for (path, body) in SECTION_SOURCES {
        assert_eq!(section_source(path), Some(body), "{path} must resolve");
        assert!(!body.trim().is_empty(), "{path} must not be blank");
    }
    assert_eq!(section_source("sections/does-not-exist.md"), None);
}

#[test]
fn unknown_file_source_is_rejected() {
    // A mistyped path must be a named validation error, never a silently empty
    // block — the #4196 shape (content referenced but never delivered).
    let mut package = package_ref().clone();
    let index = package
        .blocks
        .iter()
        .position(|b| matches!(b.body, BlockBody::File { .. }))
        .expect("the manifest uses file bodies");
    package.blocks[index].body = BlockBody::File {
        path: "sections/typo.md".to_string(),
    };
    let section = package.blocks[index].section;
    assert_eq!(
        package.validate(),
        Err(ValidationError::UnknownFileSource {
            index,
            section,
            path: "sections/typo.md".to_string(),
        })
    );
}

#[test]
fn shipped_sections_build_and_validate() {
    // The package built from the compiled-in assets must satisfy every
    // structural invariant. This is what makes the `tracing::error!` fallback in
    // `resolve_pm_prompt` unreachable rather than routine.
    let package = package_ref();
    assert_eq!(package.validate(), Ok(()));
    assert_eq!(package.package_id, PACKAGE_ID);
    assert!(!package.trailing_newline);

    // Every canonical section is declared exactly once, in canonical order.
    let ids: Vec<SectionId> = package.sections.iter().map(|s| s.id).collect();
    assert_eq!(ids, SectionId::CANONICAL.to_vec());

    // Floor sections are `fixed`; the five content sections are `project`,
    // matching the override files `BASE_PM.md` advertises.
    for section in &package.sections {
        let expected = if section.id == SectionId::Core {
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
    let package = package_ref();
    let json = package.to_json().expect("serialize");
    let parsed = InstructionPackage::from_json(&json).expect("deserialize");
    assert_eq!(&parsed, package);

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
fn the_former_floor_sections_are_still_the_block_tail() {
    // Was `floor_blocks_are_the_contiguous_tail`. #4286 deleted the rule it
    // localised (`validate_floor_is_last`) along with the floor concept, so this
    // is no longer a validated invariant — block order is whatever the manifest
    // declares. It is kept as a SHAPE assertion because the delivered prompt
    // still ends with these four sections, and a reordering that moved them
    // would be a large, silent change to every prompt.
    let package = package_ref();
    let tail = [
        SectionId::Identity,
        SectionId::Enforcement,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ];
    let first = package
        .blocks
        .len()
        .checked_sub(tail.len())
        .expect("the package has at least four blocks");
    assert!(
        package.blocks[first..]
            .iter()
            .all(|b| tail.contains(&b.section)),
        "the four former-floor sections are still the block tail"
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
fn a_retired_instructions_file_cannot_feed_the_project_addendum() {
    // Was `resolve_pm_prompt_is_byte_identical_with_a_project_addendum`, which
    // killed the `addendum.as_deref()` -> `None` mutation. #4286 makes `None`
    // the CORRECT and only value: `.trusty-mpm/INSTRUCTIONS.md` was the sole
    // production source for the optional `project-addendum` block, so the block
    // is now never fed and never emits. The mutation the old test guarded is no
    // longer a mutation — it is the specification — so the test is inverted to
    // pin that instead of deleted.
    let tmp = TempDir::new().expect("tempdir");
    write_override(
        tmp.path(),
        FILE_INSTRUCTIONS,
        "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
    );

    let (composed, _) =
        resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert!(
        !composed.contains("ALWAYS_RUN_MAKE_CHECK"),
        "a retired INSTRUCTIONS.md must not feed the project addendum"
    );

    // And the composed prompt is exactly the no-addendum oracle.
    assert_entry_point_gate(tmp.path(), None);
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

// ---------------------------------------------------------------------------
// PATH SELECTION AFTER #4286
// ---------------------------------------------------------------------------
//
// `workflow_override_forces_the_legacy_path_even_with_a_roster` and its memory,
// delegation and deployed-prompt siblings were deleted here. All four asserted
// that a `.trusty-mpm/` file forces the legacy string assembly even when a
// roster is available. No file can force anything now, so the precondition is
// unconstructible — the same reason the #4399 gates went in
// `claude_md_sections_tests.rs`.
//
// What replaces them is stronger and simpler: the roster, and only the roster,
// selects the path.

#[test]
fn the_roster_alone_selects_the_composer() {
    // With a roster the packaged composer runs; without one the string assembly
    // does. Nothing else participates in the decision.
    let tmp = TempDir::new().expect("tempdir");
    let (_, source) = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert_eq!(source, PromptSource::Package);

    let (_, source) = resolve_pm_prompt_with_roster(tmp.path(), || None);
    assert_eq!(source, PromptSource::Legacy);
}

#[test]
fn no_retired_file_can_divert_the_composer() {
    // The inversion of the four deleted tests, folded into one: each retired
    // file, alone, and then all five together, must leave the packaged path
    // selected. Against the pre-#4286 code every iteration of this loop fails.
    for name in crate::core::instruction_overrides::LEGACY_OVERRIDE_FILES {
        let tmp = TempDir::new().expect("tempdir");
        write_override(tmp.path(), name, "# Retired\n\nX\n");
        let (_, source) =
            resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
        assert_eq!(
            source,
            PromptSource::Package,
            "{name} must not divert the composer"
        );
    }

    let all = TempDir::new().expect("tempdir");
    for name in crate::core::instruction_overrides::LEGACY_OVERRIDE_FILES {
        write_override(all.path(), name, "# Retired\n\nX\n");
    }
    let (_, source) = resolve_pm_prompt_with_roster(all.path(), || Some(FIXED_ROSTER.to_string()));
    assert_eq!(source, PromptSource::Package);
}

#[test]
fn the_roster_source_is_consulted_exactly_once() {
    // The seam stays a closure, and the resolver calls it once per resolution —
    // never zero times (which would drop the roster) and never twice (which is
    // the race `resolve_pm_prompt_with_roster` exists to remove). Before #4286 a
    // delegation override short-circuited to zero scans; that branch is gone, so
    // the count is now unconditionally one.
    use std::cell::Cell;
    let scans = Cell::new(0usize);

    let plain = TempDir::new().expect("tempdir");
    let _ = resolve_pm_prompt_with_roster(plain.path(), || {
        scans.set(scans.get() + 1);
        Some(FIXED_ROSTER.to_string())
    });
    assert_eq!(scans.get(), 1, "the bundled fallback scans exactly once");

    // A retired file present changes nothing about that.
    let retired = TempDir::new().expect("tempdir");
    write_override(
        retired.path(),
        FILE_AGENT_DELEGATION,
        "# Custom Routing\n\nX\n",
    );
    let _ = resolve_pm_prompt_with_roster(retired.path(), || {
        scans.set(scans.get() + 1);
        Some(FIXED_ROSTER.to_string())
    });
    assert_eq!(scans.get(), 2, "a retired file must not suppress the scan");
}

#[test]
fn a_changed_roster_changes_the_resolved_prompt() {
    // #4766. This test used to be `resolve_pm_prompt_wrapper_matches_the_source_
    // reporting_form`: it called `resolve_pm_prompt_with_source` and
    // `resolve_pm_prompt` back-to-back and compared the two prompts. Each call
    // re-scanned the live `~/.claude/agents` tiers through
    // `deployed_roster_section`, so a concurrent change between the two scans
    // made them diverge — ~1 red in 4 full-suite runs on a provisioned
    // workstation, invisible in CI where no ambient tier exists.
    //
    // Renamed rather than merely rewritten, because the seam changes WHAT is
    // under test. `resolve_pm_prompt_with_roster` takes the roster as an
    // argument, so nothing here reaches the logging wrapper any more; a test
    // still named for that wrapper would claim coverage it does not have. The
    // wrapper's byte-equality is now structural instead of asserted:
    // `resolve_pm_prompt` is `resolve_pm_prompt_with_source(dir).0` plus one
    // `info!`, with no transformation between. Asserting it would require a
    // second live tier scan, which is the flake itself — the one thing #4766
    // rules out.
    //
    // What this keeps is the half nothing else covers: the roster is not
    // decoration. Every other byte-equality assertion in this file pins a FIXED
    // roster, and each would hold just as well if the roster never reached the
    // prompt at all.
    let tmp = TempDir::new().expect("tempdir");

    let fixed = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string())).0;
    let divergent_roster = "## Delegation Authority\n\n\
         ### ticketing\n\nHandles ticketing work. Model: opus.";
    assert_ne!(
        FIXED_ROSTER, divergent_roster,
        "the two rosters must differ"
    );
    let divergent =
        resolve_pm_prompt_with_roster(tmp.path(), || Some(divergent_roster.to_string())).0;

    assert_ne!(
        fixed, divergent,
        "a changed roster must change the prompt, else every fixed-roster \
         byte-equality assertion in this file is vacuous"
    );
    assert!(
        divergent.contains("Handles ticketing work."),
        "the supplied roster must reach the composed prompt verbatim"
    );
}

#[test]
fn roster_is_required_and_never_droppable() {
    // #4196 regression gate at the package level: the computed roster must reach
    // the composed prompt. An empty roster is a hard error, not a quiet drop.
    let package = package_ref();
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
    let floor = composed.find("# Framework Instructions").expect("floor");
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
    let package = package_ref();
    let expected: Vec<(SectionId, &str)> = vec![
        (SectionId::Core, SECTION_CORE),
        (SectionId::Memory, SECTION_MEMORY),
        (SectionId::Search, SECTION_SEARCH),
        (SectionId::Workflow, WORKFLOW),
        (SectionId::AgentDelegation, AGENT_DELEGATION),
        (SectionId::Identity, SECTION_IDENTITY),
        (SectionId::Enforcement, SECTION_ENFORCEMENT),
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
        let found = package
            .blocks
            .iter()
            .any(|b| b.section == section && authored(b) == Some(source));
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
    let package = package_ref();
    for section in [SectionId::Memory, SectionId::Search] {
        let text = package
            .blocks
            .iter()
            .filter(|b| b.section == section)
            .find_map(authored)
            .expect("section contributes an authored block")
            .trim();
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
    let package = package_ref();
    let rules = package
        .blocks
        .iter()
        .find(|b| b.section == SectionId::NonOverridableRules)
        .expect("the rules section contributes a block");
    let text = authored(rules).expect("the rules block must be authored, not generated");
    assert!(text.contains("## Trusty Tool Priority (Non-Overridable)"));
    assert!(text.contains("mcp__trusty-search__search"));
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
    let package = package_ref();
    for id in SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id == SectionId::Core)
    {
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
    let package = package_ref();
    for id in SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id != SectionId::Core)
    {
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
    let package = package_ref();
    let floor = package
        .blocks
        .iter()
        .filter(|b| b.section == SectionId::NonOverridableRules)
        .find_map(authored)
        .expect("rules block");

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

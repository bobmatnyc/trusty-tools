//! The bundled-fallback PM prompt, expressed as an [`InstructionPackage`].
//!
//! Why: #4184 landed the sectioned-JSON package type and #4249 made it the
//! composer for the DEFAULT PM prompt — the one every project with no
//! `.trusty-mpm/` override receives. Both steps were mechanical: the package was
//! still built by *cutting the legacy monolithic assets at runtime*, so the
//! eight-section taxonomy described text that was authored as four blobs. This
//! module now builds the package from **per-section sources** — one markdown
//! file per [`SectionId`] under `assets/instructions/sections/` — which is what
//! makes a section an editable, independently overridable unit rather than a
//! documented offset into someone else's file (#4183).
//!
//! What changed with the sourcing swap, and what did not:
//!
//! * GONE — `split_asset`, the `Cut` tables, and the marker-uniqueness guards.
//!   A marker that could silently relocate cannot exist when there is no marker;
//!   the section boundary is now the file boundary. A mis-authored section is
//!   caught by [`InstructionPackage::validate`] instead (an empty file is
//!   `EmptyText`, a missing one fails to compile at `include_str!`).
//! * UNCHANGED — the block stream's order, joins, and the composition mechanism.
//!   [`InstructionPackage::compose`] is untouched by this module.
//!
//! Scope — deliberately ONE of the three configurations
//! [`crate::core::instruction_overrides::resolve_pm_prompt`] can emit:
//!
//! | # | configuration | path |
//! |---|---|---|
//! | 1 | bundled fallback, roster present | **this module** |
//! | 2 | `.trusty-mpm/AGENT_DELEGATION.md` override | legacy, unchanged |
//! | 3 | `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md` | legacy, unchanged |
//!
//! Configurations 2 and 3 remain *inexpressible* in the schema — an
//! `AGENT_DELEGATION.md` override replaces the whole delegation section and so
//! never consumes the computed roster
//! ([`crate::core::instruction_package::ValidationError::RosterNotConsumed`]),
//! and `PM_INSTRUCTIONS_DEPLOYED.md` contributes no delegation section at all
//! (additionally `SectionWithoutBlocks`). #4247 tracks the decision; neither
//! check is weakened here, and both configurations keep resolving exactly as
//! they do today.
//!
//! THE BYTE-EQUALITY CONTRACT, and why it survives a content change. #4249's
//! acceptance gate was byte-equality against the *pre-#4249* prompt, and that
//! gate is necessarily gone here: this PR changes the delivered prompt on
//! purpose. What remains — and is still asserted — is byte-equality between the
//! TWO COMPOSERS: the packaged path and the legacy assembly must agree for the
//! same inputs. That stays true without any pasted duplication, because
//! [`crate::core::instruction_pipeline::pm_instructions`] and
//! [`crate::core::instruction_pipeline::base_pm`] reconstitute the legacy
//! multi-section strings *from these same section files*, joining them with the
//! literal [`Join::Blank`] emits. Editing a section therefore moves both
//! composers together; it cannot move one.
//!
//! What replaces the removed gate for CONTENT is
//! `pm_prompt_golden_tests.rs`: a committed snapshot of the fully composed
//! prompt for both configurations. Every future edit to a section file shows up
//! there as a reviewable prose diff, which is the property #4183's acceptance
//! criteria ask for and which byte-equality against a frozen legacy string could
//! never provide.
//!
//! Test: `bundled_pm_package_tests.rs`.

use crate::core::instruction_overrides::ROSTER_PRECEDENCE_NOTE;
use crate::core::instruction_package::{
    BlockBody, CompositionError, CompositionInputs, CustomizationTier, Generator, InstructionBlock,
    InstructionPackage, InstructionSection, Join, SCHEMA_VERSION, SectionId,
};
use crate::core::instruction_pipeline::{
    AGENT_DELEGATION, SECTION_CORE, SECTION_FRAMEWORK_CONVENTIONS, SECTION_IDENTITY,
    SECTION_MEMORY, SECTION_NON_OVERRIDABLE_RULES, SECTION_SEARCH, WORKFLOW,
};

/// Stable identity of the package this module builds.
pub(crate) const PACKAGE_ID: &str = "trusty-mpm.pm.bundled-fallback";

/// The declared eight-section taxonomy with the tiers this build ships.
///
/// Why: tiers are not decoration — they are the machine-readable statement of
/// which `.trusty-mpm/` override files the floor advertises, and they are what
/// #4247 will enforce. Now that each section has its own source file, the claim
/// "a project may replace this one" is finally true of the artifact as well as
/// of the schema: replacing Memory alone no longer orphans half a numbered list,
/// which was the constraint the runtime-cut model recorded and could not fix.
/// What: [`SectionId::CANONICAL`] paired with its tier and title. The five
/// content sections are `project` because every advertised override file is
/// project-scoped; the three floor sections are `fixed` because
/// `resolve_pm_prompt` appends the floor last under every branch.
/// Test: `every_advertised_override_file_maps_to_an_overridable_section`,
/// `floor_sections_refuse_every_override_tier`,
/// `content_sections_admit_project_but_not_user_overrides`.
fn sections() -> Vec<InstructionSection> {
    let declare = |id: SectionId, title: &str, tier: CustomizationTier| InstructionSection {
        id,
        title: title.to_string(),
        customization_tier: tier,
        description: None,
    };
    vec![
        declare(SectionId::Identity, "Identity", CustomizationTier::Fixed),
        declare(
            SectionId::Core,
            "Core PM Instructions",
            CustomizationTier::Project,
        ),
        declare(
            SectionId::Memory,
            "Memory Protocol",
            CustomizationTier::Project,
        ),
        declare(
            SectionId::Search,
            "Code Search Protocol",
            CustomizationTier::Project,
        ),
        declare(SectionId::Workflow, "Workflow", CustomizationTier::Project),
        declare(
            SectionId::AgentDelegation,
            "Agent Delegation",
            CustomizationTier::Project,
        ),
        declare(
            SectionId::NonOverridableRules,
            "Non-Overridable Rules",
            CustomizationTier::Fixed,
        ),
        declare(
            SectionId::FrameworkGuaranteedConventions,
            "Framework-Guaranteed Conventions",
            CustomizationTier::Fixed,
        ),
    ]
}

/// A block whose content is an authored section source, trimmed.
fn authored(section: SectionId, text: &str, join_before: Join) -> InstructionBlock {
    InstructionBlock {
        section,
        body: BlockBody::Text {
            text: text.trim().to_string(),
        },
        join_before,
        optional: false,
    }
}

/// A block whose content arrives at composition time from a named generator.
fn generated(
    section: SectionId,
    generator: Generator,
    join_before: Join,
    optional: bool,
) -> InstructionBlock {
    InstructionBlock {
        section,
        body: BlockBody::Generated { generator },
        join_before,
        optional,
    }
}

/// Build the bundled-fallback package from the authored section sources.
///
/// Why: this is the executable statement of what the DEFAULT PM prompt is made
/// of. Reading this one function answers "which section does this text belong
/// to, and who may override it?" — and, since #4183's sourcing swap, the answer
/// is a file you can open rather than a byte range.
///
/// What, in emission order: Core, Memory and Search (the former
/// `PM_INSTRUCTIONS.md`, now three files); the derived stack profile; Workflow;
/// the delegation section — bundled doctrine, the roster-precedence note, and
/// the live roster; the optional project addendum; then the three floor
/// sections. Infallible by construction: every source is `include_str!`d, so a
/// missing section is a compile error rather than a runtime one — which is why
/// this returns a package rather than a `Result`.
///
/// Joins are chosen so the composed output is byte-identical to
/// [`crate::core::instruction_overrides::assemble_sections`] for the same
/// inputs: [`Join::Rule`] at every top-level boundary (the literal
/// [`crate::core::instruction_pipeline::SECTION_SEPARATOR`]), [`Join::Blank`]
/// wherever a former monolith held two sections in one string. The two
/// generator-backed project inputs are `optional` because the legacy assembly
/// likewise drops an empty section rather than emitting a dangling `---`. The
/// agent roster is NOT optional — losing it is #4196.
///
/// Test: `shipped_sections_build_and_validate`,
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`.
pub(crate) fn bundled_fallback_package() -> InstructionPackage {
    let blocks = vec![
        // The former `PM_INSTRUCTIONS.md`, now three independently editable
        // sources. `Join::Blank` reproduces the paragraph break that separated
        // them inside the monolith.
        authored(SectionId::Core, SECTION_CORE, Join::Rule),
        authored(SectionId::Memory, SECTION_MEMORY, Join::Blank),
        authored(SectionId::Search, SECTION_SEARCH, Join::Blank),
        // Auto-derived framework context, not a user override (#1971). Optional
        // only so an empty profile is dropped exactly as `join_sections` drops it.
        generated(SectionId::Core, Generator::StackProfile, Join::Rule, true),
        authored(SectionId::Workflow, WORKFLOW, Join::Rule),
        // Delegation: bundled doctrine, the precedence note, then the LIVE
        // roster (#4069/#4196). The roster block is non-optional by contract.
        authored(SectionId::AgentDelegation, AGENT_DELEGATION, Join::Rule),
        authored(
            SectionId::AgentDelegation,
            ROSTER_PRECEDENCE_NOTE,
            Join::Blank,
        ),
        generated(
            SectionId::AgentDelegation,
            Generator::AgentRoster,
            Join::Blank,
            false,
        ),
        // Additive `.trusty-mpm/INSTRUCTIONS.md` rules, when the project has any.
        generated(
            SectionId::Core,
            Generator::ProjectAddendum,
            Join::Rule,
            true,
        ),
        // The non-overridable floor, always last. `Join::Blank` between the three
        // reproduces the paragraph breaks that separated them inside `BASE_PM.md`.
        authored(SectionId::Identity, SECTION_IDENTITY, Join::Rule),
        authored(
            SectionId::NonOverridableRules,
            SECTION_NON_OVERRIDABLE_RULES,
            Join::Blank,
        ),
        authored(
            SectionId::FrameworkGuaranteedConventions,
            SECTION_FRAMEWORK_CONVENTIONS,
            Join::Blank,
        ),
    ];

    InstructionPackage {
        schema_version: SCHEMA_VERSION,
        package_id: PACKAGE_ID.to_string(),
        description: Some(
            "Default PM instruction package: authored section sources plus the live agent roster."
                .to_string(),
        ),
        // `resolve_pm_prompt` emits no trailing newline; the prompt is embedded,
        // not written as a file.
        trailing_newline: false,
        sections: sections(),
        blocks,
    }
}

/// Compose the bundled-fallback PM prompt.
///
/// Why: the single entry point `resolve_pm_prompt` calls for configuration 1.
/// Keeping build and compose together means the caller cannot compose a package
/// it did not build, and cannot forget an input — the roster is a required
/// argument, so the #4196 "computed but never delivered" shape is not
/// expressible here.
///
/// What: builds the package, then composes it with `stack` (the derived stack
/// profile), `roster` (the rendered `## Delegation Authority` block, required)
/// and `addendum` (`.trusty-mpm/INSTRUCTIONS.md`, if any). All three are trimmed
/// by the composer. The result is byte-identical to `instruction_overrides`'
/// legacy assembly for the same inputs — see the module docs.
///
/// Test: `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`,
/// `composed_prompt_carries_the_live_roster_and_the_precedence_note`,
/// `roster_is_required_and_never_droppable`.
pub(crate) fn compose_bundled_fallback(
    stack: &str,
    roster: &str,
    addendum: Option<&str>,
) -> Result<String, CompositionError> {
    bundled_fallback_package().compose(&CompositionInputs {
        agent_roster: roster.to_string(),
        stack_profile: Some(stack.to_string()),
        project_addendum: addendum.map(str::to_string),
    })
}

#[cfg(test)]
#[path = "bundled_pm_package_tests.rs"]
mod tests;

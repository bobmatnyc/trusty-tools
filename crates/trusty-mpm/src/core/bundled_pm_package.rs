//! The bundled-fallback PM prompt, expressed as an [`InstructionPackage`].
//!
//! Why: #4184 landed the sectioned-JSON package type with zero call sites — a
//! schema nothing composes from is a schema nothing validates. This module is
//! the first real producer (#4183, carrying the scope formerly tracked as
//! #4186): it re-sources the DEFAULT PM prompt — the one every project with no
//! `.trusty-mpm/` override receives — through
//! [`InstructionPackage::compose`], so the eight-section taxonomy and its
//! `fixed | project | user` tiers describe the prompt that actually ships
//! rather than a parallel model.
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
//! Configurations 2 and 3 are not merely unported, they are currently
//! *inexpressible*: an `AGENT_DELEGATION.md` override replaces the whole
//! delegation section and so never consumes the computed roster
//! ([`crate::core::instruction_package::ValidationError::RosterNotConsumed`]),
//! and `PM_INSTRUCTIONS_DEPLOYED.md` replaces the entire body with opaque
//! prose that contributes no delegation section at all (additionally
//! `SectionWithoutBlocks`). Both are tracked as follow-up work on epic #4183;
//! neither is weakened here.
//!
//! What: [`compose_bundled_fallback`] builds the package for the compiled-in
//! assets and composes it with the three host-supplied inputs (rendered agent
//! roster, stack profile, project addendum).
//!
//! THE BYTE-EQUALITY CONTRACT. The composed prompt is byte-identical to what
//! the legacy concatenation in `instruction_overrides` produces for the same
//! inputs — not "semantically equivalent". Nothing resolves later: the agent
//! roster is fixed at session launch and Claude Code cannot change the agent
//! set afterwards, so the composed string IS the delivered prompt and any
//! divergence is a delivered-prompt regression. Two mechanisms hold the line:
//!
//! * every block that comes from a bundled asset is *cut out of that asset at
//!   runtime* by [`split_asset`], which derives each block's [`Join`] from the
//!   exact bytes it removed. Reassembling the pieces therefore reproduces the
//!   asset by construction — re-sectioning an asset cannot move a byte, and
//!   editing an asset cannot silently change the composed output;
//! * [`Join::Rule`] is `"\n\n---\n\n"`, the same literal as
//!   [`crate::core::instruction_pipeline::SECTION_SEPARATOR`], so top-level
//!   section boundaries match the legacy `join_sections` join.
//!
//! Test: `bundled_pm_package_tests.rs` — in particular
//! `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`, which
//! diffs the two strings and reports the first differing byte offset.

use crate::core::instruction_overrides::ROSTER_PRECEDENCE_NOTE;
use crate::core::instruction_package::{
    BlockBody, CompositionError, CompositionInputs, CustomizationTier, Generator, InstructionBlock,
    InstructionPackage, InstructionSection, Join, SCHEMA_VERSION, SectionId,
};
use crate::core::instruction_pipeline::{AGENT_DELEGATION, BASE_PM, PM_INSTRUCTIONS, WORKFLOW};

/// Stable identity of the package this module builds.
pub(crate) const PACKAGE_ID: &str = "trusty-mpm.pm.bundled-fallback";

/// A failure building or composing the bundled-fallback package.
///
/// Why: both variants mean the delivered PM prompt would be wrong, so neither
/// may be swallowed. `MarkerNotFound` in particular fires when a bundled asset
/// was edited so that a section boundary this module cuts on no longer exists
/// — a loud error there is what stops a silently re-sectioned prompt.
/// What: a missing cut marker, or a composition failure from the package type.
/// Test: `missing_cut_marker_is_an_error`, and
/// `shipped_assets_build_and_validate` proves neither occurs for what we ship.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PackageError {
    /// A cut marker is absent from the asset it is supposed to split.
    #[error("asset {asset} no longer contains the section cut marker {marker:?}")]
    MarkerNotFound {
        /// The asset file name.
        asset: &'static str,
        /// The marker that could not be located.
        marker: &'static str,
    },
    /// Composing the built package failed.
    #[error(transparent)]
    Compose(#[from] CompositionError),
}

/// One section boundary inside a bundled markdown asset.
///
/// `start_marker` is the literal text that begins the piece; the first cut of
/// an asset uses `""` to mean "start of asset". Markers are searched forward
/// from the previous cut, so they need only be unique in the remaining text.
struct Cut {
    /// Section the resulting block is attributed to.
    section: SectionId,
    /// Literal text that starts this piece (`""` for the first piece).
    start_marker: &'static str,
}

/// How `PM_INSTRUCTIONS.md` divides into Core / Memory / Search blocks.
///
/// Why: the taxonomy needs a Memory and a Search section, and the asset's
/// "Context-First Protocol" block is exactly that guidance — item 1 is the
/// memory protocol, item 2 the code-search protocol. Cutting mid-list looks
/// odd but is precisely what the two-array block model exists for: a section is
/// lifted out of the middle of a legacy document without moving a byte. The
/// asset's own `## Identity` heading stays in `Core`, NOT
/// [`SectionId::Identity`] — that section is the non-overridable floor's
/// identity block, and attributing an overridable block to it would make every
/// later block "overridable after the floor".
/// What: four pieces — Core prologue, Memory, Search, Core remainder.
/// Test: `pm_instructions_cuts_reassemble_the_asset`.
const PM_INSTRUCTIONS_CUTS: &[Cut] = &[
    Cut {
        section: SectionId::Core,
        start_marker: "",
    },
    Cut {
        section: SectionId::Memory,
        start_marker: "## Context-First Protocol",
    },
    Cut {
        section: SectionId::Search,
        start_marker: "2. `search` (`mcp__trusty-search__search`)",
    },
    Cut {
        section: SectionId::Core,
        start_marker: "## Agent Routing",
    },
];

/// How `BASE_PM.md` divides into the three absorbed floor sections.
///
/// Why: the mapping is the one documented on
/// [`crate::core::instruction_package::SectionId`]. The resulting block stream
/// runs `Identity, NonOverridableRules, FrameworkGuaranteedConventions,
/// NonOverridableRules` — a canonical-order inversion, because the asset places
/// the tool-priority block last. That is exactly the shape
/// `validate_floor_is_last` deliberately permits.
/// What: four pieces; `## Customizing PM Behavior` rides with
/// `## Non-Overridable Rules`, and `## Trusty Tool Priority` returns to it.
/// Test: `base_pm_cuts_reassemble_the_asset`, `floor_blocks_are_the_contiguous_tail`.
const BASE_PM_CUTS: &[Cut] = &[
    Cut {
        section: SectionId::Identity,
        start_marker: "",
    },
    Cut {
        section: SectionId::NonOverridableRules,
        start_marker: "## Non-Overridable Rules",
    },
    Cut {
        section: SectionId::FrameworkGuaranteedConventions,
        start_marker: "## Framework-Guaranteed Conventions (Non-Overridable)",
    },
    Cut {
        section: SectionId::NonOverridableRules,
        start_marker: "## Trusty Tool Priority (Non-Overridable)",
    },
];

/// The declared eight-section taxonomy with the tiers this build ships.
///
/// Why: tiers are not decoration — they are the machine-readable statement of
/// which `.trusty-mpm/` override files `BASE_PM.md` advertises. The five content
/// sections are `project` because every advertised override file is project
/// scoped; the three floor sections are `fixed` because
/// `resolve_pm_prompt` appends the floor last under every branch.
/// What: [`SectionId::CANONICAL`] paired with its tier and title.
/// Test: `shipped_assets_build_and_validate`.
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

/// The exact bytes between two adjacent pieces, as a declared [`Join`].
///
/// Why: byte-equality requires every boundary to be declared rather than
/// inferred; naming the two common gaps keeps a composed package readable
/// while `Literal` guarantees fidelity for anything else.
/// What: maps the removed gap bytes onto the closest [`Join`] variant.
/// Test: `pm_instructions_cuts_reassemble_the_asset`.
fn join_for_gap(gap: &str) -> Join {
    match gap {
        "" => Join::None,
        "\n\n" => Join::Blank,
        "\n\n---\n\n" => Join::Rule,
        other => Join::Literal(other.to_string()),
    }
}

/// Cut a bundled asset into per-section blocks without moving a byte.
///
/// Why: the alternative — pasting asset text into a hand-authored package —
/// makes the byte-equality gate a thing someone must re-verify after every
/// asset edit. Cutting at runtime makes reassembly true by construction: each
/// block carries the literal gap that was removed before it, so concatenating
/// the blocks with their joins reproduces `asset.trim()` exactly.
/// What: locates each cut's `start_marker` (searching forward from the previous
/// cut), slices the asset, trims each piece, and records the removed whitespace
/// as the next block's [`Join`]. `lead_join` is the join before the FIRST
/// piece, i.e. the boundary with whatever precedes this asset in the stream.
/// Test: `pm_instructions_cuts_reassemble_the_asset`, `base_pm_cuts_reassemble_the_asset`,
/// `missing_cut_marker_is_an_error`.
fn split_asset(
    asset: &'static str,
    raw: &str,
    cuts: &'static [Cut],
    lead_join: Join,
) -> Result<Vec<InstructionBlock>, PackageError> {
    let text = raw.trim();

    // Resolve each cut to an absolute byte offset, forward-only.
    let mut offsets: Vec<usize> = Vec::with_capacity(cuts.len());
    let mut search_from = 0usize;
    for (index, cut) in cuts.iter().enumerate() {
        if index == 0 {
            offsets.push(0);
            continue;
        }
        let Some(relative) = text
            .get(search_from..)
            .and_then(|t| t.find(cut.start_marker))
        else {
            return Err(PackageError::MarkerNotFound {
                asset,
                marker: cut.start_marker,
            });
        };
        let absolute = search_from + relative;
        offsets.push(absolute);
        search_from = absolute + cut.start_marker.len();
    }

    let mut blocks: Vec<InstructionBlock> = Vec::with_capacity(cuts.len());
    for (index, cut) in cuts.iter().enumerate() {
        let start = offsets.get(index).copied().unwrap_or(0);
        let end = offsets.get(index + 1).copied().unwrap_or(text.len());
        let piece = text.get(start..end).unwrap_or_default();
        let body = piece.trim();
        if body.is_empty() {
            // An empty piece means two cut markers are adjacent — a mis-authored
            // cut table, not valid content. `validate` would reject it anyway;
            // failing here names the asset.
            return Err(PackageError::MarkerNotFound {
                asset,
                marker: cut.start_marker,
            });
        }

        let join_before = if index == 0 {
            lead_join.clone()
        } else {
            let previous = text.get(offsets.get(index - 1).copied().unwrap_or(0)..start);
            let trailing = previous
                .map(|p| p.get(p.trim_end().len()..).unwrap_or_default())
                .unwrap_or_default();
            let leading = piece
                .get(..piece.len() - piece.trim_start().len())
                .unwrap_or_default();
            join_for_gap(&format!("{trailing}{leading}"))
        };

        blocks.push(InstructionBlock {
            section: cut.section,
            body: BlockBody::Text {
                text: body.to_string(),
            },
            join_before,
            optional: false,
        });
    }

    Ok(blocks)
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

/// A block of literal text authored outside the bundled markdown assets.
fn literal(section: SectionId, text: &str, join_before: Join) -> InstructionBlock {
    InstructionBlock {
        section,
        body: BlockBody::Text {
            text: text.trim().to_string(),
        },
        join_before,
        optional: false,
    }
}

/// Build the bundled-fallback package from the compiled-in assets.
///
/// Why: this is the executable statement of what the DEFAULT PM prompt is made
/// of. Reviewing this one function answers "which section does this text belong
/// to, and who may override it?" — a question the previous four-`include_str!`
/// concatenation could not be asked.
///
/// What: `PM_INSTRUCTIONS.md` cut into Core/Memory/Search, the derived stack
/// profile, `WORKFLOW.md`, then the delegation section — bundled doctrine, the
/// roster-precedence note, and the live roster — then the optional project
/// addendum, then `BASE_PM.md` cut into the three floor sections. Block order
/// is emission order and mirrors today's `join_sections` argument order
/// exactly; the two generator-backed project inputs are `optional` because the
/// legacy assembly likewise drops an empty section rather than emitting a
/// dangling `---`. The agent roster is NOT optional — losing it is #4196.
///
/// Test: `shipped_assets_build_and_validate`,
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`.
pub(crate) fn bundled_fallback_package() -> Result<InstructionPackage, PackageError> {
    let mut blocks = split_asset(
        "PM_INSTRUCTIONS.md",
        PM_INSTRUCTIONS,
        PM_INSTRUCTIONS_CUTS,
        Join::Rule,
    )?;

    // Auto-derived framework context, not a user override (#1971). Optional
    // only so an empty profile is dropped exactly as `join_sections` drops it.
    blocks.push(generated(
        SectionId::Core,
        Generator::StackProfile,
        Join::Rule,
        true,
    ));

    blocks.push(literal(SectionId::Workflow, WORKFLOW, Join::Rule));

    // Delegation: bundled doctrine, the precedence note, then the LIVE roster
    // (#4069/#4196). The roster block is non-optional by contract.
    blocks.push(literal(
        SectionId::AgentDelegation,
        AGENT_DELEGATION,
        Join::Rule,
    ));
    blocks.push(literal(
        SectionId::AgentDelegation,
        ROSTER_PRECEDENCE_NOTE,
        Join::Blank,
    ));
    blocks.push(generated(
        SectionId::AgentDelegation,
        Generator::AgentRoster,
        Join::Blank,
        false,
    ));

    // Additive `.trusty-mpm/INSTRUCTIONS.md` rules, when the project has any.
    blocks.push(generated(
        SectionId::Core,
        Generator::ProjectAddendum,
        Join::Rule,
        true,
    ));

    blocks.extend(split_asset(
        "BASE_PM.md",
        BASE_PM,
        BASE_PM_CUTS,
        Join::Rule,
    )?);

    Ok(InstructionPackage {
        schema_version: SCHEMA_VERSION,
        package_id: PACKAGE_ID.to_string(),
        description: Some(
            "Default PM instruction package: bundled assets plus the live agent roster."
                .to_string(),
        ),
        // `resolve_pm_prompt` emits no trailing newline; the prompt is embedded,
        // not written as a file.
        trailing_newline: false,
        sections: sections(),
        blocks,
    })
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
/// and `addendum` (`.trusty-mpm/INSTRUCTIONS.md`, if any). All three are
/// trimmed by the composer. The result is byte-identical to
/// `instruction_overrides`' legacy assembly for the same inputs — see the
/// module docs.
///
/// Test: `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`,
/// `composed_prompt_carries_the_live_roster_and_the_precedence_note`,
/// `roster_is_required_and_never_droppable`.
pub(crate) fn compose_bundled_fallback(
    stack: &str,
    roster: &str,
    addendum: Option<&str>,
) -> Result<String, PackageError> {
    let package = bundled_fallback_package()?;
    let inputs = CompositionInputs {
        agent_roster: roster.to_string(),
        stack_profile: Some(stack.to_string()),
        project_addendum: addendum.map(str::to_string),
    };
    Ok(package.compose(&inputs)?)
}

#[cfg(test)]
#[path = "bundled_pm_package_tests.rs"]
mod tests;

//! The bundled-fallback PM prompt, loaded from the authored JSON manifest.
//!
//! Why: #4184 landed the sectioned-JSON package type, #4249 made it the composer
//! for the DEFAULT PM prompt, and #4183 split the prose into one markdown file per
//! [`SectionId`]. One thing stayed wrong through all three: the package itself was
//! still a **Rust literal**. Sections, tiers, block order, joins and generators
//! were expressed as a `vec![]` in this file, `InstructionPackage::from_json` had
//! zero non-test call sites, and the shipped JSON Schema was a type contract with
//! no instance under it — while other tickets recorded "the composed instructions
//! payload is generated from the instruction package JSON" as settled fact. #4318
//! makes that true: the manifest is now
//! `content/instructions/pm-instruction-package.json` — runtime content since
//! #9012, read from the loaded [`FrameworkContent`] — and this module only
//! parses and composes it.
//!
//! What the swap changed, and what it did not:
//!
//! * GONE — the `sections()` / `authored()` / `generated()` builders. There is no
//!   second place where block order or a join can be edited, so the manifest and
//!   the delivered prompt cannot disagree.
//! * NEW — `file` bodies (schema v2). The manifest names its prose by path and
//!   resolves it through the section files of the same content source
//!   (#9012), so the bulk text keeps living in reviewable markdown and a
//!   renamed section is a validation error rather than an empty block.
//! * NEW — inline `text` bodies now carry authored RULES, not just the
//!   roster-precedence note: the clickable-links, banned-word and
//!   opportunistic-fix rules are authored in the manifest itself (owner order,
//!   2026-07-29: the 1.3 rules belong in JSON, not markdown).
//! * UNCHANGED — [`InstructionPackage::compose`], the block order, and every join.
//!   The mechanism half of #4318 is byte-identical to what #4183 shipped; the
//!   golden diff carries content only.
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
//! NO SPLIT-BRAIN, which is the thing #4318 could most easily have broken. Once a
//! rule may be authored in the manifest, rebuilding the legacy multi-section
//! strings from the raw section files would deliver that rule to
//! package-composed sessions and withhold it from configurations 2 and 3 and from
//! `assemble_system_prompt`. So those callers no longer rebuild from constants —
//! [`crate::core::instruction_pipeline::pm_instructions`],
//! [`crate::core::instruction_pipeline::base_pm`],
//! [`crate::core::instruction_pipeline::workflow_section`] and
//! [`crate::core::instruction_pipeline::delegation_doctrine`] project the SAME
//! manifest through [`InstructionPackage::authored_run`]. Editing the manifest
//! moves every composer together; it cannot move one.
//!
//! FAILURE BEHAVIOUR. `bundled_manifest_parses_and_validates` proves the
//! repository's manifest parses, validates and composes. Content ships apart
//! from the binary (#9012), so a content release can carry a manifest this
//! binary cannot parse — a new section id, a new key. [`parse_bundled_package`]
//! runs inside [`FrameworkContent::load`], which refuses such a source with
//! `AgentContentError::Invalid` naming `tm content update` or a tm upgrade. A
//! loaded [`FrameworkContent`] therefore always holds a valid package, and
//! nothing here degrades to a prompt missing the manifest-authored rules.
//!
//! What guards CONTENT is `pm_prompt_golden_tests.rs`: a committed snapshot of the
//! fully composed prompt for all three configurations. Every edit to a section
//! file or to the manifest shows up there as a reviewable prose diff.
//!
//! Test: `bundled_pm_package_tests.rs`.

use std::collections::BTreeMap;

use crate::core::claude_md_sections::{Rejection, SectionOverride};
use crate::core::framework_content::FrameworkContent;
use crate::core::instruction_package::{
    CompositionError, CompositionInputs, InstructionPackage, SectionId,
};

/// Stable identity of the package this module ships.
///
/// Checked against the loaded manifest, so a content source carrying the wrong
/// JSON file is a named error rather than a differently-shaped prompt.
pub(crate) const PACKAGE_ID: &str = "trusty-mpm.pm.bundled-fallback";

/// The authored manifest's path, relative to the content bundle's
/// `instructions/` (#9012: runtime content, no longer compiled in).
///
/// Test: `bundled_manifest_parses_and_validates`.
pub(crate) const PM_PACKAGE_PATH: &str = "pm-instruction-package.json";

/// Parses and validates the manifest among `instructions` (#9012).
///
/// Why: content ships apart from the binary, so the manifest is checked where
/// the content is loaded; a source this binary cannot compose from is refused
/// there rather than discovered mid-launch.
/// What: [`PM_PACKAGE_PATH`] parsed, its `package_id` checked against
/// [`PACKAGE_ID`], its `file` bodies bound to the `sections/**` entries of
/// `instructions` (keys relative to `instructions/`), then validated. `Err`
/// carries the rendered parse or validation error.
/// Test: `a_source_whose_package_does_not_parse_is_an_error`,
/// `bundled_manifest_parses_and_validates`.
pub(crate) fn parse_bundled_package(
    instructions: &BTreeMap<String, String>,
) -> Result<InstructionPackage, String> {
    let json = instructions
        .get(PM_PACKAGE_PATH)
        .ok_or_else(|| format!("`{PM_PACKAGE_PATH}` is absent"))?;
    let mut package = InstructionPackage::from_json(json).map_err(|err| err.to_string())?;
    if package.package_id != PACKAGE_ID {
        return Err(format!(
            "manifest declares package_id `{}`, expected `{PACKAGE_ID}`",
            package.package_id
        ));
    }
    package.sources = instructions
        .iter()
        .filter(|(path, _)| path.starts_with("sections/"))
        .map(|(path, body)| (path.clone(), body.clone()))
        .collect();
    package.validate().map_err(|err| err.to_string())?;
    Ok(package)
}

/// The bundled-fallback instruction package of `content`.
///
/// Why: the single entry point to "what the DEFAULT PM prompt is made of".
/// What: a copy of the package [`FrameworkContent::load`] parsed and validated
/// (see [`parse_bundled_package`]); total, because a source whose manifest
/// does not parse is never loaded.
/// Test: `bundled_manifest_parses_and_validates`,
/// `shipped_sections_build_and_validate`,
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`.
pub(crate) fn bundled_fallback_package(content: &FrameworkContent) -> InstructionPackage {
    content.pm_package().clone()
}

/// The authored bytes of `sections`, projected out of the bundled manifest.
///
/// Why: the legacy assembly and the roster-free `assemble_system_prompt` need
/// whole multi-section runs as one string and cannot call `compose`. Routing them
/// through the manifest is what keeps a manifest-authored rule from reaching only
/// the packaged path — see the module docs' split-brain note.
/// What: [`InstructionPackage::authored_run`] over the bundled manifest.
/// Test: `pm_instructions_is_the_pm_body_sections`, `base_pm_is_its_three_tail_sections`.
pub(crate) fn authored_run(content: &FrameworkContent, sections: &[SectionId]) -> String {
    content.pm_package().authored_run(sections)
}

/// The pinned blocks of `section`, joined with a paragraph break (#8533).
///
/// Why: the roster-absent string assembly replaces a section without the
/// package's block model, and must still keep the feature statement a pinned
/// block carries.
/// What: the trimmed authored text of every pinned block owned by `section`, in
/// block order; empty when there is none.
/// Test: `a_named_delegation_override_keeps_the_agent_selection_note_on_the_legacy_path`.
pub(crate) fn pinned_run(content: &FrameworkContent, section: SectionId) -> String {
    let package = content.pm_package();
    package
        .blocks
        .iter()
        .filter(|b| b.section == section && b.pinned)
        .filter_map(|b| match b.body.authored(&package.sources) {
            Some(Ok(text)) => Some(text.trim()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Compose the bundled-fallback PM prompt, applying named-section overrides.
///
/// Why: the single entry point `resolve_pm_prompt` calls for configuration 1.
/// Keeping load and compose together means the caller cannot compose a package it
/// did not load, and cannot forget an input — the roster is a required argument,
/// so the #4196 "computed but never delivered" shape is not expressible here.
///
/// `CLAUDE.md` named sections (#4183 / #4286) arrive here rather than through a
/// second string-splice, because this is the only composer that HAS sections and
/// because [`InstructionPackage::with_overrides`] asks the package's own
/// `customization_tier` for permission. The floor refuses a `CLAUDE.md` override
/// for exactly the reason it refuses every other one: it is tier `fixed`.
///
/// What: loads the manifest, applies `overrides`, then composes with `stack` (the
/// derived stack profile), `roster` (the rendered `## Delegation Authority` block,
/// required) and `addendum` (`.trusty-mpm/INSTRUCTIONS.md`, if any). All are
/// trimmed by the composer. Declined overrides come back alongside the result so
/// the caller can report them.
///
/// Test: `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`,
/// `composed_prompt_carries_the_live_roster_and_the_precedence_note`,
/// `roster_is_required_and_never_droppable`, `golden_claude_md_override_prompt`.
pub(crate) fn compose_bundled_fallback_with_overrides(
    content: &FrameworkContent,
    stack: &str,
    roster: &str,
    addendum: Option<&str>,
    overrides: &[SectionOverride],
) -> (Result<String, CompositionError>, Vec<Rejection>) {
    let (package, rejected) = bundled_fallback_package(content).with_overrides(overrides);
    let composed = package.compose(&CompositionInputs {
        agent_roster: roster.to_string(),
        stack_profile: Some(stack.to_string()),
        project_addendum: addendum.map(str::to_string),
    });
    (composed, rejected)
}

#[cfg(test)]
#[path = "bundled_pm_package_tests.rs"]
mod tests;

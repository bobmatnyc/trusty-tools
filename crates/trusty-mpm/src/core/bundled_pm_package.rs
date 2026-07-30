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
//! `assets/instructions/pm-instruction-package.json`, embedded here at compile
//! time, and this module only parses and composes it.
//!
//! What the swap changed, and what it did not:
//!
//! * GONE — the `sections()` / `authored()` / `generated()` builders. There is no
//!   second place where block order or a join can be edited, so the manifest and
//!   the delivered prompt cannot disagree.
//! * NEW — `file` bodies (schema v2). The manifest names its prose by path and
//!   resolves it through the compile-time
//!   [`crate::core::instruction_pipeline::SECTION_SOURCES`] table, so the bulk
//!   text keeps living in reviewable markdown, the build stays hermetic, and a
//!   renamed section is a compile error rather than an empty block.
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
//! strings from the `include_str!` constants would deliver that rule to
//! package-composed sessions and withhold it from configurations 2 and 3 and from
//! `assemble_system_prompt`. So those callers no longer rebuild from constants —
//! [`crate::core::instruction_pipeline::pm_instructions`],
//! [`crate::core::instruction_pipeline::base_pm`],
//! [`crate::core::instruction_pipeline::workflow_section`] and
//! [`crate::core::instruction_pipeline::delegation_doctrine`] project the SAME
//! manifest through [`InstructionPackage::authored_run`]. Editing the manifest
//! moves every composer together; it cannot move one.
//!
//! FAILURE BEHAVIOUR. The manifest is embedded, and
//! `bundled_manifest_parses_and_validates` proves it parses, validates and
//! composes — so a shipped build cannot reach the error path. If it somehow did,
//! [`bundled_fallback_package`] returns `Err` and `resolve_pm_prompt_with_roster`
//! logs it loudly and degrades to the legacy assembly built from the retained
//! `include_str!` constants: a prompt missing only the manifest-authored inline
//! rules, never a truncated one.
//!
//! What guards CONTENT is `pm_prompt_golden_tests.rs`: a committed snapshot of the
//! fully composed prompt for all three configurations. Every edit to a section
//! file or to the manifest shows up there as a reviewable prose diff.
//!
//! Test: `bundled_pm_package_tests.rs`.

use std::sync::LazyLock;

use crate::core::claude_md_sections::{Rejection, SectionOverride};
use crate::core::instruction_package::{
    CompositionError, CompositionInputs, InstructionPackage, SectionId,
};

/// Stable identity of the package this module ships.
///
/// Checked against the loaded manifest, so pointing [`PM_PACKAGE_JSON`] at the
/// wrong JSON file is a named error rather than a differently-shaped prompt.
pub(crate) const PACKAGE_ID: &str = "trusty-mpm.pm.bundled-fallback";

/// The authored manifest, embedded at compile time.
///
/// Why: embedding means the delivered system prompt never depends on what is on
/// disk at launch, and a manifest deleted or renamed in a refactor is a build
/// failure rather than a silently degraded prompt.
/// What: the raw bytes of `assets/instructions/pm-instruction-package.json`.
/// Test: `bundled_manifest_parses_and_validates`.
pub(crate) const PM_PACKAGE_JSON: &str =
    include_str!("../assets/instructions/pm-instruction-package.json");

/// The parsed manifest, or the parse/validation failure, computed once.
///
/// Parsing on every session launch would be wasted work, and re-parsing is the
/// kind of thing that quietly becomes a per-message cost. `LazyLock` also means
/// the failure is computed once and reported identically everywhere.
static BUNDLED: LazyLock<Result<InstructionPackage, String>> = LazyLock::new(|| {
    let package = InstructionPackage::from_json(PM_PACKAGE_JSON).map_err(|err| err.to_string())?;
    if package.package_id != PACKAGE_ID {
        return Err(format!(
            "manifest declares package_id `{}`, expected `{PACKAGE_ID}`",
            package.package_id
        ));
    }
    package.validate().map_err(|err| err.to_string())?;
    Ok(package)
});

/// The bundled-fallback instruction package.
///
/// Why: this is the single entry point to "what the DEFAULT PM prompt is made
/// of". It returns a `Result` (#4318) because the answer now comes from a parsed
/// artifact rather than from Rust code that could not fail — and a parse failure
/// must be reportable rather than papered over with a partial package.
///
/// What: the manifest parsed and structurally validated once, borrowed. `Err`
/// carries the rendered parse or validation error. Unreachable for the shipped
/// manifest; see the module docs for the degradation path.
///
/// Test: `bundled_manifest_parses_and_validates`,
/// `shipped_sections_build_and_validate`,
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`.
pub(crate) fn bundled_fallback_package() -> Result<&'static InstructionPackage, &'static str> {
    BUNDLED.as_ref().map_err(String::as_str)
}

/// The authored bytes of `sections`, projected out of the bundled manifest.
///
/// Why: the legacy assembly and the roster-free `assemble_system_prompt` need
/// whole multi-section runs as one string and cannot call `compose`. Routing them
/// through the manifest is what keeps a manifest-authored rule from reaching only
/// the packaged path — see the module docs' split-brain note.
/// What: [`InstructionPackage::authored_run`] over the bundled manifest, or `None`
/// when the manifest is unreadable so the caller can fall back to its retained
/// `include_str!` constants.
/// Test: `pm_instructions_is_its_three_sections`, `base_pm_is_its_three_sections`.
pub(crate) fn authored_run(sections: &[SectionId]) -> Option<String> {
    bundled_fallback_package()
        .ok()
        .map(|package| package.authored_run(sections))
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
/// the caller can report them. A manifest that failed to parse surfaces as
/// [`CompositionError::Manifest`] with no rejections.
///
/// Test: `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`,
/// `composed_prompt_carries_the_live_roster_and_the_precedence_note`,
/// `roster_is_required_and_never_droppable`, `golden_claude_md_override_prompt`.
pub(crate) fn compose_bundled_fallback_with_overrides(
    stack: &str,
    roster: &str,
    addendum: Option<&str>,
    overrides: &[SectionOverride],
) -> (Result<String, CompositionError>, Vec<Rejection>) {
    let bundled = match bundled_fallback_package() {
        Ok(package) => package,
        Err(err) => return (Err(CompositionError::Manifest(err.to_string())), Vec::new()),
    };
    let (package, rejected) = bundled.with_overrides(overrides);
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

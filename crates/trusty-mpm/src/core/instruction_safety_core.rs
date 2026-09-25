//! The non-overridable safety core of the PM prompt (#8533).
//!
//! Why: owner ruling 2026-09-24 — a project may override every PM prompt
//! section except a SMALL, NAMED, DOCUMENTED safety core, and agent selection,
//! the memory protocol and the search protocol stay intact under any override.
//! A core stated in three places (manifest tiers, `pinned` flags, docs) drifts;
//! this list is the one statement, and everything else is checked against it.
//! What: [`SAFETY_CORE`] names each member, the section that owns it, and how it
//! survives an override — a tier-`fixed` section declines the override outright,
//! a pinned block and a generated block stay in force while the rest of their
//! section is replaced. `InstructionPackage::validate` derives the `fixed` tier
//! from this list; the manifest's `pinned` flags, the section report, and the
//! docs tables in `tm-workflow.md`, `sections/README.md` and SPEC-PMINSTR-01
//! §11.5 are tested against it.
//! Test: `instruction_safety_core_tests.rs`.

use crate::core::instruction_package::{Generator, SectionId};

/// How a safety-core member survives a project override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyCoreKind {
    /// Its whole section is tier `fixed`: a marker naming it is declined.
    FixedSection,
    /// An authored block marked `pinned`; the rest of its section is replaceable.
    PinnedBlock,
    /// A block the composer computes at launch; no override can author it.
    GeneratedBlock(Generator),
}

impl SafetyCoreKind {
    /// The kind as the docs tables spell it.
    pub const fn label(self) -> &'static str {
        match self {
            SafetyCoreKind::FixedSection => "fixed section",
            SafetyCoreKind::PinnedBlock => "pinned block",
            SafetyCoreKind::GeneratedBlock(_) => "generated block",
        }
    }
}

/// One member of the safety core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyCoreMember {
    /// The member's name, exactly as the docs tables list it.
    pub name: &'static str,
    /// The section that owns the member.
    pub section: SectionId,
    /// How the member survives an override.
    pub kind: SafetyCoreKind,
    /// Text the delivered prompt always carries while the member is in force.
    pub marker: &'static str,
}

/// The safety core: the only prompt content a project override cannot remove.
///
/// Why: see the module docs — this is the single enumeration.
/// What: seven members in prompt order. Adding one here without the matching
/// manifest flag or docs row fails `the_manifest_pins_exactly_the_safety_core`
/// or `the_docs_name_every_safety_core_member`.
/// Test: `the_manifest_pins_exactly_the_safety_core`,
/// `the_docs_name_every_safety_core_member`,
/// `a_safety_core_override_is_declined_for_each_core_section`,
/// `overriding_every_overridable_section_keeps_the_safety_core`.
pub const SAFETY_CORE: [SafetyCoreMember; 7] = [
    SafetyCoreMember {
        name: "Memory & Instruction Sources",
        section: SectionId::Core,
        kind: SafetyCoreKind::FixedSection,
        marker: "## Memory & Instruction Sources",
    },
    SafetyCoreMember {
        name: "Customization Surface",
        section: SectionId::Core,
        kind: SafetyCoreKind::FixedSection,
        marker: "## Customization Surface",
    },
    SafetyCoreMember {
        name: "Detected project stack",
        section: SectionId::Core,
        kind: SafetyCoreKind::GeneratedBlock(Generator::StackProfile),
        marker: crate::core::stack_profile::STACK_PROFILE_HEADING,
    },
    SafetyCoreMember {
        name: "Memory protocol",
        section: SectionId::Memory,
        kind: SafetyCoreKind::PinnedBlock,
        marker: "## Memory Protocol (Context-First)",
    },
    SafetyCoreMember {
        name: "Code search protocol",
        section: SectionId::Search,
        kind: SafetyCoreKind::PinnedBlock,
        marker: "## Code Search Protocol (Context-First)",
    },
    SafetyCoreMember {
        name: "Agent selection",
        section: SectionId::AgentDelegation,
        kind: SafetyCoreKind::PinnedBlock,
        marker: "**Agent selection.**",
    },
    SafetyCoreMember {
        name: "Agent roster",
        section: SectionId::AgentDelegation,
        kind: SafetyCoreKind::GeneratedBlock(Generator::AgentRoster),
        marker: "## Delegation Authority",
    },
];

/// Whether `section` is a tier-`fixed` safety-core section.
///
/// Why: the manifest validator and the section report must agree on which
/// sections decline every override; both ask here.
/// Test: `a_safety_core_override_is_declined_for_each_core_section`.
pub fn is_fixed_core_section(section: SectionId) -> bool {
    SAFETY_CORE
        .iter()
        .any(|m| m.section == section && m.kind == SafetyCoreKind::FixedSection)
}

/// The names of the pinned members `section` keeps under an override.
///
/// Test: `overriding_every_overridable_section_keeps_the_safety_core`.
pub fn pinned_members_of(section: SectionId) -> Vec<&'static str> {
    SAFETY_CORE
        .iter()
        .filter(|m| m.section == section && m.kind == SafetyCoreKind::PinnedBlock)
        .map(|m| m.name)
        .collect()
}

#[cfg(test)]
#[path = "instruction_safety_core_tests.rs"]
mod tests;

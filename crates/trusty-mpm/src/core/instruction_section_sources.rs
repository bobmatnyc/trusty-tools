//! The section sources of the PM instruction package.
//!
//! Why: moved out of `instruction_pipeline.rs` by #8533, whose nine new
//! sections would have pushed that file past the 500-SLOC cap. Declared as a
//! `#[path]` child so `crate::core::instruction_pipeline::*` paths keep
//! resolving through the parent's re-export. Since #9012 the section files are
//! runtime content (`content/instructions/sections/`), not `include_str!`
//! constants.
//! What: [`SECTION_FILES`], the nineteen canonical section paths;
//! [`section_source`] and [`package_sources`], which read them from a loaded
//! [`FrameworkContent`]; [`PM_BODY_SECTIONS`] and [`fallback_source`].
//! Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.

use std::collections::BTreeMap;

use crate::core::framework_content::FrameworkContent;
use crate::core::instruction_package::SectionId;

/// The nineteen canonical section files, relative to `instructions/`.
///
/// Why: the instruction manifest (#4318) names its prose by path
/// (`{"kind":"file","path":"sections/core.md"}`). Every path here is in
/// [`crate::core::framework_content::REQUIRED_INSTRUCTIONS`], so a content
/// source missing one is refused at load, never delivered as a shorter prompt.
/// What: the paths the bundled manifest uses. Table order is irrelevant; the
/// manifest's `blocks` array alone decides emission order.
/// Test: `every_section_source_resolves`.
pub(crate) const SECTION_FILES: [&str; 19] = [
    "sections/identity.md",
    "sections/core.md",
    "sections/pm-allowlist.md",
    "sections/delegation-mechanics.md",
    "sections/agent-routing.md",
    "sections/subagent-re-engagement.md",
    "sections/phases.md",
    "sections/qa-gate.md",
    "sections/git-file-tracking.md",
    "sections/tickets-prs-releases.md",
    "sections/messages-reports-sessions.md",
    "sections/autonomous-execution.md",
    "sections/memory.md",
    "sections/search.md",
    "sections/workflow.md",
    "sections/agent-delegation.md",
    "sections/enforcement.md",
    "sections/non-overridable-rules.md",
    "sections/framework-guaranteed-conventions.md",
];

/// Resolve a manifest `file` body path to its source in `content`.
///
/// Why: one lookup point means a path typo in the manifest becomes a named
/// [`crate::core::instruction_package::ValidationError::UnknownFileSource`]
/// instead of an empty block.
/// What: the `instructions/<path>` file of `content`, for any path under
/// `sections/` (#9012: a content release may add a section file without a
/// binary change); `None` otherwise.
/// Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.
pub(crate) fn section_source<'a>(content: &'a FrameworkContent, path: &str) -> Option<&'a str> {
    path.starts_with("sections/")
        .then(|| content.instruction(path))
        .flatten()
}

/// Every `sections/**` file of `content`, keyed as a manifest `file` body
/// names it — the map [`crate::core::instruction_package::InstructionPackage::sources`]
/// resolves through (#9012).
/// Test: `every_section_source_resolves`.
pub(crate) fn package_sources(content: &FrameworkContent) -> BTreeMap<String, String> {
    content
        .instruction_paths("sections/")
        .filter_map(|path| Some((path.to_string(), section_source(content, path)?.to_string())))
        .collect()
}

/// The sections the legacy assembly treats as one PM body, in prompt order.
///
/// Why: #8533 moved Identity to the top and split `core` into nine sections;
/// the roster-absent assembly still needs them as one string, positioned
/// before the stack profile exactly as the package emits them.
/// What: every section whose blocks precede the stack-profile block.
/// Test: `pm_instructions_is_the_pm_body_sections`.
pub(crate) const PM_BODY_SECTIONS: [SectionId; 14] = [
    SectionId::Identity,
    SectionId::Core,
    SectionId::PmAllowlist,
    SectionId::DelegationMechanics,
    SectionId::AgentRouting,
    SectionId::SubagentReEngagement,
    SectionId::Phases,
    SectionId::QaGate,
    SectionId::GitFileTracking,
    SectionId::TicketsPrsReleases,
    SectionId::MessagesReportsSessions,
    SectionId::AutonomousExecution,
    SectionId::Memory,
    SectionId::Search,
];

/// The section file a PM-body section is authored in, for the unreadable-
/// manifest fallback only.
///
/// What: the file source by section; `None` for sections outside the PM body.
/// Test: `pm_instructions_is_the_pm_body_sections`.
pub(crate) fn fallback_source(content: &FrameworkContent, id: SectionId) -> Option<&str> {
    let path = match id {
        SectionId::Identity => "identity",
        SectionId::Core => "core",
        SectionId::PmAllowlist => "pm-allowlist",
        SectionId::DelegationMechanics => "delegation-mechanics",
        SectionId::AgentRouting => "agent-routing",
        SectionId::SubagentReEngagement => "subagent-re-engagement",
        SectionId::Phases => "phases",
        SectionId::QaGate => "qa-gate",
        SectionId::GitFileTracking => "git-file-tracking",
        SectionId::TicketsPrsReleases => "tickets-prs-releases",
        SectionId::MessagesReportsSessions => "messages-reports-sessions",
        SectionId::AutonomousExecution => "autonomous-execution",
        SectionId::Memory => "memory",
        SectionId::Search => "search",
        _ => return None,
    };
    section_source(content, &format!("sections/{path}.md"))
}

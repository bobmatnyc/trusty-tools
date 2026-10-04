//! The section sources of the PM instruction package.
//!
//! Why: moved out of `instruction_pipeline.rs` by #8533, whose nine new
//! sections would have pushed that file past the 500-SLOC cap. Declared as a
//! `#[path]` child so `crate::core::instruction_pipeline::*` paths keep
//! resolving through the parent's re-export. Since #9012 the section files are
//! runtime content (`content/instructions/sections/`), not `include_str!`
//! constants.
//! What: [`SECTION_FILES`], the nineteen canonical section paths, and
//! [`PM_BODY_SECTIONS`]. The manifest's `file` bodies bind to the content's
//! `sections/**` files when [`crate::core::framework_content::FrameworkContent::load`]
//! parses the package.
//! Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.

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

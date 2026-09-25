//! The embedded section sources of the PM instruction package.
//!
//! Why: moved out of `instruction_pipeline.rs` by #8533, whose nine new
//! sections would have pushed that file past the 500-SLOC cap. Declared as a
//! `#[path]` child so `crate::core::instruction_pipeline::SECTION_*` paths
//! keep resolving through the parent's re-export.
//! What: one `include_str!` constant per section file, the
//! [`SECTION_SOURCES`] table the manifest's `file` bodies resolve through,
//! and [`section_source`].
//! Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.

use crate::core::instruction_package::SectionId;

/// Who the PM is — the prompt's opening (#8533). Tier `project`.
pub(crate) const SECTION_IDENTITY: &str =
    include_str!("../assets/instructions/sections/identity.md");
/// The safety core — the only tier-`fixed` section (#8533).
pub(crate) const SECTION_CORE: &str = include_str!("../assets/instructions/sections/core.md");
/// The PM allowlist. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_PM_ALLOWLIST: &str =
    include_str!("../assets/instructions/sections/pm-allowlist.md");
/// Delegation mechanics. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_DELEGATION_MECHANICS: &str =
    include_str!("../assets/instructions/sections/delegation-mechanics.md");
/// Agent routing and delegating well. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_AGENT_ROUTING: &str =
    include_str!("../assets/instructions/sections/agent-routing.md");
/// Parked-subagent re-engagement. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_SUBAGENT_RE_ENGAGEMENT: &str =
    include_str!("../assets/instructions/sections/subagent-re-engagement.md");
/// The 5-phase workflow summary. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_PHASES: &str = include_str!("../assets/instructions/sections/phases.md");
/// The QA verification gate. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_QA_GATE: &str = include_str!("../assets/instructions/sections/qa-gate.md");
/// The git file-tracking protocol. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_GIT_FILE_TRACKING: &str =
    include_str!("../assets/instructions/sections/git-file-tracking.md");
/// Tickets, PRs and releases. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_TICKETS_PRS_RELEASES: &str =
    include_str!("../assets/instructions/sections/tickets-prs-releases.md");
/// Messages, reports, sessions and prose style. Split out of `core.md` by #8533; tier `project`.
pub(crate) const SECTION_MESSAGES_REPORTS_SESSIONS: &str =
    include_str!("../assets/instructions/sections/messages-reports-sessions.md");
/// When the PM runs without stopping, and when it may stop and ask. Tier
/// `project`.
///
/// Split out of `core.md` by #8361: `core` is tier `fixed`, so while the rule
/// lived there no project could set its own comfort level — and on a session
/// resume the PM read one rule telling it to continue and a skill telling it to
/// confirm, with no override surface to settle the two.
pub(crate) const SECTION_AUTONOMOUS_EXECUTION: &str =
    include_str!("../assets/instructions/sections/autonomous-execution.md");
/// Memory (context-first) protocol guidance. Tier `project`.
pub(crate) const SECTION_MEMORY: &str = include_str!("../assets/instructions/sections/memory.md");
/// Code/architecture search protocol guidance. Tier `project`.
pub(crate) const SECTION_SEARCH: &str = include_str!("../assets/instructions/sections/search.md");
/// 5-phase workflow execution details, including the sprint/harden doctrine.
///
/// `pub(crate)` so the override resolver can use it when no `WORKFLOW.md`
/// override is present.
pub(crate) const WORKFLOW: &str = include_str!("../assets/instructions/sections/workflow.md");
/// Agent delegation routing doctrine (the live roster is appended at compose
/// time, never authored here).
///
/// `pub(crate)` so the override resolver can use it when no
/// `AGENT_DELEGATION.md` override is present.
pub(crate) const AGENT_DELEGATION: &str =
    include_str!("../assets/instructions/sections/agent-delegation.md");
/// The canonical Prohibitions and Circuit Breakers tables. Floor, tier `fixed`.
///
/// Split out of `core.md` by #4573: both tables sat inside the `project`-tier
/// core section, so a three-line `CORE` block in a project's `CLAUDE.md` deleted
/// the PM's entire delegation-enforcement authority and still validated.
pub(crate) const SECTION_ENFORCEMENT: &str =
    include_str!("../assets/instructions/sections/enforcement.md");
/// Absorbed BASE_PM non-overridable rules, the customization contract, and the
/// Trusty tool-priority mandate. Floor, tier `fixed`.
pub(crate) const SECTION_NON_OVERRIDABLE_RULES: &str =
    include_str!("../assets/instructions/sections/non-overridable-rules.md");
/// Absorbed BASE_PM framework-guaranteed conventions. Floor, tier `fixed`.
pub(crate) const SECTION_FRAMEWORK_CONVENTIONS: &str =
    include_str!("../assets/instructions/sections/framework-guaranteed-conventions.md");

/// The compile-time table a schema-v2 `file` body resolves through.
///
/// Why: the instruction manifest (#4318) names its prose by path
/// (`{"kind":"file","path":"sections/core.md"}`) so the bulk of the instructions
/// keeps living in reviewable markdown rather than becoming one 23 KB JSON line
/// — but a path resolved at *runtime* would put the delivered system prompt at
/// the mercy of the filesystem and would let a renamed section ship as a silent
/// content drop. Every entry here is an `include_str!` of a constant declared
/// above, so the build stays hermetic and a missing section file is a compile
/// error rather than a launch-time surprise.
/// What: the nineteen canonical section sources, keyed by the path form the manifest
/// uses — relative to `assets/instructions/`. Table order is irrelevant; the
/// manifest's `blocks` array alone decides emission order.
/// Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.
pub(crate) const SECTION_SOURCES: [(&str, &str); 19] = [
    ("sections/identity.md", SECTION_IDENTITY),
    ("sections/core.md", SECTION_CORE),
    ("sections/pm-allowlist.md", SECTION_PM_ALLOWLIST),
    (
        "sections/delegation-mechanics.md",
        SECTION_DELEGATION_MECHANICS,
    ),
    ("sections/agent-routing.md", SECTION_AGENT_ROUTING),
    (
        "sections/subagent-re-engagement.md",
        SECTION_SUBAGENT_RE_ENGAGEMENT,
    ),
    ("sections/phases.md", SECTION_PHASES),
    ("sections/qa-gate.md", SECTION_QA_GATE),
    ("sections/git-file-tracking.md", SECTION_GIT_FILE_TRACKING),
    (
        "sections/tickets-prs-releases.md",
        SECTION_TICKETS_PRS_RELEASES,
    ),
    (
        "sections/messages-reports-sessions.md",
        SECTION_MESSAGES_REPORTS_SESSIONS,
    ),
    (
        "sections/autonomous-execution.md",
        SECTION_AUTONOMOUS_EXECUTION,
    ),
    ("sections/memory.md", SECTION_MEMORY),
    ("sections/search.md", SECTION_SEARCH),
    ("sections/workflow.md", WORKFLOW),
    ("sections/agent-delegation.md", AGENT_DELEGATION),
    ("sections/enforcement.md", SECTION_ENFORCEMENT),
    (
        "sections/non-overridable-rules.md",
        SECTION_NON_OVERRIDABLE_RULES,
    ),
    (
        "sections/framework-guaranteed-conventions.md",
        SECTION_FRAMEWORK_CONVENTIONS,
    ),
];

/// Resolve a manifest `file` body path to its embedded source.
///
/// Why: one lookup point means a path typo in the manifest becomes a named
/// [`crate::core::instruction_package::ValidationError::UnknownFileSource`]
/// instead of an empty block.
/// What: a linear scan of [`SECTION_SOURCES`] — one per section, called a handful
/// of times per process, so a map would buy nothing and would reintroduce the
/// iteration-order hazard the package format exists to avoid.
/// Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.
pub(crate) fn section_source(path: &str) -> Option<&'static str> {
    SECTION_SOURCES
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, body)| *body)
}

/// The sections the legacy assembly treats as one PM body, in prompt order.
///
/// Why: #8533 moved Identity to the top and split `core` into nine sections;
/// the roster-absent assembly still needs them as one string, positioned
/// before the stack profile exactly as the package emits them.
/// What: every section whose blocks precede the stack-profile block.
/// Test: `pm_instructions_is_its_four_sections`.
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
/// Test: `pm_instructions_is_its_four_sections`.
pub(crate) fn fallback_source(id: SectionId) -> Option<&'static str> {
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
    section_source(&format!("sections/{path}.md"))
}

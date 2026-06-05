//! Phase 1 (#770) mpm-* guidance skill constants.
//!
//! Why: `bundle.rs` embeds every framework artifact; as the skill catalog
//! grows the file would exceed the 500-line cap. Splitting skill constants
//! into this focused module keeps both files under the cap while making the
//! skill catalog easy to audit at a glance.
//! What: `pub const` strings for each bundled guidance skill, embedded at
//! compile time via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `constants_are_non_empty`,
//! `phase1_guidance_skills_are_in_bundle`, `phase1_guidance_skills_have_frontmatter`.

/// Common delegation patterns for PM agent.
///
/// Why: agents need authoritative delegation workflow references so the PM
/// selects the right agent type and evidence chain for each task class.
/// What: embedded markdown skill file deployed to `skills/mpm-delegation-patterns.md`.
/// Test: `constants_are_non_empty` asserts non-empty; `phase1_guidance_skills_are_in_bundle`
/// confirms it appears in ALL.
pub const MPM_DELEGATION_PATTERNS: &str =
    include_str!("../assets/skills/mpm-delegation-patterns.md");

/// QA verification gate and evidence requirements.
///
/// Why: prevents PM from claiming work complete without QA evidence, which
/// is the most common source of silent quality regressions.
/// What: embedded markdown skill file deployed to `skills/mpm-verification-protocols.md`.
/// Test: `phase1_guidance_skills_have_frontmatter` checks frontmatter.
pub const MPM_VERIFICATION_PROTOCOLS: &str =
    include_str!("../assets/skills/mpm-verification-protocols.md");

/// Protocol for tracking files immediately after agent creation.
///
/// Why: untracked deliverables are lost between agent completion and session
/// end; this skill enforces the blocking git-tracking gate.
/// What: embedded markdown skill file deployed to `skills/mpm-git-file-tracking.md`.
/// Test: `phase1_guidance_skills_are_in_bundle`.
pub const MPM_GIT_FILE_TRACKING: &str = include_str!("../assets/skills/mpm-git-file-tracking.md");

/// Branch protection and PR creation workflow.
///
/// Why: direct pushes to main bypass review and break the squash-merge
/// invariant; this skill enforces the feature-branch + PR workflow.
/// What: embedded markdown skill file deployed to `skills/mpm-pr-workflow.md`.
/// Test: `phase1_guidance_skills_have_frontmatter`.
pub const MPM_PR_WORKFLOW: &str = include_str!("../assets/skills/mpm-pr-workflow.md");

/// Ticket-driven development protocol.
///
/// Why: ticket-driven development requires consistent ticket state transitions
/// and comment trails that the PM must orchestrate without touching ticket
/// tools directly.
/// What: embedded markdown skill file deployed to `skills/mpm-ticketing-integration.md`.
/// Test: `phase1_guidance_skills_are_in_bundle`.
pub const MPM_TICKETING_INTEGRATION: &str =
    include_str!("../assets/skills/mpm-ticketing-integration.md");

/// Complete circuit breaker enforcement patterns.
///
/// Why: circuit breakers are the runtime enforcement layer for PM behavioral
/// contracts; agents need the full catalogue with examples to recognise and
/// remediate violations.
/// What: embedded markdown skill file deployed to `skills/mpm-circuit-breaker-enforcement.md`.
/// Test: `phase1_guidance_skills_have_frontmatter`.
pub const MPM_CIRCUIT_BREAKER_ENFORCEMENT: &str =
    include_str!("../assets/skills/mpm-circuit-breaker-enforcement.md");

/// Bug reporting protocol for PM and agents.
///
/// Why: framework bugs discovered during sessions must be filed to the
/// correct repo with the right labels; this skill provides the routing
/// decision tree and delegation template.
/// What: embedded markdown skill file deployed to `skills/mpm-bug-reporting.md`.
/// Test: `phase1_guidance_skills_are_in_bundle`.
pub const MPM_BUG_REPORTING: &str = include_str!("../assets/skills/mpm-bug-reporting.md");

/// Session pause/resume capabilities for PM context limit management.
///
/// Why: sessions approaching context limits need a structured handoff
/// protocol to capture in-progress work and enable clean resume.
/// What: embedded markdown skill file deployed to `skills/mpm-session-management.md`.
/// Test: `phase1_guidance_skills_have_frontmatter`.
pub const MPM_SESSION_MANAGEMENT: &str = include_str!("../assets/skills/mpm-session-management.md");

/// Pause session and save current work state for later resume.
///
/// Why: provides the user-invocable `/mpm-session-pause` slash command
/// with concrete PM instructions for capturing session state to disk.
/// What: embedded markdown skill file deployed to `skills/mpm-session-pause.md`.
/// Test: `phase1_guidance_skills_are_in_bundle`.
pub const MPM_SESSION_PAUSE: &str = include_str!("../assets/skills/mpm-session-pause.md");

/// Load context from paused session.
///
/// Why: provides the user-invocable `/mpm-session-resume` slash command
/// with concrete PM instructions for restoring session state from disk.
/// What: embedded markdown skill file deployed to `skills/mpm-session-resume.md`.
/// Test: `phase1_guidance_skills_have_frontmatter`.
pub const MPM_SESSION_RESUME: &str = include_str!("../assets/skills/mpm-session-resume.md");

/// Detailed tool usage patterns and examples for PM agents.
///
/// Why: PM agents need a reference for correct tool delegation — what to
/// use, what to forbid, and how to escalate to specialist agents.
/// What: embedded markdown skill file deployed to `skills/mpm-tool-usage-guide.md`.
/// Test: `phase1_guidance_skills_are_in_bundle`.
pub const MPM_TOOL_USAGE_GUIDE: &str = include_str!("../assets/skills/mpm-tool-usage-guide.md");

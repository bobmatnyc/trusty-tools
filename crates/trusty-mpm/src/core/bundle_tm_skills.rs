//! The `/tm-*` PM skill portfolio — constants embedding each bundled skill.
//!
//! Why: the `mpm-*` guidance skills (`bundle_skills.rs`) were a direct,
//! mechanical port of claude-mpm's Python skill catalog — same names, same
//! claude-mpm-flavored tool/path references (`~/.claude-mpm/`, Socket.IO
//! dashboards on :8765, `mcp__mcp-ticketer__*`, `deepeval test --agent`). None
//! of that matches trusty-mpm's actual Rust daemon/MCP-tool surface. This
//! module holds the tm-native replacement catalog: same behavioral intent,
//! rewritten against the real `mcp__trusty-mpm__*` / `mcp__trusty-memory__*` /
//! `mcp__trusty-search__*` / `mcp__trusty-review__*` tool families, the real
//! `.trusty-mpm/` paths, and the real agent roster.
//! What: `pub const` strings for each `/tm-*` skill, embedded at compile time
//! via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `tm_skills_are_in_bundle`,
//! `tm_skills_have_frontmatter`, `constants_are_non_empty`.

/// User-invocable `/tm-doctor` slash command — runs `tm doctor`.
///
/// Why: `tm-doctor.md` existed as an asset file (A3, tm-skills-portfolio
/// epic) but was never wired into a const or the `ALL` bundle table, so it
/// silently never shipped to any user despite being a real, useful skill
/// that just shells out to the already-documented `tm doctor` diagnostic.
/// What: embedded markdown skill file deployed to `skills/tm-doctor.md`.
/// Test: `tm_doctor_skill_is_wired_into_bundle`.
pub const TM_DOCTOR: &str = include_str!("../assets/skills/tm-doctor.md");

/// Circuit breaker enforcement patterns — the crown jewel of the portfolio.
///
/// Why: the PM's delegation discipline (CB#1-10, CB#14) is the single most
/// consequential behavioral contract in the framework; agents need the full
/// detection-pattern/violation/remediation reference, not just the summary
/// table in `PM_INSTRUCTIONS.md`.
/// What: embedded markdown skill file deployed to `skills/tm-circuit-breaker.md`.
/// Test: `tm_skills_are_in_bundle`, `tm_skills_have_frontmatter`.
pub const TM_CIRCUIT_BREAKER: &str = include_str!("../assets/skills/tm-circuit-breaker.md");

/// QA verification gate and evidence standards.
///
/// Why: prevents the PM from claiming work complete without agent-produced
/// evidence — the most common source of silent quality regressions.
/// What: embedded markdown skill file deployed to `skills/tm-verification-protocols.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_VERIFICATION_PROTOCOLS: &str =
    include_str!("../assets/skills/tm-verification-protocols.md");

/// Detailed tool usage patterns for the PM's real MCP tool families.
///
/// Why: the PM needs a concrete reference for `mcp__trusty-mpm__*` /
/// `mcp__trusty-memory__*` / `mcp__trusty-search__*` / `mcp__trusty-review__*`
/// usage and the vector-search-first discipline.
/// What: embedded markdown skill file deployed to `skills/tm-tool-usage-guide.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_TOOL_USAGE_GUIDE: &str = include_str!("../assets/skills/tm-tool-usage-guide.md");

/// Immediate post-agent git file tracking protocol.
///
/// Why: untracked deliverables are lost between agent completion and session
/// end; this skill enforces the blocking git-tracking gate (CB#4).
/// What: embedded markdown skill file deployed to `skills/tm-git-file-tracking.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_GIT_FILE_TRACKING: &str = include_str!("../assets/skills/tm-git-file-tracking.md");

/// Architecture Decision Records (Nygard template) discipline.
///
/// Why: significant, hard-to-reverse architectural decisions need a durable,
/// discoverable record; this is opt-in guidance for when to write one.
/// What: embedded markdown skill file deployed to `skills/tm-adr.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_ADR: &str = include_str!("../assets/skills/tm-adr.md");

/// PM workflow/phase/gate customization via project-level `.trusty-mpm/` overrides.
///
/// Why: maps to the *real* override mechanism in `core/instruction_overrides.rs`
/// and `core/instruction_pipeline.rs`, not a fixed claude-mpm 5-phase script.
/// What: embedded markdown skill file deployed to `skills/tm-workflow.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_WORKFLOW: &str = include_str!("../assets/skills/tm-workflow.md");

/// Official (source→rebuild→deploy) vs custom agent update workflow.
///
/// Why: prevents agents from editing a deployed `~/.claude/agents/*.md` build
/// output instead of the composable source under `assets/agents/`.
/// What: embedded markdown skill file deployed to `skills/tm-agent-architecture.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_AGENT_ARCHITECTURE: &str = include_str!("../assets/skills/tm-agent-architecture.md");

/// Session-error postmortem orchestration over the MCP bug-reporting pipeline.
///
/// Why: routes through the real `list_recent_errors` / `preview_bug_report` /
/// `report_bug` MCP tools instead of ad-hoc log parsing.
/// What: embedded markdown skill file deployed to `skills/tm-postmortem.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_POSTMORTEM: &str = include_str!("../assets/skills/tm-postmortem.md");

/// Bug reporting protocol — MCP-native filing pipeline, single-repo routing.
///
/// Why: trusty-tools is one monorepo (no claude-mpm/-agents/-skills split),
/// and filing is MCP-native (`report_bug`) rather than manual `gh issue create`.
/// What: embedded markdown skill file deployed to `skills/tm-bug-reporting.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_BUG_REPORTING: &str = include_str!("../assets/skills/tm-bug-reporting.md");

/// Progressive-disclosure onboarding/teaching templates.
///
/// Why: condensed from claude-mpm's 658-line teaching-mode skill, stripping
/// generic (non-framework) examples and pointing at the concrete tm-* skills
/// instead of duplicating their content.
/// What: embedded markdown skill file deployed to `skills/tm-teaching-templates.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_TEACHING_TEMPLATES: &str = include_str!("../assets/skills/tm-teaching-templates.md");

/// Ticket-driven development protocol and ticketing orchestration.
///
/// Why: consolidates mpm-ticket-view + mpm-ticketing-integration; verified
/// against the real bundled `ticketing` agent's mcp-ticketer/aitrackdown
/// priority (not assumed).
/// What: embedded markdown skill file deployed to `skills/tm-ticketing.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_TICKETING: &str = include_str!("../assets/skills/tm-ticketing.md");

/// Branch protection, trusty-review gate, squash-merge, worktree discipline.
///
/// Why: SLIM per the portfolio design — native Claude Code already knows
/// basic `gh pr create` mechanics; this keeps only the tm-specific layer.
/// What: embedded markdown skill file deployed to `skills/tm-pr-workflow.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_PR_WORKFLOW: &str = include_str!("../assets/skills/tm-pr-workflow.md");

/// Delegation matrices and agent-selection decision trees.
///
/// Why: SLIM per the portfolio design — drops native Agent/Task-tool basics,
/// keeps only the tm-specific chains and the verified real agent roster.
/// What: embedded markdown skill file deployed to `skills/tm-delegation-patterns.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_DELEGATION_PATTERNS: &str = include_str!("../assets/skills/tm-delegation-patterns.md");

/// PM context-limit pause/resume, project-local sessions, worktree pruning.
///
/// Why: consolidates mpm-session-management + mpm-session-pause +
/// mpm-session-resume into one skill; keeps the 70% auto-pause threshold, the
/// `.trusty-mpm/sessions/` format, `tm session prune-worktrees`, and
/// trusty-memory task_add/task_list/task_complete integration.
/// What: embedded markdown skill file deployed to `skills/tm-session-management.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_SESSION_MANAGEMENT: &str = include_str!("../assets/skills/tm-session-management.md");

/// Focused `/tm-session-pause` action — snapshot + prune, split from the
/// consolidated session-management overview.
///
/// Why: restores the per-action UX of claude-mpm's `/mpm-session-pause`; users
/// want a single-purpose "pause and save my work" entry point rather than
/// reading the full policy skill. Adapted to tm-native `.trusty-mpm/sessions/`
/// paths and `tm session prune-worktrees` (no `.claude-mpm/` references).
/// What: embedded markdown skill file deployed to `skills/tm-session-pause.md`.
/// Test: `tm_skills_are_in_bundle`, `tm_skills_have_frontmatter`.
pub const TM_SESSION_PAUSE: &str = include_str!("../assets/skills/tm-session-pause.md");

/// Focused `/tm-session-resume` action — load + restore, split from the
/// consolidated session-management overview.
///
/// Why: restores the per-action UX of claude-mpm's `/mpm-session-resume`; a
/// single-purpose "resume my paused session" entry point over `tm session
/// catchup`, with project-path validation. Adapted to tm-native
/// `.trusty-mpm/sessions/` paths (no `.claude-mpm/` references).
/// What: embedded markdown skill file deployed to `skills/tm-session-resume.md`.
/// Test: `tm_skills_are_in_bundle`, `tm_skills_have_frontmatter`.
pub const TM_SESSION_RESUME: &str = include_str!("../assets/skills/tm-session-resume.md");

/// SLIM `/tm` orchestration-model overview (optional 15th portfolio skill).
///
/// Why: a quick-reference entry point for the agent roster, skills catalog,
/// memory system, and health tooling — the tm-native replacement for the
/// claude-mpm `mpm` overview skill.
/// What: embedded markdown skill file deployed to `skills/tm.md`.
/// Test: `tm_skills_are_in_bundle`.
pub const TM_OVERVIEW: &str = include_str!("../assets/skills/tm.md");

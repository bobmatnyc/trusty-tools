//! Embedded default agents & skills for trusty-code (#2895).
//!
//! Why: A fresh project has no `.claude/agents/` or `.claude/skills/` yet, so
//! `tcode` would otherwise start with zero agents and zero skills — a cold,
//! unusable default. Bundling a working default set at compile time (mirroring
//! the embed pattern `trusty-mpm` uses in `crates/trusty-mpm/src/core/bundle.rs`,
//! but without trusty-mpm's disk-materialize install step — these are parsed
//! in-memory only) gives every project a usable harness out of the box while
//! disk-based `.claude/agents/` and `.claude/skills/` always take precedence
//! when present (see `agents::load_all_agents` and
//! `skills::discover_skill_metadata`'s embedded-fallback branches).
//! What: [`EmbeddedAgent`]/[`DEFAULT_AGENTS`] — the three default agent
//! configs (`engineer`, `qa-agent`, `code-reviewer`), authored as Markdown+
//! frontmatter (`.md`) as of #2897 Slice C (previously native TOML; the TOML
//! loader for USER `.claude/agents/*.toml` configs is untouched and stays
//! until Slice D). The embedded fallback projects each `.md` string onto
//! tcode's `AgentConfig` via `agents::md_loader::project_embedded_md`, which
//! shares its frontmatter->`AgentConfig` mapping with the disk `.md` loader
//! (`agents::md_loader::load_md_agent`) — see that module's docs.
//! [`EmbeddedSkill`]/[`DEFAULT_SKILLS`] — trusty-mpm's universal skill set
//! (format-identical `SKILL.md` files), reused verbatim per Bob's reuse
//! directive; the `tm-*` orchestration skills are excluded because they drive
//! trusty-mpm MCP tools tcode does not have.
//! Test: `assets::tests::*` — every embedded agent `.md` parses and projects
//! to a field-identical `AgentConfig` vs. the retired TOML fixtures, every
//! skill name is unique, and every skill's frontmatter `name:` matches its
//! table key.

/// One embedded default agent: its dispatch name and raw `.md` source.
///
/// Why: `agents::mod`'s embedded-fallback needs both the name (for logging)
/// and the raw `.md` document (to hand to
/// `agents::md_loader::project_embedded_md`).
/// What: `name` matches the frontmatter `name:` field inside `md`; `md` is
/// the verbatim embedded file contents (frontmatter fence + prose body).
/// Test: `assets::tests::default_agents_parse_and_names_match`.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedAgent {
    /// Dispatch key, matching the frontmatter `name:` inside `md`.
    pub name: &'static str,
    /// Raw `.md` source (frontmatter fence + prose body), projectable via
    /// `agents::md_loader::project_embedded_md`.
    pub md: &'static str,
}

/// One embedded default skill: its catalog name and raw `SKILL.md` source.
///
/// Why: `skills::mod`'s embedded-fallback needs both the name (for the
/// `SkillMetadata` catalog) and the raw Markdown (frontmatter + body, parsed
/// the same way a disk-based `SKILL.md` is).
/// What: `name` matches the frontmatter `name:` inside `skill_md`; `skill_md`
/// is the verbatim embedded file contents (frontmatter fence + body).
/// Test: `assets::tests::default_skills_names_are_unique`.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedSkill {
    /// Catalog name, matching the frontmatter `name:` inside `skill_md`.
    pub name: &'static str,
    /// Raw `SKILL.md` source (frontmatter fence + Markdown body).
    pub skill_md: &'static str,
}

const ENGINEER_MD: &str = include_str!("agents/engineer.md");
const QA_AGENT_MD: &str = include_str!("agents/qa-agent.md");
const CODE_REVIEWER_MD: &str = include_str!("agents/code-reviewer.md");

/// The three default tcode agents, embedded at compile time.
///
/// Why: gives `agents::load_all_agents`'s embedded-fallback branch a fixed,
/// ordered table to parse when the disk `.claude/agents/` directory is empty
/// or absent.
/// What: `engineer` (general implementation, full read/write tool set),
/// `qa-agent` (verification — read/inspect/run only, no `write_file`/`edit`,
/// hands bugs back to the engineer rather than fixing them), `code-reviewer`
/// (adversarial, read-only review, no `bash`).
/// Test: `assets::tests::default_agents_parse_and_names_match`.
pub const DEFAULT_AGENTS: &[EmbeddedAgent] = &[
    EmbeddedAgent {
        name: "engineer",
        md: ENGINEER_MD,
    },
    EmbeddedAgent {
        name: "qa-agent",
        md: QA_AGENT_MD,
    },
    EmbeddedAgent {
        name: "code-reviewer",
        md: CODE_REVIEWER_MD,
    },
];

const API_DESIGN_PATTERNS_SKILL: &str = include_str!("skills/api-design-patterns/SKILL.md");
const API_DOCUMENTATION_SKILL: &str = include_str!("skills/api-documentation/SKILL.md");
const ARTIFACTS_BUILDER_SKILL: &str = include_str!("skills/artifacts-builder/SKILL.md");
const BRAINSTORMING_SKILL: &str = include_str!("skills/brainstorming/SKILL.md");
const CODE_PRODUCTION_PROCESS_SKILL: &str = include_str!("skills/code-production-process/SKILL.md");
const CODE_REVIEW_STANDARDS_SKILL: &str = include_str!("skills/code-review-standards/SKILL.md");
const CONDITION_BASED_WAITING_SKILL: &str = include_str!("skills/condition-based-waiting/SKILL.md");
const CONTRACT_DRIVEN_TESTING_SKILL: &str = include_str!("skills/contract-driven-testing/SKILL.md");
const DATABASE_MIGRATION_SKILL: &str = include_str!("skills/database-migration/SKILL.md");
const DOCUMENTATION_STYLE_SKILL: &str = include_str!("skills/documentation-style/SKILL.md");
const ENV_MANAGER_SKILL: &str = include_str!("skills/env-manager/SKILL.md");
const GIT_WORKFLOW_SKILL: &str = include_str!("skills/git-workflow/SKILL.md");
const INTERNAL_COMMS_SKILL: &str = include_str!("skills/internal-comms/SKILL.md");
const JSON_DATA_HANDLING_SKILL: &str = include_str!("skills/json-data-handling/SKILL.md");
const MODEL_CONTEXT_BUILDER_SKILL: &str = include_str!("skills/model-context-builder/SKILL.md");
const REQUESTING_CODE_REVIEW_SKILL: &str = include_str!("skills/requesting-code-review/SKILL.md");
const ROOT_CAUSE_TRACING_SKILL: &str = include_str!("skills/root-cause-tracing/SKILL.md");
const SECURITY_SCANNING_SKILL: &str = include_str!("skills/security-scanning/SKILL.md");
const SOFTWARE_PATTERNS_SKILL: &str = include_str!("skills/software-patterns/SKILL.md");
const SYSTEMATIC_DEBUGGING_SKILL: &str = include_str!("skills/systematic-debugging/SKILL.md");
const TEST_DRIVEN_DEVELOPMENT_SKILL: &str = include_str!("skills/test-driven-development/SKILL.md");
const TEST_QUALITY_INSPECTOR_SKILL: &str = include_str!("skills/test-quality-inspector/SKILL.md");
const TESTING_ANTI_PATTERNS_SKILL: &str = include_str!("skills/testing-anti-patterns/SKILL.md");
const VERIFICATION_BEFORE_COMPLETION_SKILL: &str =
    include_str!("skills/verification-before-completion/SKILL.md");
const WEB_PERFORMANCE_OPTIMIZATION_SKILL: &str =
    include_str!("skills/web-performance-optimization/SKILL.md");
const WEBAPP_TESTING_SKILL: &str = include_str!("skills/webapp-testing/SKILL.md");
const WRITING_PLANS_SKILL: &str = include_str!("skills/writing-plans/SKILL.md");
const XLSX_SKILL: &str = include_str!("skills/xlsx/SKILL.md");

/// trusty-mpm's universal skill set, embedded at compile time (`tm-*`
/// orchestration skills excluded — see module docs).
///
/// Why: gives `skills::discover_skill_metadata`'s embedded-fallback branch a
/// fixed, ordered table to build a `SkillMetadata` catalog from when the disk
/// `.claude/skills/` directory is empty or absent.
/// What: 28 skills, sorted by name, each an `EmbeddedSkill { name, skill_md }`.
/// Test: `assets::tests::default_skills_names_are_unique`.
pub const DEFAULT_SKILLS: &[EmbeddedSkill] = &[
    EmbeddedSkill {
        name: "api-design-patterns",
        skill_md: API_DESIGN_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "api-documentation",
        skill_md: API_DOCUMENTATION_SKILL,
    },
    EmbeddedSkill {
        name: "artifacts-builder",
        skill_md: ARTIFACTS_BUILDER_SKILL,
    },
    EmbeddedSkill {
        name: "brainstorming",
        skill_md: BRAINSTORMING_SKILL,
    },
    EmbeddedSkill {
        name: "code-production-process",
        skill_md: CODE_PRODUCTION_PROCESS_SKILL,
    },
    EmbeddedSkill {
        name: "code-review-standards",
        skill_md: CODE_REVIEW_STANDARDS_SKILL,
    },
    EmbeddedSkill {
        name: "condition-based-waiting",
        skill_md: CONDITION_BASED_WAITING_SKILL,
    },
    EmbeddedSkill {
        name: "contract-driven-testing",
        skill_md: CONTRACT_DRIVEN_TESTING_SKILL,
    },
    EmbeddedSkill {
        name: "database-migration",
        skill_md: DATABASE_MIGRATION_SKILL,
    },
    EmbeddedSkill {
        name: "documentation-style",
        skill_md: DOCUMENTATION_STYLE_SKILL,
    },
    EmbeddedSkill {
        name: "env-manager",
        skill_md: ENV_MANAGER_SKILL,
    },
    EmbeddedSkill {
        name: "git-workflow",
        skill_md: GIT_WORKFLOW_SKILL,
    },
    EmbeddedSkill {
        name: "internal-comms",
        skill_md: INTERNAL_COMMS_SKILL,
    },
    EmbeddedSkill {
        name: "json-data-handling",
        skill_md: JSON_DATA_HANDLING_SKILL,
    },
    EmbeddedSkill {
        name: "model-context-builder",
        skill_md: MODEL_CONTEXT_BUILDER_SKILL,
    },
    EmbeddedSkill {
        name: "requesting-code-review",
        skill_md: REQUESTING_CODE_REVIEW_SKILL,
    },
    EmbeddedSkill {
        name: "root-cause-tracing",
        skill_md: ROOT_CAUSE_TRACING_SKILL,
    },
    EmbeddedSkill {
        name: "security-scanning",
        skill_md: SECURITY_SCANNING_SKILL,
    },
    EmbeddedSkill {
        name: "software-patterns",
        skill_md: SOFTWARE_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "systematic-debugging",
        skill_md: SYSTEMATIC_DEBUGGING_SKILL,
    },
    EmbeddedSkill {
        name: "test-driven-development",
        skill_md: TEST_DRIVEN_DEVELOPMENT_SKILL,
    },
    EmbeddedSkill {
        name: "test-quality-inspector",
        skill_md: TEST_QUALITY_INSPECTOR_SKILL,
    },
    EmbeddedSkill {
        name: "testing-anti-patterns",
        skill_md: TESTING_ANTI_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "verification-before-completion",
        skill_md: VERIFICATION_BEFORE_COMPLETION_SKILL,
    },
    EmbeddedSkill {
        name: "web-performance-optimization",
        skill_md: WEB_PERFORMANCE_OPTIMIZATION_SKILL,
    },
    EmbeddedSkill {
        name: "webapp-testing",
        skill_md: WEBAPP_TESTING_SKILL,
    },
    EmbeddedSkill {
        name: "writing-plans",
        skill_md: WRITING_PLANS_SKILL,
    },
    EmbeddedSkill {
        name: "xlsx",
        skill_md: XLSX_SKILL,
    },
];

// -- Tests --------------------------------------------------------------------
// Split into `tests.rs` (not inlined) to keep this include-table file thin;
// see `tests.rs` module docs.

#[cfg(test)]
mod tests;

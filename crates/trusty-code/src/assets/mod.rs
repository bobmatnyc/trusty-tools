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
//! frontmatter (`.md`) as of #2897 Slice C (previously native TOML; Slice D
//! subsequently retired the TOML loader for USER `.claude/agents/*.toml`
//! configs entirely — see `agents::mod`'s docs). The embedded fallback
//! projects each `.md` string onto
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

// -- Slice E2 (#2958): embedded tm agent catalog, for `md_loader`'s in-memory
// extends-composer. NOT wired into `DEFAULT_AGENTS`/`load_all_agents`'s
// fallback yet -- that expansion is Slice E3. --

const BASE_AGENT_MD: &str = include_str!("agents/BASE-AGENT.md");
const BASE_ENGINEER_MD: &str = include_str!("agents/BASE-ENGINEER.md");
const BASE_OPS_MD: &str = include_str!("agents/BASE-OPS.md");
const BASE_QA_MD: &str = include_str!("agents/BASE-QA.md");
const BASE_RESEARCH_MD: &str = include_str!("agents/BASE-RESEARCH.md");

const API_QA_MD: &str = include_str!("agents/api-qa.md");
const CODE_ANALYZER_MD: &str = include_str!("agents/code-analyzer.md");
const CODE_CRITIC_MD: &str = include_str!("agents/code-critic.md");
const DART_ENGINEER_MD: &str = include_str!("agents/dart-engineer.md");
const DATA_ENGINEER_MD: &str = include_str!("agents/data-engineer.md");
const DOCUMENTATION_MD: &str = include_str!("agents/documentation.md");
const GOLANG_ENGINEER_MD: &str = include_str!("agents/golang-engineer.md");
const JAVA_ENGINEER_MD: &str = include_str!("agents/java-engineer.md");
const JAVASCRIPT_ENGINEER_MD: &str = include_str!("agents/javascript-engineer.md");
const LOCAL_OPS_MD: &str = include_str!("agents/local-ops.md");
const NEXTJS_ENGINEER_MD: &str = include_str!("agents/nextjs-engineer.md");
const OPS_MD: &str = include_str!("agents/ops.md");
const PHOENIX_ENGINEER_MD: &str = include_str!("agents/phoenix-engineer.md");
const PHP_ENGINEER_MD: &str = include_str!("agents/php-engineer.md");
const PROMPT_ENGINEER_MD: &str = include_str!("agents/prompt-engineer.md");
const PYTHON_ENGINEER_MD: &str = include_str!("agents/python-engineer.md");
const QA_MD: &str = include_str!("agents/qa.md");
const REACT_ENGINEER_MD: &str = include_str!("agents/react-engineer.md");
const REFACTORING_ENGINEER_MD: &str = include_str!("agents/refactoring-engineer.md");
const RESEARCH_MD: &str = include_str!("agents/research.md");
const RUBY_ENGINEER_MD: &str = include_str!("agents/ruby-engineer.md");
const RUST_ENGINEER_MD: &str = include_str!("agents/rust-engineer.md");
const SECURITY_MD: &str = include_str!("agents/security.md");
const SVELTE_ENGINEER_MD: &str = include_str!("agents/svelte-engineer.md");
const TAURI_ENGINEER_MD: &str = include_str!("agents/tauri-engineer.md");
const TYPESCRIPT_ENGINEER_MD: &str = include_str!("agents/typescript-engineer.md");
const WEB_QA_MD: &str = include_str!("agents/web-qa.md");
const WEB_UI_ENGINEER_MD: &str = include_str!("agents/web-ui-engineer.md");

/// The embedded tm agent catalog's raw sources (Slice E2, #2958): the 5
/// `BASE-*` extends templates plus the 28 coding-relevant roster agents Bob
/// selected in #2958 (mpm/memory/cloud-vendor agents excluded -- see the
/// issue's roster decision). Keyed by each asset's ORIGINAL embedded
/// filename (`"BASE-QA.md"`, `"rust-engineer.md"`, ...) rather than a
/// pre-lowercased bare name, because
/// `trusty_agents_common::agents::builder_in_memory::InMemorySources::insert`
/// already lowercases and strips a trailing `.md` on both insert and lookup
/// (Slice E1, PR #3013) -- so `("BASE-QA.md", ...)` resolves an
/// `extends: base-qa` reference with no extra normalisation needed here.
///
/// Why: `agents::md_loader::project_embedded_md_with_extends` needs a single
/// batch source for `build_in_memory_source_map` instead of 33 individual
/// `insert` calls; this table is that source. Kept separate from
/// [`DEFAULT_AGENTS`] so Slice E3 (roster expansion into the dispatchable
/// fallback) can build on this table without disturbing the existing
/// 3-agent fallback this slice does not touch.
/// What: 33 `(original_filename, raw_md_content)` pairs. None of these are
/// dispatchable agents yet -- wiring them into `DEFAULT_AGENTS` /
/// `load_all_agents`'s fallback is deferred to Slice E3.
/// Test: `assets::tests::embedded_tm_agent_sources_has_33_entries_and_unique_keys`,
/// `md_loader::tests::project_embedded_md_with_extends_resolves_rust_engineer_from_base_engineer`.
pub const EMBEDDED_TM_AGENT_SOURCES: &[(&str, &str)] = &[
    ("BASE-AGENT.md", BASE_AGENT_MD),
    ("BASE-ENGINEER.md", BASE_ENGINEER_MD),
    ("BASE-OPS.md", BASE_OPS_MD),
    ("BASE-QA.md", BASE_QA_MD),
    ("BASE-RESEARCH.md", BASE_RESEARCH_MD),
    ("api-qa.md", API_QA_MD),
    ("code-analyzer.md", CODE_ANALYZER_MD),
    ("code-critic.md", CODE_CRITIC_MD),
    ("dart-engineer.md", DART_ENGINEER_MD),
    ("data-engineer.md", DATA_ENGINEER_MD),
    ("documentation.md", DOCUMENTATION_MD),
    ("golang-engineer.md", GOLANG_ENGINEER_MD),
    ("java-engineer.md", JAVA_ENGINEER_MD),
    ("javascript-engineer.md", JAVASCRIPT_ENGINEER_MD),
    ("local-ops.md", LOCAL_OPS_MD),
    ("nextjs-engineer.md", NEXTJS_ENGINEER_MD),
    ("ops.md", OPS_MD),
    ("phoenix-engineer.md", PHOENIX_ENGINEER_MD),
    ("php-engineer.md", PHP_ENGINEER_MD),
    ("prompt-engineer.md", PROMPT_ENGINEER_MD),
    ("python-engineer.md", PYTHON_ENGINEER_MD),
    ("qa.md", QA_MD),
    ("react-engineer.md", REACT_ENGINEER_MD),
    ("refactoring-engineer.md", REFACTORING_ENGINEER_MD),
    ("research.md", RESEARCH_MD),
    ("ruby-engineer.md", RUBY_ENGINEER_MD),
    ("rust-engineer.md", RUST_ENGINEER_MD),
    ("security.md", SECURITY_MD),
    ("svelte-engineer.md", SVELTE_ENGINEER_MD),
    ("tauri-engineer.md", TAURI_ENGINEER_MD),
    ("typescript-engineer.md", TYPESCRIPT_ENGINEER_MD),
    ("web-qa.md", WEB_QA_MD),
    ("web-ui-engineer.md", WEB_UI_ENGINEER_MD),
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

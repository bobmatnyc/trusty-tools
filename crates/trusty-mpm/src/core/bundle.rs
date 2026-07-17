//! Compile-time embedded framework artifacts.
//!
//! Why: `trusty-mpm install` must deploy a working set of default artifacts
//! (optimizer policy, framework instructions, user instruction stub,
//! placeholder agent/skill) without depending on files shipped alongside the
//! binary — embedding them at compile time keeps the installer a single
//! self-contained executable.
//! What: exposes each default artifact under `crates/trusty-mpm-core/assets/`
//! as a `pub const &str` via `include_str!`, plus a [`BundledArtifact`] table
//! describing the relative install path of each one.
//! Test: `cargo test -p trusty-mpm-core bundle` asserts every constant is
//! non-empty and that [`ALL`] enumerates each artifact exactly once.

/// Default token-optimizer policy installed to `hooks/optimizer.toml`.
pub const OPTIMIZER_TOML: &str = include_str!("../assets/hooks/optimizer.toml");

/// Default session-overseer policy installed to `hooks/overseer.toml`.
///
/// Overseer oversight is opt-in: the shipped policy has `enabled = false`, so
/// installing it is inert until an operator flips the flag.
pub const OVERSEER_TOML: &str = include_str!("../assets/hooks/overseer.toml");

/// Framework launch instructions installed to `instructions/INSTRUCTIONS.md`.
///
/// This is the framework-owned artifact: `trusty-mpm install` overwrites it on
/// every run so framework upgrades take effect.
pub const FRAMEWORK_INSTRUCTIONS: &str = include_str!("../assets/instructions/INSTRUCTIONS.md");

/// User-editable instruction stub installed to `instructions/CLAUDE.md`.
///
/// This is a one-time seed: the installer writes it only when absent so any
/// project-specific edits the user makes survive subsequent installs.
pub const CLAUDE_STUB: &str = include_str!("../assets/instructions/CLAUDE.md");

/// Base agent — the root of every trusty-mpm inheritance chain.
pub const BASE_AGENT: &str = include_str!("../assets/agents/BASE-AGENT.md");

/// Base engineer agent — foundation for all engineer agents.
pub const BASE_ENGINEER: &str = include_str!("../assets/agents/BASE-ENGINEER.md");

/// Base research agent — foundation for all research agents.
pub const BASE_RESEARCH: &str = include_str!("../assets/agents/BASE-RESEARCH.md");

/// Base QA agent — foundation for all QA agents.
pub const BASE_QA: &str = include_str!("../assets/agents/BASE-QA.md");

/// Base ops agent — foundation for all ops agents.
pub const BASE_OPS: &str = include_str!("../assets/agents/BASE-OPS.md");

/// Concrete general-purpose engineer agent (`extends: base-engineer`).
pub const ENGINEER_AGENT: &str = include_str!("../assets/agents/engineer.md");

/// Concrete QA agent (`extends: base-qa`).
pub const QA_AGENT: &str = include_str!("../assets/agents/qa.md");

/// Concrete research agent (`extends: base-research`).
pub const RESEARCH_AGENT: &str = include_str!("../assets/agents/research.md");

/// Concrete ops agent — local operations specialist (`extends: base-ops`).
pub const OPS_AGENT: &str = include_str!("../assets/agents/ops.md");

/// Concrete security agent (`extends: base-agent`).
pub const SECURITY_AGENT: &str = include_str!("../assets/agents/security.md");

/// Concrete documentation agent (`extends: base-agent`).
pub const DOCUMENTATION_AGENT: &str = include_str!("../assets/agents/documentation.md");

/// Concrete data-engineer agent (`extends: base-engineer`).
pub const DATA_ENGINEER_AGENT: &str = include_str!("../assets/agents/data-engineer.md");

/// Concrete version-control agent (`extends: base-ops`).
pub const VERSION_CONTROL_AGENT: &str = include_str!("../assets/agents/version-control.md");

/// Concrete ticketing agent (`extends: base-agent`).
pub const TICKETING_AGENT: &str = include_str!("../assets/agents/ticketing.md");

/// Concrete code-analyzer agent (`extends: base-research`).
pub const CODE_ANALYZER_AGENT: &str = include_str!("../assets/agents/code-analyzer.md");

/// Concrete python-engineer agent (`extends: base-engineer`).
pub const PYTHON_ENGINEER_AGENT: &str = include_str!("../assets/agents/python-engineer.md");

/// Concrete typescript-engineer agent (`extends: base-engineer`).
pub const TYPESCRIPT_ENGINEER_AGENT: &str = include_str!("../assets/agents/typescript-engineer.md");

/// Concrete golang-engineer agent (`extends: base-engineer`).
pub const GOLANG_ENGINEER_AGENT: &str = include_str!("../assets/agents/golang-engineer.md");

/// Concrete rust-engineer agent (`extends: base-engineer`).
pub const RUST_ENGINEER_AGENT: &str = include_str!("../assets/agents/rust-engineer.md");

/// Concrete java-engineer agent (`extends: base-engineer`).
pub const JAVA_ENGINEER_AGENT: &str = include_str!("../assets/agents/java-engineer.md");

/// Concrete php-engineer agent (`extends: base-engineer`).
pub const PHP_ENGINEER_AGENT: &str = include_str!("../assets/agents/php-engineer.md");

/// Concrete ruby-engineer agent (`extends: base-engineer`).
pub const RUBY_ENGINEER_AGENT: &str = include_str!("../assets/agents/ruby-engineer.md");

/// Concrete react-engineer agent (`extends: base-engineer`).
pub const REACT_ENGINEER_AGENT: &str = include_str!("../assets/agents/react-engineer.md");

/// Concrete nextjs-engineer agent (`extends: base-engineer`).
pub const NEXTJS_ENGINEER_AGENT: &str = include_str!("../assets/agents/nextjs-engineer.md");

/// Concrete svelte-engineer agent (`extends: base-engineer`).
pub const SVELTE_ENGINEER_AGENT: &str = include_str!("../assets/agents/svelte-engineer.md");

/// Concrete web-qa agent (`extends: base-qa`).
pub const WEB_QA_AGENT: &str = include_str!("../assets/agents/web-qa.md");

/// Concrete api-qa agent (`extends: base-qa`).
pub const API_QA_AGENT: &str = include_str!("../assets/agents/api-qa.md");

// --- Increment 3 agents ---

/// Concrete javascript-engineer agent (`extends: base-engineer`).
pub const JAVASCRIPT_ENGINEER_AGENT: &str = include_str!("../assets/agents/javascript-engineer.md");

/// Concrete phoenix-engineer agent — Elixir/Phoenix (`extends: base-engineer`).
pub const PHOENIX_ENGINEER_AGENT: &str = include_str!("../assets/agents/phoenix-engineer.md");

/// Concrete dart-engineer agent — Flutter/Dart (`extends: base-engineer`).
pub const DART_ENGINEER_AGENT: &str = include_str!("../assets/agents/dart-engineer.md");

/// Concrete dotnet-engineer agent — C#/.NET 8+ with VB.NET awareness (`extends: base-engineer`).
pub const DOTNET_ENGINEER_AGENT: &str = include_str!("../assets/agents/dotnet-engineer.md");

/// Concrete tauri-engineer agent (`extends: base-engineer`).
pub const TAURI_ENGINEER_AGENT: &str = include_str!("../assets/agents/tauri-engineer.md");

/// Concrete web-ui-engineer agent (`extends: base-engineer`).
pub const WEB_UI_ENGINEER_AGENT: &str = include_str!("../assets/agents/web-ui-engineer.md");

/// Concrete refactoring-engineer agent (`extends: base-engineer`).
pub const REFACTORING_ENGINEER_AGENT: &str =
    include_str!("../assets/agents/refactoring-engineer.md");

/// Concrete prompt-engineer agent (`extends: base-engineer`).
pub const PROMPT_ENGINEER_AGENT: &str = include_str!("../assets/agents/prompt-engineer.md");

/// Concrete code-critic agent — adversarial reviewer (`extends: base-qa`).
pub const CODE_CRITIC_AGENT: &str = include_str!("../assets/agents/code-critic.md");

/// `code-review-standards` skill — the full adversarial review rubric
/// (severity taxonomy, 80% confidence filter, verdict protocol) that
/// `code-critic` declares in its `skills:` frontmatter (issue #2890, DOC-42).
///
/// Why: DOC-28 (self-awareness incident) taught that a skill file existing
/// under `src/assets/skills/` is not sufficient for it to ship — it must also
/// be registered as a [`crate::core::bundle::BundledArtifact`] in [`ALL`] or
/// `deploy_all_skill_tiers` never sees it (the exact historical bug that left
/// `tm-doctor.md` orphaned; see `bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to
/// `skills/code-review-standards.md`.
/// Test: `bundle_table_is_complete`, `code_critic_declared_skills_are_in_bundle`.
pub const CODE_REVIEW_STANDARDS: &str = include_str!("../assets/skills/code-review-standards.md");

/// `contract-driven-testing` skill — the three-level test pyramid derived
/// from Code Contracts that `code-critic` declares in its `skills:`
/// frontmatter (issue #2890, DOC-42).
///
/// Why: see [`CODE_REVIEW_STANDARDS`] — registration in [`ALL`] is what
/// actually makes a bundled skill installable, not the presence of the file.
/// What: embedded markdown skill file deployed to
/// `skills/contract-driven-testing.md`.
/// Test: `bundle_table_is_complete`, `code_critic_declared_skills_are_in_bundle`.
pub const CONTRACT_DRIVEN_TESTING: &str =
    include_str!("../assets/skills/contract-driven-testing.md");

/// Concrete gcp-ops agent — Google Cloud Platform (`extends: base-ops`).
pub const GCP_OPS_AGENT: &str = include_str!("../assets/agents/gcp-ops.md");

/// Concrete vercel-ops agent (`extends: base-ops`).
pub const VERCEL_OPS_AGENT: &str = include_str!("../assets/agents/vercel-ops.md");

/// Concrete local-ops agent — local dev environment (`extends: base-ops`).
pub const LOCAL_OPS_AGENT: &str = include_str!("../assets/agents/local-ops.md");

/// Concrete memory-manager agent — trusty-memory MCP backend only (`extends: base-agent`).
pub const MEMORY_MANAGER_AGENT: &str = include_str!("../assets/agents/memory-manager.md");

/// Concrete mpm-agent-manager agent — bundled-asset catalog lifecycle (`extends: base-agent`).
pub const MPM_AGENT_MANAGER_AGENT: &str = include_str!("../assets/agents/mpm-agent-manager.md");

/// Concrete mpm-skills-manager agent — skill lifecycle and recommendations (`extends: base-agent`).
pub const MPM_SKILLS_MANAGER_AGENT: &str = include_str!("../assets/agents/mpm-skills-manager.md");

// --- Issue #2911: `documentation-style` bundled skill (SLD-grounded
// per-artifact-type documentation conventions) — own module, kept separate
// from the batch-1 groups since it lands independently of epic #2902.
#[path = "bundle_skills_documentation_style.rs"]
mod skills_documentation_style_inner;
pub use skills_documentation_style_inner::{
    DOCUMENTATION_STYLE, DOCUMENTATION_STYLE_BLOCK_INLINE, DOCUMENTATION_STYLE_CLASS,
    DOCUMENTATION_STYLE_FILE_LEVEL, DOCUMENTATION_STYLE_METHOD_FUNCTION,
    DOCUMENTATION_STYLE_README, DOCUMENTATION_STYLE_SPEC,
};

// --- Skill-port batch 1 (issue #2903, epic #2902): 25 upstream universal/
// skills, split across 4 modules by category to stay under the 500-SLOC cap.
#[path = "bundle_skills_batch1_debugging.rs"]
mod skills_batch1_debugging_inner;
pub use skills_batch1_debugging_inner::{
    CONDITION_BASED_WAITING, CONDITION_BASED_WAITING_PATTERNS_AND_IMPLEMENTATION,
    ROOT_CAUSE_TRACING, ROOT_CAUSE_TRACING_ADVANCED_TECHNIQUES, ROOT_CAUSE_TRACING_EXAMPLES,
    ROOT_CAUSE_TRACING_INTEGRATION, ROOT_CAUSE_TRACING_TRACING_TECHNIQUES, SYSTEMATIC_DEBUGGING,
    SYSTEMATIC_DEBUGGING_ANTI_PATTERNS, SYSTEMATIC_DEBUGGING_EXAMPLES,
    SYSTEMATIC_DEBUGGING_TROUBLESHOOTING, SYSTEMATIC_DEBUGGING_WORKFLOW, TEST_DRIVEN_DEVELOPMENT,
    TEST_DRIVEN_DEVELOPMENT_ANTI_PATTERNS, TEST_DRIVEN_DEVELOPMENT_EXAMPLES,
    TEST_DRIVEN_DEVELOPMENT_INTEGRATION, TEST_DRIVEN_DEVELOPMENT_PHILOSOPHY,
    TEST_DRIVEN_DEVELOPMENT_WORKFLOW, TEST_QUALITY_INSPECTOR,
    TEST_QUALITY_INSPECTOR_ASSERTION_QUALITY, TEST_QUALITY_INSPECTOR_INSPECTION_CHECKLIST,
    TEST_QUALITY_INSPECTOR_RED_FLAGS, TESTING_ANTI_PATTERNS,
    TESTING_ANTI_PATTERNS_COMPLETENESS_ANTI_PATTERNS, TESTING_ANTI_PATTERNS_CORE_ANTI_PATTERNS,
    TESTING_ANTI_PATTERNS_DETECTION_GUIDE, TESTING_ANTI_PATTERNS_PYTHON_EXAMPLES,
    TESTING_ANTI_PATTERNS_TDD_CONNECTION, VERIFICATION_BEFORE_COMPLETION,
    VERIFICATION_BEFORE_COMPLETION_GATE_FUNCTION,
    VERIFICATION_BEFORE_COMPLETION_INTEGRATION_AND_WORKFLOWS,
    VERIFICATION_BEFORE_COMPLETION_RED_FLAGS_AND_FAILURES,
    VERIFICATION_BEFORE_COMPLETION_VERIFICATION_PATTERNS, WEBAPP_TESTING,
};

#[path = "bundle_skills_batch1_collab.rs"]
mod skills_batch1_collab_inner;
pub use skills_batch1_collab_inner::{
    BRAINSTORMING, DATABASE_MIGRATION, DATABASE_MIGRATION_DECISION_TREES,
    DATABASE_MIGRATION_TROUBLESHOOTING, GIT_WORKFLOW, JSON_DATA_HANDLING, REQUESTING_CODE_REVIEW,
    REQUESTING_CODE_REVIEW_CODE_REVIEWER_TEMPLATE, REQUESTING_CODE_REVIEW_REVIEW_EXAMPLES,
    SOFTWARE_PATTERNS, SOFTWARE_PATTERNS_ANTI_PATTERNS, SOFTWARE_PATTERNS_CODE_SMELL_SIGNALS,
    SOFTWARE_PATTERNS_DECISION_TREES, SOFTWARE_PATTERNS_EXAMPLES,
    SOFTWARE_PATTERNS_FOUNDATIONAL_PATTERNS, SOFTWARE_PATTERNS_SITUATIONAL_PATTERNS, WRITING_PLANS,
    WRITING_PLANS_BEST_PRACTICES, WRITING_PLANS_PLAN_STRUCTURE_TEMPLATES,
};

#[path = "bundle_skills_batch1_security_web.rs"]
mod skills_batch1_security_web_inner;
pub use skills_batch1_security_web_inner::{
    API_DESIGN_PATTERNS, API_DESIGN_PATTERNS_AUTHENTICATION, API_DESIGN_PATTERNS_GRAPHQL_PATTERNS,
    API_DESIGN_PATTERNS_GRPC_PATTERNS, API_DESIGN_PATTERNS_REST_PATTERNS,
    API_DESIGN_PATTERNS_VERSIONING_STRATEGIES, API_DOCUMENTATION, ENV_MANAGER,
    ENV_MANAGER_FRAMEWORKS, ENV_MANAGER_SECURITY, ENV_MANAGER_SYNCHRONIZATION,
    ENV_MANAGER_TROUBLESHOOTING, ENV_MANAGER_VALIDATION, SECURITY_SCANNING,
    SECURITY_SCANNING_CI_WORKFLOWS, SECURITY_SCANNING_COMMON_FINDINGS_AND_FIXES,
    SECURITY_SCANNING_OPEN_SOURCE_SAFETY, SECURITY_SCANNING_SUPPLY_CHAIN_AND_SBOM,
    SECURITY_SCANNING_TOOLING_MATRIX, SECURITY_SCANNING_TRIAGE_AND_REMEDIATION,
    WEB_PERFORMANCE_OPTIMIZATION, WEB_PERFORMANCE_OPTIMIZATION_CORE_WEB_VITALS,
    WEB_PERFORMANCE_OPTIMIZATION_FRAMEWORK_SPECIFIC,
    WEB_PERFORMANCE_OPTIMIZATION_MODERN_PATTERNS_2025, WEB_PERFORMANCE_OPTIMIZATION_MONITORING,
    WEB_PERFORMANCE_OPTIMIZATION_OPTIMIZATION_TECHNIQUES, WEB_PERFORMANCE_OPTIMIZATION_QUICK_WINS,
};

#[path = "bundle_skills_batch1_process.rs"]
mod skills_batch1_process_inner;
pub use skills_batch1_process_inner::{
    ARTIFACTS_BUILDER, CODE_PRODUCTION_PROCESS, CODE_PRODUCTION_PROCESS_CRITIC_ISOLATION,
    CODE_PRODUCTION_PROCESS_SKIP_RULES, CODE_PRODUCTION_PROCESS_STAGE_ARCHITECT,
    CODE_PRODUCTION_PROCESS_STAGE_CRITIC, CODE_PRODUCTION_PROCESS_STAGE_IMPLEMENT,
    CODE_PRODUCTION_PROCESS_STAGE_RESEARCH, CODE_PRODUCTION_PROCESS_STAGE_SECURITY,
    CODE_PRODUCTION_PROCESS_STAGE_TESTS, INTERNAL_COMMS, MODEL_CONTEXT_BUILDER, XLSX,
};

/// Canonical self-description doc installed to `docs/WHAT-IS-TRUSTY-MPM.md`
/// (DOC-28 R1 — `docs/specs/trusty-mpm-self-awareness.md`).
///
/// Why: no single, stable, canonically-pointed-at answer to "what is
/// trusty-mpm" existed anywhere in the bundled assets, which let a launched
/// session conflate this Rust project with the unrelated Python `claude-mpm`
/// package (the "self-awareness incident"). Source lives under
/// `crates/trusty-mpm/docs/` (ordinary, human-reviewable documentation)
/// rather than `src/assets/` so it reads naturally as a doc while still being
/// embeddable here, exactly like every other bundled artifact.
pub const WHAT_IS_TRUSTY_MPM: &str = include_str!("../../docs/WHAT-IS-TRUSTY-MPM.md");

/// Architecture overview doc installed to `docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md`
/// (issue #2034 — bundled PM-facing reference for shipped design patterns).
///
/// Why: three foundational behaviours (memory over MCP, session↔worktree 1:1
/// model, per-worktree search-index) are now shipped on main but not
/// documented for PM audiences. This doc summarizes each pattern, its
/// trade-offs, and its integration with the others, then cross-references
/// the deeper specs and source code so operators and framework users can
/// understand the design from first principles.
pub const ARCHITECTURE_MEMORY_SESSIONS_SEARCH: &str =
    include_str!("../../docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md");

// --- tm-skills-portfolio epic: the /tm- skill catalog — bundle_tm_skills.rs ---
// Supersedes the Phase 1 (#770) mpm-* guidance skills (formerly
// bundle_skills.rs), which were a mechanical port of claude-mpm's Python
// skill catalog with unadapted tool/path references. Removed entirely.
#[path = "bundle_tm_skills.rs"]
mod tm_skills_inner;
pub use tm_skills_inner::{
    TM_ADR, TM_AGENT_ARCHITECTURE, TM_BUG_REPORTING, TM_CIRCUIT_BREAKER, TM_CLI_OPERATIONS,
    TM_DELEGATION_PATTERNS, TM_DOCTOR, TM_GIT_FILE_TRACKING, TM_INIT, TM_ISSUES_PRUNE, TM_OVERVIEW,
    TM_POSTMORTEM, TM_PR_WORKFLOW, TM_SESSION_MANAGEMENT, TM_SESSION_PAUSE, TM_SESSION_RESUME,
    TM_TEACHING_TEMPLATES, TM_TICKETING, TM_TOOL_USAGE_GUIDE, TM_VERIFICATION_PROTOCOLS,
    TM_WORKFLOW,
};

/// Default (professional) Claude Code output style deployed to
/// `~/.claude/output-styles/trusty-mpm.md`.
///
/// Why: launched sessions set `"outputStyle": "trusty-mpm"` in the project
/// `.claude/settings.json`; Claude Code only honours that name if a matching
/// style file exists. Bundling it lets the launch path deploy it directly,
/// outside the framework-root [`ALL`] table (which installs under
/// `~/.trusty-mpm/framework/`, not `~/.claude/`).
pub const OUTPUT_STYLE: &str = include_str!("../assets/output-styles/trusty-mpm.md");

/// Teaching-mode Claude Code output style (id `trusty-mpm-teacher`).
///
/// Why: HR-4 bundles three styles so an operator can pick a communication mode;
/// this variant narrates the orchestration reasoning while keeping the
/// mandatory-delegation floor intact.
pub const OUTPUT_STYLE_TEACHER: &str =
    include_str!("../assets/output-styles/trusty-mpm-teacher.md");

/// Research-mode Claude Code output style (id `trusty-mpm-research`).
///
/// Why: HR-4 bundles three styles; this variant front-loads investigation and
/// demands evidence before any change, again keeping the delegation floor.
pub const OUTPUT_STYLE_RESEARCH: &str =
    include_str!("../assets/output-styles/trusty-mpm-research.md");

/// The default output-style id used when none is configured/selected.
///
/// Why: callers (config resolution, settings writer) need a single source of
/// truth for the professional default so they cannot drift.
/// What: the frontmatter `name:` of [`OUTPUT_STYLE`].
/// Test: `bundle_tests::output_style_registry_default_resolves`.
pub const DEFAULT_OUTPUT_STYLE_ID: &str = "trusty-mpm";

/// One entry in the bundled output-style registry.
///
/// Why: the multi-style launch path (HR-4) must map a configured/selected style
/// id to its bundled content and the file name it deploys to; bundling the id,
/// file name, and content together keeps those three facts from drifting.
/// What: the style id (matching the file's frontmatter `name:`), the file name
/// written under `~/.claude/output-styles/`, and the embedded content.
/// Test: `bundle_tests::output_style_registry_ids_match_frontmatter`.
#[derive(Debug, Clone, Copy)]
pub struct BundledStyle {
    /// Style id — matches the frontmatter `name:` and the `outputStyle` settings
    /// key Claude Code resolves against `~/.claude/output-styles/<file_name>`.
    pub id: &'static str,
    /// File name written under `~/.claude/output-styles/`.
    pub file_name: &'static str,
    /// Embedded Markdown content of the style.
    pub content: &'static str,
}

/// All bundled output styles, default first.
///
/// Why: `deploy_output_style` writes every entry, and the style resolver looks
/// up a configured/selected id against this table; a single ordered slice keeps
/// both behaviours consistent.
/// What: the professional (`trusty-mpm`), teaching (`trusty-mpm-teacher`), and
/// research (`trusty-mpm-research`) styles.
/// Test: `bundle_tests::output_style_registry_has_three_distinct_ids`.
pub const OUTPUT_STYLES: &[BundledStyle] = &[
    BundledStyle {
        id: DEFAULT_OUTPUT_STYLE_ID,
        file_name: "trusty-mpm.md",
        content: OUTPUT_STYLE,
    },
    BundledStyle {
        id: "trusty-mpm-teacher",
        file_name: "trusty-mpm-teacher.md",
        content: OUTPUT_STYLE_TEACHER,
    },
    BundledStyle {
        id: "trusty-mpm-research",
        file_name: "trusty-mpm-research.md",
        content: OUTPUT_STYLE_RESEARCH,
    },
];

// BundledArtifact, InstallPolicy, and ALL are defined in bundle_all.rs.
// They are included here so they can access the constants above via `use super::*`.
#[path = "bundle_all.rs"]
mod all_inner;
pub use all_inner::{ALL, BundledArtifact, InstallPolicy};

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

//! [`BundledArtifact`], [`InstallPolicy`], and the canonical [`ALL`] table.
//!
//! Why: splitting the artifact table out of `bundle.rs` keeps that file under
//! the 500-line cap as the skill and agent catalogs grow.
//! What: defines the two public types used by the installer and the static
//! slice enumerating every artifact in install order.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`.

use super::*;

/// One embedded framework artifact and its install location.
///
/// Why: the installer iterates a single table rather than hard-coding each
/// write, so adding a bundled artifact is a one-line change here.
/// What: a relative path (under `~/.trusty-mpm/framework/`) and the embedded
/// file contents.
/// Test: `bundle_table_is_complete`.
#[derive(Debug, Clone, Copy)]
pub struct BundledArtifact {
    /// Path relative to the framework root (e.g. `hooks/optimizer.toml`).
    pub rel_path: &'static str,
    /// Embedded file contents.
    pub contents: &'static str,
    /// Install policy: how the installer treats a pre-existing file.
    pub install: InstallPolicy,
}

/// How the installer writes a [`BundledArtifact`] when the target already exists.
///
/// Why: framework-owned files (instructions, policy) must track upgrades, but
/// user-editable stubs must not be clobbered — one enum makes the distinction
/// explicit and data-driven. Issue #3374 removed the last user-owned artifact
/// (the `CLAUDE.md` stub) and, with it, the `SeedOnce` variant; issue #3381
/// restores it because `install_to()` had never actually branched on the
/// enum at all — every entry used `Overwrite` so the single-variant enum was
/// silently equivalent to no policy. The two-way distinction is required
/// again the moment any bundled artifact is meant to be user-owned.
/// What: [`Overwrite`](InstallPolicy::Overwrite) always writes the embedded
/// contents; [`SeedOnce`](InstallPolicy::SeedOnce) writes only when absent.
/// Test: `framework_instructions_overwrites`,
/// `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPolicy {
    /// Always write the embedded contents, replacing any existing file.
    Overwrite,
    /// Write the embedded contents only if the target file does not exist.
    /// `tm install --force` is the escape hatch: it resets a user-owned file
    /// back to the shipped default.
    SeedOnce,
}

/// Build an [`InstallPolicy::Overwrite`] artifact entry.
///
/// Why: nearly every bundled artifact is framework-owned and must track
/// upgrades. Spelling out the 5-line [`BundledArtifact`] struct literal for
/// all ~164 entries would blow the 500-SLOC production cap (issue #2903
/// alone adds 93 skill-port entries); this shorthand collapses the common
/// case to one line per entry so [`ALL`] stays a single, cap-compliant table
/// instead of being split across files (which Rust cannot do for one array
/// literal without unsafe const-array concatenation or forbidden
/// global/lazy state).
/// What: returns a [`BundledArtifact`] with `install: InstallPolicy::Overwrite`.
/// Test: `bundle_table_is_complete` (exercises every entry `ALL` produces,
/// including those built by this helper).
const fn overwrite(rel_path: &'static str, contents: &'static str) -> BundledArtifact {
    BundledArtifact {
        rel_path,
        contents,
        install: InstallPolicy::Overwrite,
    }
}

/// Build an [`InstallPolicy::SeedOnce`] artifact entry.
///
/// Why: mirrors [`overwrite`] for the user-owned case, so a genuinely
/// user-editable bundled artifact reads as a one-line table entry rather than
/// a hand-rolled struct literal. No entry in [`ALL`] currently uses it (issue
/// #3381 restores the enum shape without shipping a fake bundled entry just
/// to exercise it) — `pub(super)` so `bundle_tests.rs` can build a fixture
/// artifact with it instead.
/// What: returns a [`BundledArtifact`] with `install: InstallPolicy::SeedOnce`.
/// Test: `seed_once_constructor_builds_the_expected_artifact`,
/// `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const fn seed_once(rel_path: &'static str, contents: &'static str) -> BundledArtifact {
    BundledArtifact {
        rel_path,
        contents,
        install: InstallPolicy::SeedOnce,
    }
}

/// Every bundled framework artifact, in install order.
///
/// Why: gives the installer (and tests) one canonical list to walk.
/// What: optimizer policy, framework instructions, agent catalog, placeholder
/// skill, and Phase 1 (#770) mpm-* guidance skills.
/// Test: `bundle_table_is_complete`.
pub const ALL: &[BundledArtifact] = &[
    overwrite("hooks/optimizer.toml", OPTIMIZER_TOML),
    overwrite("hooks/overseer.toml", OVERSEER_TOML),
    overwrite("instructions/INSTRUCTIONS.md", FRAMEWORK_INSTRUCTIONS),
    overwrite("agents/BASE-AGENT.md", BASE_AGENT),
    overwrite("agents/BASE-ENGINEER.md", BASE_ENGINEER),
    overwrite("agents/BASE-RESEARCH.md", BASE_RESEARCH),
    overwrite("agents/BASE-QA.md", BASE_QA),
    overwrite("agents/BASE-OPS.md", BASE_OPS),
    overwrite("agents/engineer.md", ENGINEER_AGENT),
    overwrite("agents/qa.md", QA_AGENT),
    overwrite("agents/research.md", RESEARCH_AGENT),
    overwrite("agents/ops.md", OPS_AGENT),
    overwrite("agents/security.md", SECURITY_AGENT),
    overwrite("agents/documentation.md", DOCUMENTATION_AGENT),
    overwrite("agents/data-engineer.md", DATA_ENGINEER_AGENT),
    overwrite("agents/version-control.md", VERSION_CONTROL_AGENT),
    overwrite("agents/ticketing.md", TICKETING_AGENT),
    overwrite("agents/code-analyzer.md", CODE_ANALYZER_AGENT),
    overwrite("agents/python-engineer.md", PYTHON_ENGINEER_AGENT),
    overwrite("agents/typescript-engineer.md", TYPESCRIPT_ENGINEER_AGENT),
    overwrite("agents/golang-engineer.md", GOLANG_ENGINEER_AGENT),
    overwrite("agents/rust-engineer.md", RUST_ENGINEER_AGENT),
    overwrite("agents/java-engineer.md", JAVA_ENGINEER_AGENT),
    overwrite("agents/php-engineer.md", PHP_ENGINEER_AGENT),
    overwrite("agents/ruby-engineer.md", RUBY_ENGINEER_AGENT),
    overwrite("agents/react-engineer.md", REACT_ENGINEER_AGENT),
    overwrite("agents/nextjs-engineer.md", NEXTJS_ENGINEER_AGENT),
    overwrite("agents/svelte-engineer.md", SVELTE_ENGINEER_AGENT),
    overwrite("agents/web-qa.md", WEB_QA_AGENT),
    overwrite("agents/api-qa.md", API_QA_AGENT),
    // --- Increment 3: remaining 14 agents ---
    overwrite("agents/javascript-engineer.md", JAVASCRIPT_ENGINEER_AGENT),
    overwrite("agents/phoenix-engineer.md", PHOENIX_ENGINEER_AGENT),
    overwrite("agents/dart-engineer.md", DART_ENGINEER_AGENT),
    overwrite("agents/dotnet-engineer.md", DOTNET_ENGINEER_AGENT),
    overwrite("agents/tauri-engineer.md", TAURI_ENGINEER_AGENT),
    overwrite("agents/web-ui-engineer.md", WEB_UI_ENGINEER_AGENT),
    overwrite("agents/refactoring-engineer.md", REFACTORING_ENGINEER_AGENT),
    overwrite("agents/prompt-engineer.md", PROMPT_ENGINEER_AGENT),
    overwrite("agents/code-critic.md", CODE_CRITIC_AGENT),
    // --- Issue #2890: code-critic's declared `skills:` dependencies ---
    overwrite("skills/code-review-standards.md", CODE_REVIEW_STANDARDS),
    overwrite("skills/contract-driven-testing.md", CONTRACT_DRIVEN_TESTING),
    overwrite("agents/gcp-ops.md", GCP_OPS_AGENT),
    overwrite("agents/vercel-ops.md", VERCEL_OPS_AGENT),
    overwrite("agents/local-ops.md", LOCAL_OPS_AGENT),
    overwrite("agents/memory-manager.md", MEMORY_MANAGER_AGENT),
    overwrite("agents/mpm-agent-manager.md", MPM_AGENT_MANAGER_AGENT),
    overwrite("agents/mpm-skills-manager.md", MPM_SKILLS_MANAGER_AGENT),
    // --- A3 (tm-skills-portfolio epic): previously orphaned tm-doctor.md ---
    overwrite("skills/tm-doctor.md", TM_DOCTOR),
    // --- tm-skills-portfolio epic: the /tm- skill catalog (supersedes mpm-*) ---
    overwrite("skills/tm-circuit-breaker.md", TM_CIRCUIT_BREAKER),
    overwrite(
        "skills/tm-verification-protocols.md",
        TM_VERIFICATION_PROTOCOLS,
    ),
    overwrite("skills/tm-tool-usage-guide.md", TM_TOOL_USAGE_GUIDE),
    overwrite("skills/tm-git-file-tracking.md", TM_GIT_FILE_TRACKING),
    overwrite("skills/tm-adr.md", TM_ADR),
    overwrite("skills/tm-workflow.md", TM_WORKFLOW),
    overwrite("skills/tm-agent-architecture.md", TM_AGENT_ARCHITECTURE),
    overwrite("skills/tm-postmortem.md", TM_POSTMORTEM),
    overwrite("skills/tm-bug-reporting.md", TM_BUG_REPORTING),
    overwrite("skills/tm-teaching-templates.md", TM_TEACHING_TEMPLATES),
    overwrite("skills/tm-ticketing.md", TM_TICKETING),
    overwrite("skills/tm-pr-workflow.md", TM_PR_WORKFLOW),
    overwrite("skills/tm-delegation-patterns.md", TM_DELEGATION_PATTERNS),
    overwrite("skills/tm-session-management.md", TM_SESSION_MANAGEMENT),
    overwrite("skills/tm-session-pause.md", TM_SESSION_PAUSE),
    overwrite("skills/tm-session-resume.md", TM_SESSION_RESUME),
    overwrite("skills/tm-init.md", TM_INIT),
    overwrite("skills/tm.md", TM_OVERVIEW),
    // --- Issue #2185: gh issue backlog prune/prioritize PM delegation skill ---
    overwrite("skills/tm-issues-prune.md", TM_ISSUES_PRUNE),
    // --- Issue #2321: tm CLI operations skill (MCP mgmt, sessions, diagnostics) ---
    overwrite("skills/tm-cli-operations.md", TM_CLI_OPERATIONS),
    // --- DOC-28 R1: canonical self-description doc ---
    overwrite("docs/WHAT-IS-TRUSTY-MPM.md", WHAT_IS_TRUSTY_MPM),
    // --- Issue #2034: architecture doc covering memory/sessions/search ---
    overwrite(
        "docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md",
        ARCHITECTURE_MEMORY_SESSIONS_SEARCH,
    ),
    // --- Issue #2903 (skill-port batch 1, epic #2902): 25 upstream
    // universal/ skills (entry SKILL.md + references/*.md), resolved via
    // the alias table so DOC-42 agent skills: declarations resolve. Uses
    // the overwrite() shorthand — every entry here is framework-owned.
    overwrite("skills/systematic-debugging.md", SYSTEMATIC_DEBUGGING),
    overwrite(
        "skills/systematic-debugging/references/anti-patterns.md",
        SYSTEMATIC_DEBUGGING_ANTI_PATTERNS,
    ),
    overwrite(
        "skills/systematic-debugging/references/examples.md",
        SYSTEMATIC_DEBUGGING_EXAMPLES,
    ),
    overwrite(
        "skills/systematic-debugging/references/troubleshooting.md",
        SYSTEMATIC_DEBUGGING_TROUBLESHOOTING,
    ),
    overwrite(
        "skills/systematic-debugging/references/workflow.md",
        SYSTEMATIC_DEBUGGING_WORKFLOW,
    ),
    overwrite(
        "skills/verification-before-completion.md",
        VERIFICATION_BEFORE_COMPLETION,
    ),
    overwrite(
        "skills/verification-before-completion/references/gate-function.md",
        VERIFICATION_BEFORE_COMPLETION_GATE_FUNCTION,
    ),
    overwrite(
        "skills/verification-before-completion/references/integration-and-workflows.md",
        VERIFICATION_BEFORE_COMPLETION_INTEGRATION_AND_WORKFLOWS,
    ),
    overwrite(
        "skills/verification-before-completion/references/red-flags-and-failures.md",
        VERIFICATION_BEFORE_COMPLETION_RED_FLAGS_AND_FAILURES,
    ),
    overwrite(
        "skills/verification-before-completion/references/verification-patterns.md",
        VERIFICATION_BEFORE_COMPLETION_VERIFICATION_PATTERNS,
    ),
    overwrite("skills/root-cause-tracing.md", ROOT_CAUSE_TRACING),
    overwrite(
        "skills/root-cause-tracing/references/advanced-techniques.md",
        ROOT_CAUSE_TRACING_ADVANCED_TECHNIQUES,
    ),
    overwrite(
        "skills/root-cause-tracing/references/examples.md",
        ROOT_CAUSE_TRACING_EXAMPLES,
    ),
    overwrite(
        "skills/root-cause-tracing/references/integration.md",
        ROOT_CAUSE_TRACING_INTEGRATION,
    ),
    overwrite(
        "skills/root-cause-tracing/references/tracing-techniques.md",
        ROOT_CAUSE_TRACING_TRACING_TECHNIQUES,
    ),
    overwrite("skills/test-driven-development.md", TEST_DRIVEN_DEVELOPMENT),
    overwrite(
        "skills/test-driven-development/references/anti-patterns.md",
        TEST_DRIVEN_DEVELOPMENT_ANTI_PATTERNS,
    ),
    overwrite(
        "skills/test-driven-development/references/examples.md",
        TEST_DRIVEN_DEVELOPMENT_EXAMPLES,
    ),
    overwrite(
        "skills/test-driven-development/references/integration.md",
        TEST_DRIVEN_DEVELOPMENT_INTEGRATION,
    ),
    overwrite(
        "skills/test-driven-development/references/philosophy.md",
        TEST_DRIVEN_DEVELOPMENT_PHILOSOPHY,
    ),
    overwrite(
        "skills/test-driven-development/references/workflow.md",
        TEST_DRIVEN_DEVELOPMENT_WORKFLOW,
    ),
    overwrite("skills/condition-based-waiting.md", CONDITION_BASED_WAITING),
    overwrite(
        "skills/condition-based-waiting/references/patterns-and-implementation.md",
        CONDITION_BASED_WAITING_PATTERNS_AND_IMPLEMENTATION,
    ),
    overwrite("skills/test-quality-inspector.md", TEST_QUALITY_INSPECTOR),
    overwrite(
        "skills/test-quality-inspector/references/assertion-quality.md",
        TEST_QUALITY_INSPECTOR_ASSERTION_QUALITY,
    ),
    overwrite(
        "skills/test-quality-inspector/references/inspection-checklist.md",
        TEST_QUALITY_INSPECTOR_INSPECTION_CHECKLIST,
    ),
    overwrite(
        "skills/test-quality-inspector/references/red-flags.md",
        TEST_QUALITY_INSPECTOR_RED_FLAGS,
    ),
    overwrite("skills/testing-anti-patterns.md", TESTING_ANTI_PATTERNS),
    overwrite(
        "skills/testing-anti-patterns/references/completeness-anti-patterns.md",
        TESTING_ANTI_PATTERNS_COMPLETENESS_ANTI_PATTERNS,
    ),
    overwrite(
        "skills/testing-anti-patterns/references/core-anti-patterns.md",
        TESTING_ANTI_PATTERNS_CORE_ANTI_PATTERNS,
    ),
    overwrite(
        "skills/testing-anti-patterns/references/detection-guide.md",
        TESTING_ANTI_PATTERNS_DETECTION_GUIDE,
    ),
    overwrite(
        "skills/testing-anti-patterns/references/python-examples.md",
        TESTING_ANTI_PATTERNS_PYTHON_EXAMPLES,
    ),
    overwrite(
        "skills/testing-anti-patterns/references/tdd-connection.md",
        TESTING_ANTI_PATTERNS_TDD_CONNECTION,
    ),
    overwrite("skills/webapp-testing.md", WEBAPP_TESTING),
    overwrite("skills/git-workflow.md", GIT_WORKFLOW),
    overwrite("skills/requesting-code-review.md", REQUESTING_CODE_REVIEW),
    overwrite(
        "skills/requesting-code-review/references/code-reviewer-template.md",
        REQUESTING_CODE_REVIEW_CODE_REVIEWER_TEMPLATE,
    ),
    overwrite(
        "skills/requesting-code-review/references/review-examples.md",
        REQUESTING_CODE_REVIEW_REVIEW_EXAMPLES,
    ),
    overwrite("skills/writing-plans.md", WRITING_PLANS),
    overwrite(
        "skills/writing-plans/references/best-practices.md",
        WRITING_PLANS_BEST_PRACTICES,
    ),
    overwrite(
        "skills/writing-plans/references/plan-structure-templates.md",
        WRITING_PLANS_PLAN_STRUCTURE_TEMPLATES,
    ),
    overwrite("skills/brainstorming.md", BRAINSTORMING),
    overwrite("skills/json-data-handling.md", JSON_DATA_HANDLING),
    overwrite("skills/database-migration.md", DATABASE_MIGRATION),
    overwrite(
        "skills/database-migration/references/decision-trees.md",
        DATABASE_MIGRATION_DECISION_TREES,
    ),
    overwrite(
        "skills/database-migration/references/troubleshooting.md",
        DATABASE_MIGRATION_TROUBLESHOOTING,
    ),
    overwrite("skills/software-patterns.md", SOFTWARE_PATTERNS),
    overwrite(
        "skills/software-patterns/references/anti-patterns.md",
        SOFTWARE_PATTERNS_ANTI_PATTERNS,
    ),
    overwrite(
        "skills/software-patterns/references/code-smell-signals.md",
        SOFTWARE_PATTERNS_CODE_SMELL_SIGNALS,
    ),
    overwrite(
        "skills/software-patterns/references/decision-trees.md",
        SOFTWARE_PATTERNS_DECISION_TREES,
    ),
    overwrite(
        "skills/software-patterns/references/examples.md",
        SOFTWARE_PATTERNS_EXAMPLES,
    ),
    overwrite(
        "skills/software-patterns/references/foundational-patterns.md",
        SOFTWARE_PATTERNS_FOUNDATIONAL_PATTERNS,
    ),
    overwrite(
        "skills/software-patterns/references/situational-patterns.md",
        SOFTWARE_PATTERNS_SITUATIONAL_PATTERNS,
    ),
    overwrite("skills/security-scanning.md", SECURITY_SCANNING),
    overwrite(
        "skills/security-scanning/references/ci-workflows.md",
        SECURITY_SCANNING_CI_WORKFLOWS,
    ),
    overwrite(
        "skills/security-scanning/references/common-findings-and-fixes.md",
        SECURITY_SCANNING_COMMON_FINDINGS_AND_FIXES,
    ),
    overwrite(
        "skills/security-scanning/references/open-source-safety.md",
        SECURITY_SCANNING_OPEN_SOURCE_SAFETY,
    ),
    overwrite(
        "skills/security-scanning/references/supply-chain-and-sbom.md",
        SECURITY_SCANNING_SUPPLY_CHAIN_AND_SBOM,
    ),
    overwrite(
        "skills/security-scanning/references/tooling-matrix.md",
        SECURITY_SCANNING_TOOLING_MATRIX,
    ),
    overwrite(
        "skills/security-scanning/references/triage-and-remediation.md",
        SECURITY_SCANNING_TRIAGE_AND_REMEDIATION,
    ),
    overwrite("skills/api-design-patterns.md", API_DESIGN_PATTERNS),
    overwrite(
        "skills/api-design-patterns/references/authentication.md",
        API_DESIGN_PATTERNS_AUTHENTICATION,
    ),
    overwrite(
        "skills/api-design-patterns/references/graphql-patterns.md",
        API_DESIGN_PATTERNS_GRAPHQL_PATTERNS,
    ),
    overwrite(
        "skills/api-design-patterns/references/grpc-patterns.md",
        API_DESIGN_PATTERNS_GRPC_PATTERNS,
    ),
    overwrite(
        "skills/api-design-patterns/references/rest-patterns.md",
        API_DESIGN_PATTERNS_REST_PATTERNS,
    ),
    overwrite(
        "skills/api-design-patterns/references/versioning-strategies.md",
        API_DESIGN_PATTERNS_VERSIONING_STRATEGIES,
    ),
    overwrite(
        "skills/web-performance-optimization.md",
        WEB_PERFORMANCE_OPTIMIZATION,
    ),
    overwrite(
        "skills/web-performance-optimization/references/core-web-vitals.md",
        WEB_PERFORMANCE_OPTIMIZATION_CORE_WEB_VITALS,
    ),
    overwrite(
        "skills/web-performance-optimization/references/framework-specific.md",
        WEB_PERFORMANCE_OPTIMIZATION_FRAMEWORK_SPECIFIC,
    ),
    overwrite(
        "skills/web-performance-optimization/references/modern-patterns-2025.md",
        WEB_PERFORMANCE_OPTIMIZATION_MODERN_PATTERNS_2025,
    ),
    overwrite(
        "skills/web-performance-optimization/references/monitoring.md",
        WEB_PERFORMANCE_OPTIMIZATION_MONITORING,
    ),
    overwrite(
        "skills/web-performance-optimization/references/optimization-techniques.md",
        WEB_PERFORMANCE_OPTIMIZATION_OPTIMIZATION_TECHNIQUES,
    ),
    overwrite(
        "skills/web-performance-optimization/references/quick-wins.md",
        WEB_PERFORMANCE_OPTIMIZATION_QUICK_WINS,
    ),
    overwrite("skills/api-documentation.md", API_DOCUMENTATION),
    overwrite("skills/env-manager.md", ENV_MANAGER),
    overwrite(
        "skills/env-manager/references/frameworks.md",
        ENV_MANAGER_FRAMEWORKS,
    ),
    overwrite(
        "skills/env-manager/references/security.md",
        ENV_MANAGER_SECURITY,
    ),
    overwrite(
        "skills/env-manager/references/synchronization.md",
        ENV_MANAGER_SYNCHRONIZATION,
    ),
    overwrite(
        "skills/env-manager/references/troubleshooting.md",
        ENV_MANAGER_TROUBLESHOOTING,
    ),
    overwrite(
        "skills/env-manager/references/validation.md",
        ENV_MANAGER_VALIDATION,
    ),
    overwrite("skills/code-production-process.md", CODE_PRODUCTION_PROCESS),
    overwrite(
        "skills/code-production-process/references/critic-isolation.md",
        CODE_PRODUCTION_PROCESS_CRITIC_ISOLATION,
    ),
    overwrite(
        "skills/code-production-process/references/skip-rules.md",
        CODE_PRODUCTION_PROCESS_SKIP_RULES,
    ),
    overwrite(
        "skills/code-production-process/references/stage-architect.md",
        CODE_PRODUCTION_PROCESS_STAGE_ARCHITECT,
    ),
    overwrite(
        "skills/code-production-process/references/stage-critic.md",
        CODE_PRODUCTION_PROCESS_STAGE_CRITIC,
    ),
    overwrite(
        "skills/code-production-process/references/stage-implement.md",
        CODE_PRODUCTION_PROCESS_STAGE_IMPLEMENT,
    ),
    overwrite(
        "skills/code-production-process/references/stage-research.md",
        CODE_PRODUCTION_PROCESS_STAGE_RESEARCH,
    ),
    overwrite(
        "skills/code-production-process/references/stage-security.md",
        CODE_PRODUCTION_PROCESS_STAGE_SECURITY,
    ),
    overwrite(
        "skills/code-production-process/references/stage-tests.md",
        CODE_PRODUCTION_PROCESS_STAGE_TESTS,
    ),
    overwrite("skills/internal-comms.md", INTERNAL_COMMS),
    overwrite("skills/artifacts-builder.md", ARTIFACTS_BUILDER),
    overwrite("skills/model-context-builder.md", MODEL_CONTEXT_BUILDER),
    overwrite("skills/xlsx.md", XLSX),
    // --- BEGIN issue #2911: documentation-style bundled skill (append-only) ---
    overwrite("skills/documentation-style.md", DOCUMENTATION_STYLE),
    overwrite(
        "skills/documentation-style/references/spec.md",
        DOCUMENTATION_STYLE_SPEC,
    ),
    overwrite(
        "skills/documentation-style/references/readme.md",
        DOCUMENTATION_STYLE_README,
    ),
    overwrite(
        "skills/documentation-style/references/file-level.md",
        DOCUMENTATION_STYLE_FILE_LEVEL,
    ),
    overwrite(
        "skills/documentation-style/references/class.md",
        DOCUMENTATION_STYLE_CLASS,
    ),
    overwrite(
        "skills/documentation-style/references/method-function.md",
        DOCUMENTATION_STYLE_METHOD_FUNCTION,
    ),
    overwrite(
        "skills/documentation-style/references/block-inline.md",
        DOCUMENTATION_STYLE_BLOCK_INLINE,
    ),
    // --- END issue #2911 ---
    // --- Issue #2913: tm-capabilities auto-generated harness capability
    // catalog (entry SKILL.md + references/*.md). Generator lives at
    // `crates/trusty-mpm/src/bin/tm/generate/`; regenerate with
    // `tm generate capabilities`. `references/workflows.md` is the one
    // hand-authored file in this set. Appended at the end of `ALL` (not
    // interleaved) to minimize merge conflicts with concurrent skill-port
    // work touching this same table.
    overwrite("skills/tm-capabilities.md", TM_CAPABILITIES),
    overwrite(
        "skills/tm-capabilities/references/cli.md",
        TM_CAPABILITIES_CLI,
    ),
    overwrite(
        "skills/tm-capabilities/references/mcp-tools.md",
        TM_CAPABILITIES_MCP_TOOLS,
    ),
    overwrite(
        "skills/tm-capabilities/references/agents.md",
        TM_CAPABILITIES_AGENTS,
    ),
    overwrite(
        "skills/tm-capabilities/references/skills.md",
        TM_CAPABILITIES_SKILLS,
    ),
    overwrite(
        "skills/tm-capabilities/references/doctor.md",
        TM_CAPABILITIES_DOCTOR,
    ),
    overwrite(
        "skills/tm-capabilities/references/workflows.md",
        TM_CAPABILITIES_WORKFLOWS,
    ),
    // --- BEGIN rust-build-performance bundled skill (per Bob directive
    // 2026-07-17; append-only) ---
    overwrite("skills/rust-build-performance.md", RUST_BUILD_PERFORMANCE),
    // --- END rust-build-performance ---
];

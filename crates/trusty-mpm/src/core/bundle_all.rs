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
/// explicit and data-driven.
/// What: [`Overwrite`](InstallPolicy::Overwrite) always writes the embedded
/// contents; [`SeedOnce`](InstallPolicy::SeedOnce) writes only when absent.
/// Test: `claude_stub_is_seed_once`, `framework_instructions_overwrites`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPolicy {
    /// Always write the embedded contents, replacing any existing file.
    Overwrite,
    /// Write the embedded contents only if the target file does not exist.
    SeedOnce,
}

/// Every bundled framework artifact, in install order.
///
/// Why: gives the installer (and tests) one canonical list to walk.
/// What: optimizer policy, framework instructions, user stub, agent catalog,
/// placeholder skill, and Phase 1 (#770) mpm-* guidance skills.
/// Test: `bundle_table_is_complete`.
pub const ALL: &[BundledArtifact] = &[
    BundledArtifact {
        rel_path: "hooks/optimizer.toml",
        contents: OPTIMIZER_TOML,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "hooks/overseer.toml",
        contents: OVERSEER_TOML,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "instructions/INSTRUCTIONS.md",
        contents: FRAMEWORK_INSTRUCTIONS,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "instructions/CLAUDE.md",
        contents: CLAUDE_STUB,
        install: InstallPolicy::SeedOnce,
    },
    BundledArtifact {
        rel_path: "agents/BASE-AGENT.md",
        contents: BASE_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/BASE-ENGINEER.md",
        contents: BASE_ENGINEER,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/BASE-RESEARCH.md",
        contents: BASE_RESEARCH,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/BASE-QA.md",
        contents: BASE_QA,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/BASE-OPS.md",
        contents: BASE_OPS,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/engineer.md",
        contents: ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/qa.md",
        contents: QA_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/research.md",
        contents: RESEARCH_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/ops.md",
        contents: OPS_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/security.md",
        contents: SECURITY_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/documentation.md",
        contents: DOCUMENTATION_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/data-engineer.md",
        contents: DATA_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/version-control.md",
        contents: VERSION_CONTROL_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/ticketing.md",
        contents: TICKETING_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/code-analyzer.md",
        contents: CODE_ANALYZER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/python-engineer.md",
        contents: PYTHON_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/typescript-engineer.md",
        contents: TYPESCRIPT_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/golang-engineer.md",
        contents: GOLANG_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/rust-engineer.md",
        contents: RUST_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/java-engineer.md",
        contents: JAVA_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/php-engineer.md",
        contents: PHP_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/ruby-engineer.md",
        contents: RUBY_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/react-engineer.md",
        contents: REACT_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/nextjs-engineer.md",
        contents: NEXTJS_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/svelte-engineer.md",
        contents: SVELTE_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/web-qa.md",
        contents: WEB_QA_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/api-qa.md",
        contents: API_QA_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/example-skill.md",
        contents: EXAMPLE_SKILL,
        install: InstallPolicy::Overwrite,
    },
    // --- Increment 3: remaining 14 agents ---
    BundledArtifact {
        rel_path: "agents/javascript-engineer.md",
        contents: JAVASCRIPT_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/phoenix-engineer.md",
        contents: PHOENIX_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/dart-engineer.md",
        contents: DART_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/tauri-engineer.md",
        contents: TAURI_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/web-ui-engineer.md",
        contents: WEB_UI_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/refactoring-engineer.md",
        contents: REFACTORING_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/prompt-engineer.md",
        contents: PROMPT_ENGINEER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/code-critic.md",
        contents: CODE_CRITIC_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/gcp-ops.md",
        contents: GCP_OPS_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/vercel-ops.md",
        contents: VERCEL_OPS_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/local-ops.md",
        contents: LOCAL_OPS_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/memory-manager.md",
        contents: MEMORY_MANAGER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/mpm-agent-manager.md",
        contents: MPM_AGENT_MANAGER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "agents/mpm-skills-manager.md",
        contents: MPM_SKILLS_MANAGER_AGENT,
        install: InstallPolicy::Overwrite,
    },
    // --- Phase 1 (#770): mpm-* guidance skills ---
    BundledArtifact {
        rel_path: "skills/mpm-delegation-patterns.md",
        contents: MPM_DELEGATION_PATTERNS,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-verification-protocols.md",
        contents: MPM_VERIFICATION_PROTOCOLS,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-git-file-tracking.md",
        contents: MPM_GIT_FILE_TRACKING,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-pr-workflow.md",
        contents: MPM_PR_WORKFLOW,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-ticketing-integration.md",
        contents: MPM_TICKETING_INTEGRATION,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-circuit-breaker-enforcement.md",
        contents: MPM_CIRCUIT_BREAKER_ENFORCEMENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-bug-reporting.md",
        contents: MPM_BUG_REPORTING,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-session-management.md",
        contents: MPM_SESSION_MANAGEMENT,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-session-pause.md",
        contents: MPM_SESSION_PAUSE,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-session-resume.md",
        contents: MPM_SESSION_RESUME,
        install: InstallPolicy::Overwrite,
    },
    BundledArtifact {
        rel_path: "skills/mpm-tool-usage-guide.md",
        contents: MPM_TOOL_USAGE_GUIDE,
        install: InstallPolicy::Overwrite,
    },
];

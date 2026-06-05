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

/// Placeholder skill definition installed to `skills/example-skill.md`.
pub const EXAMPLE_SKILL: &str = include_str!("../assets/skills/example-skill.md");

// --- Phase 1 (#770): mpm-* guidance skills — constants in bundle_skills.rs ---
#[path = "bundle_skills.rs"]
mod skills_inner;
pub use skills_inner::{
    MPM_BUG_REPORTING, MPM_CIRCUIT_BREAKER_ENFORCEMENT, MPM_DELEGATION_PATTERNS,
    MPM_GIT_FILE_TRACKING, MPM_PR_WORKFLOW, MPM_SESSION_MANAGEMENT, MPM_SESSION_PAUSE,
    MPM_SESSION_RESUME, MPM_TICKETING_INTEGRATION, MPM_TOOL_USAGE_GUIDE,
    MPM_VERIFICATION_PROTOCOLS,
};

/// Claude Code output style deployed to `~/.claude/output-styles/trusty-mpm.md`.
///
/// Why: launched sessions set `"outputStyle": "trusty-mpm"` in the project
/// `.claude/settings.json`; Claude Code only honours that name if a matching
/// style file exists. Bundling it lets the launch path deploy it directly,
/// outside the framework-root [`ALL`] table (which installs under
/// `~/.trusty-mpm/framework/`, not `~/.claude/`).
pub const OUTPUT_STYLE: &str = include_str!("../assets/output-styles/trusty-mpm.md");

// BundledArtifact, InstallPolicy, and ALL are defined in bundle_all.rs.
// They are included here so they can access the constants above via `use super::*`.
#[path = "bundle_all.rs"]
mod all_inner;
pub use all_inner::{ALL, BundledArtifact, InstallPolicy};

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

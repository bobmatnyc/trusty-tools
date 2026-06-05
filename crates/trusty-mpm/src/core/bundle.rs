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

/// Placeholder skill definition installed to `skills/example-skill.md`.
pub const EXAMPLE_SKILL: &str = include_str!("../assets/skills/example-skill.md");

/// Claude Code output style deployed to `~/.claude/output-styles/trusty-mpm.md`.
///
/// Why: launched sessions set `"outputStyle": "trusty-mpm"` in the project
/// `.claude/settings.json`; Claude Code only honours that name if a matching
/// style file exists. Bundling it lets the launch path deploy it directly,
/// outside the framework-root [`ALL`] table (which installs under
/// `~/.trusty-mpm/framework/`, not `~/.claude/`).
pub const OUTPUT_STYLE: &str = include_str!("../assets/output-styles/trusty-mpm.md");

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
/// What: the optimizer policy, framework instructions, the user stub, and the
/// two placeholder artifacts.
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
];

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

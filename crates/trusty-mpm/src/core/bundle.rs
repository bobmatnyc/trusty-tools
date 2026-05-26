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
        rel_path: "skills/example-skill.md",
        contents: EXAMPLE_SKILL,
        install: InstallPolicy::Overwrite,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_non_empty() {
        // Every embedded artifact must carry real content — an empty
        // `include_str!` target would mean a missing or truncated asset file.
        assert!(!OPTIMIZER_TOML.trim().is_empty());
        assert!(!OVERSEER_TOML.trim().is_empty());
        assert!(!FRAMEWORK_INSTRUCTIONS.trim().is_empty());
        assert!(!CLAUDE_STUB.trim().is_empty());
        assert!(!BASE_AGENT.trim().is_empty());
        assert!(!BASE_ENGINEER.trim().is_empty());
        assert!(!BASE_RESEARCH.trim().is_empty());
        assert!(!BASE_QA.trim().is_empty());
        assert!(!BASE_OPS.trim().is_empty());
        assert!(!ENGINEER_AGENT.trim().is_empty());
        assert!(!EXAMPLE_SKILL.trim().is_empty());
        assert!(!OUTPUT_STYLE.trim().is_empty());
    }

    #[test]
    fn output_style_has_matching_frontmatter_name() {
        // Claude Code matches the `outputStyle` settings key against the
        // `name:` in the style file's frontmatter; a mismatch silently falls
        // back to the operator's default style.
        assert!(OUTPUT_STYLE.contains("name: trusty-mpm"));
    }

    #[test]
    fn framework_instructions_and_stub_differ() {
        // The framework artifact and the user stub are distinct files with
        // distinct content; conflating them would lose either upgrades or
        // user edits.
        assert_ne!(FRAMEWORK_INSTRUCTIONS, CLAUDE_STUB);
    }

    #[test]
    fn claude_stub_is_seed_once() {
        // The user stub must never be overwritten on re-install.
        let stub = ALL
            .iter()
            .find(|a| a.rel_path == "instructions/CLAUDE.md")
            .expect("CLAUDE.md stub present in bundle");
        assert_eq!(stub.install, InstallPolicy::SeedOnce);
    }

    #[test]
    fn framework_instructions_overwrites() {
        // The framework instructions must be refreshed on every install.
        let instr = ALL
            .iter()
            .find(|a| a.rel_path == "instructions/INSTRUCTIONS.md")
            .expect("INSTRUCTIONS.md present in bundle");
        assert_eq!(instr.install, InstallPolicy::Overwrite);
    }

    #[test]
    fn optimizer_toml_is_parseable() {
        // The shipped policy must be valid TOML or the installer would deploy
        // a file the daemon then fails to load.
        let parsed: toml::Value = toml::from_str(OPTIMIZER_TOML).expect("valid TOML");
        assert!(parsed.get("default").is_some());
    }

    #[test]
    fn bundle_table_is_complete() {
        // `ALL` must enumerate every artifact with unique, non-empty paths.
        assert_eq!(ALL.len(), 11);
        let mut paths: Vec<&str> = ALL.iter().map(|a| a.rel_path).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), 11, "artifact paths must be unique");
        for artifact in ALL {
            assert!(!artifact.rel_path.is_empty());
            assert!(!artifact.contents.trim().is_empty());
        }
    }

    #[test]
    fn overseer_toml_is_parseable() {
        // The shipped overseer policy must be valid TOML and ship disabled —
        // oversight is opt-in, so installing it must not silently enable it.
        let parsed: toml::Value = toml::from_str(OVERSEER_TOML).expect("valid TOML");
        assert_eq!(
            parsed
                .get("overseer")
                .and_then(|o| o.get("enabled"))
                .and_then(toml::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn overseer_toml_is_in_bundle() {
        // `ALL` must include the overseer policy so `trusty-mpm install`
        // deploys it.
        assert!(
            ALL.iter().any(|a| a.rel_path == "hooks/overseer.toml"),
            "overseer.toml must be a bundled artifact"
        );
    }
}

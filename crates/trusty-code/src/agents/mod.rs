//! Agent configuration loading and types for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML files
//! under `.claude/agents/` so model, prompt, and LLM parameters can evolve
//! without code changes. This module is the assembly point for config types
//! and the discovery helpers.
//! What: Re-exports `AgentConfig` and all nested config types from `config`;
//! provides `discover_agents` for scanning an agents directory.
//! Test: `discover_agents` tests place TOML files in a tempdir and verify the
//! returned list. `AgentConfig::load` tests read individual files.

pub mod config;

pub use config::{
    AgentConfig, AgentInfo, LlmParams, RunnerConfig, RunnerKind, SystemPrompt, ToolsConfig,
};

use std::path::Path;

/// Discover all agent configs in the given directory.
///
/// Why: tcode needs to know which agents are available before the PM loop
/// starts so it can validate `delegate_to_agent` calls pre-flight.
/// What: Scans `dir/*.toml` and returns `(name, path)` pairs sorted by name.
/// Files that fail to parse are skipped with a tracing warning.
/// Test: `discover_agents_finds_tomls`, `discover_agents_skips_non_toml`.
pub fn discover_agents(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!("agents dir not found or unreadable: {}", dir.display());
        return vec![];
    };
    let mut agents: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                let name = p.file_stem()?.to_str()?.to_string();
                Some((name, p))
            } else {
                None
            }
        })
        .collect();
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    agents
}

/// Load all agent configs from the given directory, skipping parse errors.
///
/// Why: Startup needs a map of all available agents; individual parse errors
/// should not crash the whole harness.
/// What: Calls `discover_agents`, then `AgentConfig::load` on each; returns
/// successfully parsed configs only. Failures are logged at WARN level.
/// Test: `load_all_agents_skips_invalid`.
pub fn load_all_agents(dir: &Path) -> Vec<AgentConfig> {
    discover_agents(dir)
        .into_iter()
        .filter_map(|(name, path)| match AgentConfig::load(&path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("skipping agent '{name}': {e}");
                None
            }
        })
        .collect()
}

/// Locate the agents directory for the given project root.
///
/// Why: projects may use either `.claude/agents` (Claude Code native) or
/// `.open-mpm/agents` (open-mpm legacy). Checking both preserves
/// compatibility. Shared between `main.rs`'s `run-task` CLI path and (#2056)
/// `serve::build_router`'s `task.run` wiring, so a project's agents resolve
/// identically whether driven from the CLI or the daemon.
/// What: returns the first of the two conventional directories that exists;
/// falls back to `.claude/agents` (which may not exist yet — callers that
/// need it to exist check separately, e.g. via `AgentConfig::load`'s own
/// error).
/// Test: `agents::tests::locate_agents_dir_prefers_claude_then_open_mpm_then_default`.
pub fn locate_agents_dir(project_root: &Path) -> std::path::PathBuf {
    let claude_agents = project_root.join(".claude").join("agents");
    if claude_agents.exists() {
        return claude_agents;
    }
    let open_mpm_agents = project_root.join(".open-mpm").join("agents");
    if open_mpm_agents.exists() {
        return open_mpm_agents;
    }
    // Default to .claude/agents (may not exist yet).
    claude_agents
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `discover_agents` finds TOML files and returns sorted (name, path) pairs.
    ///
    /// Why: Verify the scanning and sorting logic.
    /// What: Place two TOML + one non-TOML in a tempdir; assert two results in order.
    /// Test: This test.
    #[test]
    fn discover_agents_finds_tomls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("qa-agent.toml"),
            "[agent]\nname=\"qa-agent\"\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname=\"engineer\"\n",
        )
        .expect("write");
        std::fs::write(tmp.path().join("README.md"), "docs").expect("write");

        let agents = discover_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["engineer", "qa-agent"], "sorted by name");
    }

    /// `discover_agents` returns empty when the directory does not exist.
    ///
    /// Why: Guard against panic on a missing `.claude/agents` dir.
    /// What: Pass a non-existent path; expect empty Vec.
    /// Test: This test.
    #[test]
    fn discover_agents_missing_dir_is_empty() {
        let agents = discover_agents(std::path::Path::new("/nonexistent/path/agents"));
        assert!(agents.is_empty());
    }

    /// `load_all_agents` skips files with invalid TOML.
    ///
    /// Why: A single bad config should not crash the harness.
    /// What: Place one valid + one invalid TOML; `load_all_agents` returns 1 entry.
    /// Test: This test.
    #[test]
    fn load_all_agents_skips_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname=\"engineer\"\n",
        )
        .expect("write");
        std::fs::write(tmp.path().join("broken.toml"), "<<NOT TOML>>").expect("write");

        let agents = load_all_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent.name, "engineer");
    }

    /// `locate_agents_dir` prefers `.claude/agents`, falls back to
    /// `.open-mpm/agents`, and defaults to `.claude/agents` when neither
    /// exists.
    #[test]
    fn locate_agents_dir_prefers_claude_then_open_mpm_then_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            locate_agents_dir(tmp.path()),
            tmp.path().join(".claude").join("agents"),
            "default (neither exists) must be .claude/agents"
        );

        std::fs::create_dir_all(tmp.path().join(".open-mpm").join("agents")).expect("mkdir");
        assert_eq!(
            locate_agents_dir(tmp.path()),
            tmp.path().join(".open-mpm").join("agents"),
            "must fall back to .open-mpm/agents when only it exists"
        );

        std::fs::create_dir_all(tmp.path().join(".claude").join("agents")).expect("mkdir");
        assert_eq!(
            locate_agents_dir(tmp.path()),
            tmp.path().join(".claude").join("agents"),
            "must prefer .claude/agents when both exist"
        );
    }
}

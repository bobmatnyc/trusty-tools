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
}

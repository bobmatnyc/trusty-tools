//! Agent configuration loading and types for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML files
//! under `.claude/agents/` so model, prompt, and LLM parameters can evolve
//! without code changes. This module is the assembly point for config types
//! and the discovery helpers.
//! What: Re-exports `AgentConfig` and all nested config types from `config`;
//! provides `discover_agents` for scanning an agents directory, and
//! `load_all_agents` for loading every config in it. `load_all_agents` falls
//! back to `crate::assets::DEFAULT_AGENTS` (#2895) when the disk scan finds
//! nothing — a project with no `.claude/agents/` (or an empty one) still
//! gets a usable `engineer`/`qa-agent`/`code-reviewer` set. A disk directory
//! with even one valid config is treated as the project opting in to its own
//! catalog, so it is used as-is and the embedded defaults are not merged in.
//! Test: `discover_agents` tests place TOML files in a tempdir and verify the
//! returned list. `AgentConfig::load` tests read individual files.
//! `load_all_agents_falls_back_to_embedded_when_disk_empty` and
//! `load_all_agents_disk_wins_when_present` cover the fallback threshold.

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
/// should not crash the whole harness. A project that has not yet created
/// `.claude/agents/` (or has created it but left it empty) must not start
/// with zero agents — see the embedded-fallback note below.
/// What: Calls `discover_agents`, then `AgentConfig::load` on each; returns
/// successfully parsed configs only. Failures are logged at WARN level. If
/// `discover_agents` finds no `.toml` files at all (empty is the fallback
/// threshold — any single valid disk config is treated as the project's own
/// catalog and used as-is, never merged with the embedded set), falls back to
/// `load_embedded_default_agents`. Disk agents always win when present.
/// Test: `load_all_agents_skips_invalid`,
/// `load_all_agents_falls_back_to_embedded_when_disk_empty`,
/// `load_all_agents_disk_wins_when_present`.
pub fn load_all_agents(dir: &Path) -> Vec<AgentConfig> {
    let discovered = discover_agents(dir);
    if discovered.is_empty() {
        return load_embedded_default_agents();
    }
    discovered
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

/// Parse `crate::assets::DEFAULT_AGENTS` in-memory, skipping parse errors.
///
/// Why: The embedded fallback path for `load_all_agents` — no disk I/O, no
/// materialization step (unlike trusty-mpm's install-time embed pattern);
/// the bundled TOML is parsed directly from the compiled-in `&'static str`.
/// What: Calls `AgentConfig::from_toml_str` on every `EmbeddedAgent::toml`;
/// a parse failure (should not happen for a bundled asset, but must not
/// panic) is logged at WARN and skipped.
/// Test: `load_all_agents_falls_back_to_embedded_when_disk_empty`.
fn load_embedded_default_agents() -> Vec<AgentConfig> {
    crate::assets::DEFAULT_AGENTS
        .iter()
        .filter_map(|embedded| match AgentConfig::from_toml_str(embedded.toml) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("skipping embedded default agent '{}': {e}", embedded.name);
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

    /// `load_all_agents` falls back to the embedded defaults when the disk
    /// directory has no `.toml` files (missing dir and empty-but-existing
    /// dir both hit this branch, since `discover_agents` returns `[]` for
    /// both).
    ///
    /// Why: This is the whole point of #2895 — a fresh project must not
    /// start with zero agents.
    /// What: Load from a nonexistent dir; expect exactly the three bundled
    /// defaults (`engineer`, `qa-agent`, `code-reviewer`), sorted by name
    /// via `crate::assets::DEFAULT_AGENTS`'s declared order.
    /// Test: this test.
    #[test]
    fn load_all_agents_falls_back_to_embedded_when_disk_empty() {
        let agents = load_all_agents(std::path::Path::new("/nonexistent/agents/dir"));
        let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(names, vec!["engineer", "qa-agent", "code-reviewer"]);
    }

    /// A disk directory with even one valid config is used as-is; the
    /// embedded defaults are never merged in alongside it.
    ///
    /// Why: Pins the fallback threshold (empty disk scan only) so a project
    /// that has deliberately curated a single custom agent is not silently
    /// joined by three more it did not ask for.
    /// What: One `custom.toml` on disk; `load_all_agents` returns exactly
    /// that one config, not four.
    /// Test: this test.
    #[test]
    fn load_all_agents_disk_wins_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("custom.toml"), "[agent]\nname=\"custom\"\n")
            .expect("write");

        let agents = load_all_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent.name, "custom");
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

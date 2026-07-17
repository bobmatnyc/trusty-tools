//! Agent configuration loading and types for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML files
//! under `.claude/agents/` so model, prompt, and LLM parameters can evolve
//! without code changes. This module is the assembly point for config types
//! and the discovery helpers. As of #2897 (epic #2892, Slice B) a Markdown+
//! frontmatter `.md` loader ([`md_loader`]) is DARK-LAUNCHED alongside the
//! TOML loader — both formats are discovered and loaded; TOML retirement is
//! a later slice (D).
//! What: Re-exports `AgentConfig` and all nested config types from `config`;
//! provides `discover_agents` for scanning an agents directory (now `*.toml`
//! AND `*.md`), and `load_all_agents` for loading every config in it,
//! dispatching to the TOML or `.md` loader by extension. `load_all_agents`
//! falls back to `crate::assets::DEFAULT_AGENTS` (#2895) when the *parsed*
//! result is empty — not merely when no paths were discovered — so a
//! `.claude/agents/` dir that exists but holds only unparseable configs still
//! yields a usable `engineer`/`qa-agent`/`code-reviewer` set instead of
//! silently starting with zero agents. A disk directory with even one
//! successfully-parsed config is treated as the project opting in to its own
//! catalog, so it is used as-is and the embedded defaults are not merged in.
//! Test: `discover_agents` tests place TOML/`.md` files in a tempdir and
//! verify the returned list. `AgentConfig::load` tests read individual TOML
//! files; `md_loader`'s own tests cover the `.md` path.
//! `load_all_agents_falls_back_to_embedded_when_disk_empty`,
//! `load_all_agents_falls_back_to_embedded_when_disk_all_invalid`, and
//! `load_all_agents_disk_wins_when_present` cover the fallback threshold.

pub mod config;
pub mod md_loader;

pub use config::{
    AgentConfig, AgentInfo, LlmParams, RunnerConfig, RunnerKind, SystemPrompt, ToolsConfig,
};
pub use md_loader::load_md_agent;

use std::collections::HashMap;
use std::path::Path;

/// Discover all agent configs in the given directory.
///
/// Why: tcode needs to know which agents are available before the PM loop
/// starts so it can validate `delegate_to_agent` calls pre-flight. As of
/// #2897 both `.toml` (existing) and `.md` (dark-launched) source files are
/// discovered, since both loaders coexist this slice.
/// What: Scans `dir/*.toml` and `dir/*.md`, keyed by file stem (the agent
/// name), and returns `(name, path)` pairs sorted by name. When BOTH a
/// `<name>.toml` and `<name>.md` exist for the same agent name — an edge
/// case, not the common single-format-per-agent path — the `.toml` wins
/// deterministically and a warning is logged: the TOML loader is the
/// established, still-authoritative format during this dark launch (`.md`
/// retirement of TOML is Slice D), so an operator who has not yet migrated
/// a given agent keeps getting the config they already have.
/// Test: `discover_agents_finds_tomls`, `discover_agents_finds_md`,
/// `discover_agents_toml_wins_on_collision`.
pub fn discover_agents(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!("agents dir not found or unreadable: {}", dir.display());
        return vec![];
    };

    // `is_toml` tracked per entry so a same-name `.toml`/`.md` collision can
    // be resolved deterministically (TOML wins) regardless of directory
    // iteration order.
    let mut by_name: HashMap<String, (std::path::PathBuf, bool)> = HashMap::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let ext = p.extension().and_then(|x| x.to_str());
        let is_toml = ext == Some("toml");
        let is_md = ext == Some("md");
        if !is_toml && !is_md {
            continue;
        }
        let Some(name) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let name = name.to_string();

        match by_name.get(&name) {
            None => {
                by_name.insert(name, (p, is_toml));
            }
            Some((existing_path, existing_is_toml)) => {
                if *existing_is_toml {
                    // Existing entry is already the winning `.toml`; an `.md`
                    // for the same name loses.
                    tracing::warn!(
                        "agent '{name}' has both {} and {} — using the .toml \
                         (established format wins during the .md dark launch)",
                        existing_path.display(),
                        p.display()
                    );
                } else if is_toml {
                    // The `.md` seen first loses to this later `.toml`.
                    tracing::warn!(
                        "agent '{name}' has both {} and {} — using the .toml \
                         (established format wins during the .md dark launch)",
                        p.display(),
                        existing_path.display()
                    );
                    by_name.insert(name, (p, is_toml));
                }
                // Two entries of the same extension for one stem cannot occur
                // (distinct filenames with identical stem+ext collide on
                // disk), so no other branch is reachable.
            }
        }
    }

    let mut agents: Vec<(String, std::path::PathBuf)> = by_name
        .into_iter()
        .map(|(name, (p, _))| (name, p))
        .collect();
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    agents
}

/// Load all agent configs from the given directory, skipping parse errors.
///
/// Why: Startup needs a map of all available agents; individual parse errors
/// should not crash the whole harness. A project that has not yet created
/// `.claude/agents/`, has created it but left it empty, or has populated it
/// with only unparseable configs must not start with zero agents — see the
/// embedded-fallback note below.
/// What: Calls `discover_agents`, then dispatches each discovered path to
/// `AgentConfig::load` (`.toml`) or [`md_loader::load_md_agent`] (`.md`) by
/// extension, collecting only the successfully parsed configs (failures are
/// logged at WARN level and skipped). The fallback threshold is the *parsed*
/// result being empty, not merely `discover_agents` finding no paths — a
/// directory full of configs that all fail to parse must fall back exactly
/// like an empty or missing directory does. Any single successfully-parsed
/// disk config is treated as the project's own catalog and used as-is, never
/// merged with the embedded set. Falls back to `load_embedded_default_agents`
/// when parsing yields nothing. Disk agents always win when present.
/// Test: `load_all_agents_skips_invalid`,
/// `load_all_agents_falls_back_to_embedded_when_disk_empty`,
/// `load_all_agents_falls_back_to_embedded_when_disk_all_invalid`,
/// `load_all_agents_disk_wins_when_present`, `load_all_agents_loads_md_agents`.
pub fn load_all_agents(dir: &Path) -> Vec<AgentConfig> {
    let parsed: Vec<AgentConfig> = discover_agents(dir)
        .into_iter()
        .filter_map(|(name, path)| {
            let loaded = if path.extension().and_then(|e| e.to_str()) == Some("md") {
                md_loader::load_md_agent(&path)
            } else {
                AgentConfig::load(&path)
            };
            match loaded {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    tracing::warn!("skipping agent '{name}': {e}");
                    None
                }
            }
        })
        .collect();
    if parsed.is_empty() {
        return load_embedded_default_agents();
    }
    parsed
}

/// Project `crate::assets::DEFAULT_AGENTS` in-memory into `AgentConfig`s.
///
/// Why: The embedded fallback path for `load_all_agents` — no disk I/O, no
/// materialization step (unlike trusty-mpm's install-time embed pattern); the
/// bundled `.md` source is projected directly from the compiled-in
/// `&'static str` (#2897 Slice C — previously native TOML, parsed via
/// `AgentConfig::from_toml_str`; the format changed, the embedded-fallback
/// behavior did not).
/// What: Calls [`md_loader::project_embedded_md`] on every
/// `EmbeddedAgent::md`. Unlike the retired TOML path this projection is
/// infallible (`agent_metadata_from_str` degrades to an empty default on a
/// malformed document rather than erroring — see its doc comment), so there
/// is nothing to skip; every bundled default always yields a config.
/// Test: `load_all_agents_falls_back_to_embedded_when_disk_empty`,
/// `assets::tests::default_agents_field_identical_to_retired_toml`.
fn load_embedded_default_agents() -> Vec<AgentConfig> {
    crate::assets::DEFAULT_AGENTS
        .iter()
        .map(|embedded| md_loader::project_embedded_md(embedded.name, embedded.md))
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
    /// Why: Verify the scanning and sorting logic. Uses a `.txt` file (not
    /// `.md`) as the "irrelevant extension" fixture — since #2897, `.md` is a
    /// legitimate discovered agent format, not an ignorable extension.
    /// What: Place two TOML + one `.txt` in a tempdir; assert two results in
    /// order.
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
        std::fs::write(tmp.path().join("README.txt"), "docs").expect("write");

        let agents = discover_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["engineer", "qa-agent"], "sorted by name");
    }

    /// `discover_agents` also finds `.md` files (#2897 dark launch), sorted
    /// alongside `.toml` results by name.
    ///
    /// Why: both formats must coexist in discovery this slice.
    /// What: one `.toml` + one `.md`, distinct names; both are discovered.
    /// Test: this test.
    #[test]
    fn discover_agents_finds_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname=\"engineer\"\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("md-agent.md"),
            "---\nname: md-agent\n---\n\nBody.\n",
        )
        .expect("write");

        let agents = discover_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["engineer", "md-agent"], "sorted by name");
    }

    /// When both a `<name>.toml` and `<name>.md` exist for the same agent
    /// name, the `.toml` deterministically wins (established format, during
    /// the `.md` dark launch).
    ///
    /// Why: pins the collision-resolution policy so it can't silently flip
    /// with `HashMap` iteration order.
    /// What: `dup.toml` and `dup.md`, both named `dup`; `discover_agents`
    /// returns exactly one `dup` entry pointing at the `.toml` path.
    /// Test: this test.
    #[test]
    fn discover_agents_toml_wins_on_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let toml_path = tmp.path().join("dup.toml");
        std::fs::write(&toml_path, "[agent]\nname=\"dup\"\n").expect("write toml");
        std::fs::write(tmp.path().join("dup.md"), "---\nname: dup\n---\n\nBody.\n")
            .expect("write md");

        let agents = discover_agents(tmp.path());
        assert_eq!(agents.len(), 1, "collision must resolve to one entry");
        assert_eq!(agents[0].0, "dup");
        assert_eq!(agents[0].1, toml_path, "the .toml path must win");
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

    /// A `.claude/agents/` dir that exists and holds `.toml` files, but every
    /// one of them fails to parse, must fall back to the embedded defaults
    /// exactly like a missing/empty dir does — not silently return zero
    /// agents (code-critic finding, #2895 follow-up).
    ///
    /// Why: The fallback threshold is the *parsed* result being empty, not
    /// merely `discover_agents` finding no `.toml` paths. Keying on paths
    /// alone would defeat the "never zero agents" goal for a directory that
    /// exists but is entirely malformed.
    /// What: One `broken.toml` (invalid TOML) on disk, nothing else;
    /// `load_all_agents` returns the three bundled defaults, not `[]`.
    /// Test: this test.
    #[test]
    fn load_all_agents_falls_back_to_embedded_when_disk_all_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("broken.toml"), "<<NOT TOML>>").expect("write");

        let agents = load_all_agents(tmp.path());
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

    /// `load_all_agents` loads `.md` agents alongside `.toml` agents (#2897
    /// dark launch) — both dispatch through the same discover/load/collect
    /// pipeline.
    ///
    /// Why: proves `load_all_agents` actually routes `.md` paths to
    /// `md_loader::load_md_agent`, not just that `discover_agents` finds them.
    /// What: one `.toml` agent + one `.md` agent on disk; both appear in the
    /// loaded result with correctly projected fields.
    /// Test: this test.
    #[test]
    fn load_all_agents_loads_md_agents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("toml-agent.toml"),
            "[agent]\nname=\"toml-agent\"\n",
        )
        .expect("write toml");
        std::fs::write(
            tmp.path().join("md-agent.md"),
            "---\nname: md-agent\nmodel: sonnet\n---\n\nAn md-format agent.\n",
        )
        .expect("write md");

        let agents = load_all_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(agents.len(), 2, "both formats must load");
        assert!(names.contains(&"toml-agent"));
        assert!(names.contains(&"md-agent"));

        let md_cfg = agents
            .iter()
            .find(|a| a.agent.name == "md-agent")
            .expect("md-agent present");
        assert_eq!(md_cfg.agent.model.as_deref(), Some("sonnet"));
        assert_eq!(md_cfg.system_prompt.content, "An md-format agent.");
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

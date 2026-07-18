//! Agent configuration loading and types for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively under
//! `.claude/agents/` so model, prompt, and LLM parameters can evolve without
//! code changes. This module is the assembly point for config types and the
//! discovery helpers. As of #2897 Slice D the on-disk source format is
//! Markdown+frontmatter (`.md`) ONLY — the TOML loader that was
//! DARK-LAUNCHED alongside `.md` in Slice B (#2897) has been retired.
//! Pre-#2897 projects with `.claude/agents/*.toml` files must convert them;
//! see [`discover_agents`]'s orphaned-`.toml` warning and
//! `scripts/migrate-tcode-agents-toml-to-md.py`.
//! What: Re-exports `AgentConfig` and all nested config types from `config`;
//! provides `discover_agents` for scanning an agents directory (`*.md`
//! only — a coexisting `*.toml` is warned about and skipped, never parsed),
//! and `load_all_agents` for loading every discovered `.md` config.
//! `load_all_agents` falls back to `crate::assets::DEFAULT_AGENTS` (#2895;
//! expanded from 3 to 31 agents in Slice E3, #2958) when the *parsed* result
//! is empty — not merely when no paths were discovered — so a
//! `.claude/agents/` dir that exists but holds only unparseable (or
//! exclusively orphaned-`.toml`) configs still yields a usable roster instead
//! of silently starting with zero agents. A disk directory with even one
//! successfully-parsed config is treated as the project opting in to its own
//! catalog, so it is used as-is and the embedded defaults are not merged in.
//! Test: `discover_agents` tests place `.md` (and, for the orphan-warning
//! path, `.toml`) files in a tempdir and verify the returned list;
//! `md_loader`'s own tests cover the loader itself.
//! `load_all_agents_falls_back_to_embedded_when_disk_empty`,
//! `load_all_agents_falls_back_to_embedded_when_disk_all_invalid`, and
//! `load_all_agents_disk_wins_when_present` cover the fallback threshold.

pub mod config;
pub mod md_loader;

pub use config::{
    AgentConfig, AgentInfo, LlmParams, RunnerConfig, RunnerKind, SystemPrompt, ToolsConfig,
};
pub use md_loader::load_md_agent;

use std::path::{Path, PathBuf};

/// Discover all agent configs in the given directory.
///
/// Why: tcode needs to know which agents are available before the PM loop
/// starts so it can validate `delegate_to_agent` calls pre-flight. As of
/// #2897 Slice D, `.toml` is no longer a loadable format — a project that
/// has not migrated yet must be told LOUDLY, not silently ignored, or its
/// agents would appear to vanish with no diagnostic.
/// What: Scans `dir/*.md` and returns `(name, path)` pairs sorted by
/// file-stem name. Any `dir/*.toml` file found in the SAME scan is never
/// parsed and never appears in the returned list; instead every orphaned
/// `.toml` path found in this call is named in ONE aggregated
/// `tracing::warn!` pointing at the migration script — one call per
/// `discover_agents` invocation (not one per file) so a directory with
/// several un-migrated files does not spam the log, and no process-global
/// latch is used (the crate convention is no global/`Once`-gated state for
/// plain helpers — see repo `CLAUDE.md`), so the warning fires again on
/// every subsequent scan for as long as the orphaned file remains.
/// Test: `discover_agents_finds_md`, `discover_agents_ignores_toml`,
/// `discover_agents_warns_on_orphaned_toml`,
/// `discover_agents_missing_dir_is_empty`.
pub fn discover_agents(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!("agents dir not found or unreadable: {}", dir.display());
        return vec![];
    };

    let mut agents: Vec<(String, PathBuf)> = Vec::new();
    let mut orphaned_toml: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        match p.extension().and_then(|x| x.to_str()) {
            Some("md") => {
                if let Some(name) = p.file_stem().and_then(|s| s.to_str()) {
                    agents.push((name.to_string(), p));
                }
            }
            Some("toml") => orphaned_toml.push(p),
            _ => {}
        }
    }

    warn_on_orphaned_toml(&orphaned_toml);

    agents.sort_by(|a, b| a.0.cmp(&b.0));
    agents
}

/// Emit one aggregated warning naming every orphaned `.toml` agent file found
/// in a single [`discover_agents`] scan.
///
/// Why: factored out of `discover_agents` so the "one call, every filename"
/// aggregation rule (see that function's doc) is a single, independently
/// readable/testable unit rather than inline branching.
/// What: no-op when `orphaned` is empty; otherwise one `tracing::warn!`
/// naming issue #2897, every orphaned path (comma-joined), and the
/// migration script.
/// Test: `discover_agents_warns_on_orphaned_toml`.
fn warn_on_orphaned_toml(orphaned: &[PathBuf]) {
    if orphaned.is_empty() {
        return;
    }
    let names = orphaned
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!(
        "trusty-code no longer loads TOML agents (#2897). Found {names} — convert \
         with scripts/migrate-tcode-agents-toml-to-md.py. See CHANGELOG."
    );
}

/// Load all agent configs from the given directory, skipping parse errors.
///
/// Why: Startup needs a map of all available agents; individual parse errors
/// should not crash the whole harness. A project that has not yet created
/// `.claude/agents/`, has created it but left it empty, or has populated it
/// with only unparseable (or, post-#2897, only orphaned-`.toml`) configs must
/// not start with zero agents — see the embedded-fallback note below.
/// What: Calls `discover_agents` (which already excludes `.toml` paths — see
/// its doc), then dispatches every discovered `.md` path to
/// [`md_loader::load_md_agent`], collecting only the successfully parsed
/// configs (failures are logged at WARN level and skipped). The fallback
/// threshold is the *parsed* result being empty, not merely `discover_agents`
/// finding no paths — a directory full of configs that all fail to parse (or
/// that holds nothing but orphaned `.toml` files) must fall back exactly like
/// an empty or missing directory does. Any single successfully-parsed disk
/// config is treated as the project's own catalog and used as-is, never
/// merged with the embedded set. Falls back to `load_embedded_default_agents`
/// when parsing yields nothing. Disk agents always win when present.
/// Test: `load_all_agents_skips_invalid`,
/// `load_all_agents_falls_back_to_embedded_when_disk_empty`,
/// `load_all_agents_falls_back_to_embedded_when_disk_all_invalid`,
/// `load_all_agents_falls_back_when_disk_has_only_orphaned_toml`,
/// `load_all_agents_disk_wins_when_present`.
pub fn load_all_agents(dir: &Path) -> Vec<AgentConfig> {
    let parsed: Vec<AgentConfig> = discover_agents(dir)
        .into_iter()
        .filter_map(|(name, path)| match md_loader::load_md_agent(&path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("skipping agent '{name}': {e}");
                None
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
/// materialization step (unlike trusty-mpm's install-time embed pattern). As
/// of Slice E3 (#2958) `DEFAULT_AGENTS` mixes two projection strategies:
/// tcode's original 3 defaults are flat `.md` strings (#2897 Slice C —
/// previously native TOML, parsed via `AgentConfig::from_toml_str`; the
/// format changed, the embedded-fallback behavior did not), while the 28 tm
/// roster agents inherit from `BASE-*` templates and must be composed
/// in-memory first.
/// What: For each `EmbeddedAgent::Direct { name, md }`, calls
/// [`md_loader::project_embedded_md`] directly — infallible, same as before
/// Slice E3 (`agent_metadata_from_str` degrades to an empty default on a
/// malformed document rather than erroring). For each
/// `EmbeddedAgent::Composed { name }`, calls
/// [`md_loader::project_embedded_md_with_extends`], which IS fallible (an
/// unresolvable `extends:` chain returns `Err`); a failure is logged at
/// ERROR level and the agent is skipped rather than panicking — this should
/// be unreachable in practice (every roster name is pinned against
/// `EMBEDDED_TM_AGENT_SOURCES` by `assets::tests::default_agents_parse_and_names_match`),
/// but a compiled-in asset table defect must degrade gracefully, not crash
/// the harness, exactly like the disk-parsing path in [`load_all_agents`]
/// does for a malformed on-disk config.
/// Test: `load_all_agents_falls_back_to_embedded_when_disk_empty`,
/// `assets::tests::default_agents_field_identical_to_retired_toml`,
/// `assets::tests::default_agents_parse_and_names_match`.
fn load_embedded_default_agents() -> Vec<AgentConfig> {
    crate::assets::DEFAULT_AGENTS
        .iter()
        .filter_map(|embedded| match embedded {
            crate::assets::EmbeddedAgent::Direct { name, md } => {
                Some(md_loader::project_embedded_md(name, md))
            }
            crate::assets::EmbeddedAgent::Composed { name } => {
                match md_loader::project_embedded_md_with_extends(name) {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        tracing::error!(
                            "embedded roster agent '{name}' failed to compose \
                             (build-time asset defect, not a runtime condition): {e}"
                        );
                        None
                    }
                }
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
/// need it to exist check separately, e.g. via [`md_loader::load_md_agent`]'s
/// own error).
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

    /// The 31-name default embedded roster, in `crate::assets::DEFAULT_AGENTS`'s
    /// declared order (Slice E3, #2958) — shared by every fallback test in
    /// this module so the expected list is written once, not duplicated
    /// per-test with the risk of one copy drifting from another.
    ///
    /// Why: three separate fallback scenarios (empty dir, all-invalid dir,
    /// only-orphaned-toml dir) all assert the identical 31-name outcome; a
    /// single source avoids a silent typo in one copy passing review
    /// unnoticed.
    /// What: tcode's original 3 (`engineer`, `qa-agent`, `code-reviewer`)
    /// followed by the 28 roster names, alphabetical, matching
    /// `DEFAULT_AGENTS`'s literal declaration order.
    /// Test: every `load_all_agents_falls_back_*` test below.
    fn expected_default_agent_names() -> Vec<&'static str> {
        vec![
            "engineer",
            "qa-agent",
            "code-reviewer",
            "api-qa",
            "code-analyzer",
            "code-critic",
            "dart-engineer",
            "data-engineer",
            "documentation",
            "golang-engineer",
            "java-engineer",
            "javascript-engineer",
            "local-ops",
            "nextjs-engineer",
            "ops",
            "phoenix-engineer",
            "php-engineer",
            "prompt-engineer",
            "python-engineer",
            "qa",
            "react-engineer",
            "refactoring-engineer",
            "research",
            "ruby-engineer",
            "rust-engineer",
            "security",
            "svelte-engineer",
            "tauri-engineer",
            "typescript-engineer",
            "web-qa",
            "web-ui-engineer",
        ]
    }

    /// `discover_agents` finds `.md` files and returns sorted (name, path)
    /// pairs.
    ///
    /// Why: Verify the scanning and sorting logic. Uses a `.txt` file as the
    /// "irrelevant extension" fixture.
    /// What: Place two `.md` + one `.txt` in a tempdir; assert two results in
    /// order.
    /// Test: This test.
    #[test]
    fn discover_agents_finds_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("qa-agent.md"),
            "---\nname: qa-agent\n---\n\nBody.\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("engineer.md"),
            "---\nname: engineer\n---\n\nBody.\n",
        )
        .expect("write");
        std::fs::write(tmp.path().join("README.txt"), "docs").expect("write");

        let agents = discover_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["engineer", "qa-agent"], "sorted by name");
    }

    /// A `.toml` file in the agents dir is never discovered as an agent
    /// (#2897 Slice D — TOML is retired, not merely deprioritised).
    ///
    /// Why: pins that the format cutover is a hard exclusion, not a
    /// collision-resolution tiebreak (which is what Slice B/C had).
    /// What: one `.md` + one `.toml`, distinct names; only the `.md` name
    /// appears in the result.
    /// Test: this test.
    #[test]
    fn discover_agents_ignores_toml() {
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
        assert_eq!(names, vec!["md-agent"], "the .toml file must be excluded");
    }

    /// An orphaned `.toml` agent file triggers one aggregated WARN-level log
    /// naming the file(s) and the migration script (#2897 Slice D).
    ///
    /// Why: silently dropping a project's pre-#2897 `.toml` agents (rather
    /// than warning) would make them appear to vanish with no diagnostic —
    /// the whole point of the migration-warning requirement.
    /// What: two orphaned `.toml` files, no `.md`; asserts the captured
    /// WARN-level log names both file stems, issue `#2897`, and the
    /// converter script path.
    /// Test: this test.
    #[test]
    fn discover_agents_warns_on_orphaned_toml() {
        crate::test_support::begin_capture();

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("legacy-a.toml"),
            "[agent]\nname=\"legacy-a\"\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("legacy-b.toml"),
            "[agent]\nname=\"legacy-b\"\n",
        )
        .expect("write");

        let agents = discover_agents(tmp.path());
        assert!(agents.is_empty(), "orphaned .toml files are never agents");

        let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
        let warning = captured
            .iter()
            .find(|m| m.contains("no longer loads TOML agents"))
            .unwrap_or_else(|| panic!("expected an orphaned-.toml warning, got: {captured:?}"));
        assert!(warning.contains("legacy-a.toml"), "got: {warning}");
        assert!(warning.contains("legacy-b.toml"), "got: {warning}");
        assert!(warning.contains("#2897"), "got: {warning}");
        assert!(
            warning.contains("scripts/migrate-tcode-agents-toml-to-md.py"),
            "got: {warning}"
        );
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

    /// `load_all_agents` skips files with invalid `.md` frontmatter.
    ///
    /// Why: A single bad config should not crash the harness.
    /// What: Place one valid + one malformed `.md`; `load_all_agents` returns
    /// the one entry that parsed.
    /// Test: This test.
    #[test]
    fn load_all_agents_skips_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.md"),
            "---\nname: engineer\n---\n\nBody.\n",
        )
        .expect("write");
        // `extends:` a base that does not exist — a real `compose_agent`
        // failure, unlike a TOML syntax error, since the `.md` loader has no
        // "unparseable syntax" failure mode of its own (frontmatter degrades
        // to empty defaults rather than erroring — see `md_loader`'s docs).
        std::fs::write(
            tmp.path().join("broken.md"),
            "---\nname: broken\nextends: does-not-exist\n---\n\nBody.\n",
        )
        .expect("write");

        let agents = load_all_agents(tmp.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent.name, "engineer");
    }

    /// `load_all_agents` falls back to the embedded defaults when the disk
    /// directory has no `.md` files (missing dir and empty-but-existing dir
    /// both hit this branch, since `discover_agents` returns `[]` for both).
    ///
    /// Why: This is the whole point of #2895 — a fresh project must not
    /// start with zero agents. As of Slice E3 (#2958) the bundled roster is
    /// 31 agents, not 3.
    /// What: Load from a nonexistent dir; expect exactly the 31-agent
    /// embedded roster, in `crate::assets::DEFAULT_AGENTS`'s declared order —
    /// the original 3 defaults intact, followed by the 28 roster agents.
    /// Test: this test.
    #[test]
    fn load_all_agents_falls_back_to_embedded_when_disk_empty() {
        let agents = load_all_agents(std::path::Path::new("/nonexistent/agents/dir"));
        let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(names, expected_default_agent_names());
    }

    /// A `.claude/agents/` dir that exists and holds `.md` files, but every
    /// one of them fails to parse, must fall back to the embedded defaults
    /// exactly like a missing/empty dir does — not silently return zero
    /// agents (code-critic finding, #2895 follow-up).
    ///
    /// Why: The fallback threshold is the *parsed* result being empty, not
    /// merely `discover_agents` finding no `.md` paths. Keying on paths
    /// alone would defeat the "never zero agents" goal for a directory that
    /// exists but is entirely malformed.
    /// What: One `broken.md` (an `extends:` cycle — a real `compose_agent`
    /// failure) on disk, nothing else; `load_all_agents` returns the
    /// 31-agent bundled roster, not `[]`.
    /// Test: this test.
    #[test]
    fn load_all_agents_falls_back_to_embedded_when_disk_all_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("broken.md"),
            "---\nname: broken\nextends: broken\n---\n\nBody.\n",
        )
        .expect("write");

        let agents = load_all_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(names, expected_default_agent_names());
    }

    /// A directory holding ONLY orphaned `.toml` agents (no `.md` at all)
    /// must fall back to the embedded defaults exactly like an empty
    /// directory does — the warning fires, but the project still gets a
    /// usable agent set (#2897 Slice D).
    ///
    /// Why: pins that the orphan-warning path and the never-zero-agents
    /// fallback compose correctly: warning is a side effect, not a
    /// substitute for a real agent.
    /// What: one `legacy.toml` on disk, no `.md`; `load_all_agents` returns
    /// the 31-agent bundled roster.
    /// Test: this test.
    #[test]
    fn load_all_agents_falls_back_when_disk_has_only_orphaned_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("legacy.toml"), "[agent]\nname=\"legacy\"\n")
            .expect("write");

        let agents = load_all_agents(tmp.path());
        let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(names, expected_default_agent_names());
    }

    /// A disk directory with even one valid config is used as-is; the
    /// embedded defaults are never merged in alongside it.
    ///
    /// Why: Pins the fallback threshold (empty disk scan only) so a project
    /// that has deliberately curated a single custom agent is not silently
    /// joined by three more it did not ask for.
    /// What: One `custom.md` on disk; `load_all_agents` returns exactly that
    /// one config, not four.
    /// Test: this test.
    #[test]
    fn load_all_agents_disk_wins_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("custom.md"),
            "---\nname: custom\n---\n\nBody.\n",
        )
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

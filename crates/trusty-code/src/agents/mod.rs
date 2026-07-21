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
pub mod protocol;

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
pub(crate) fn load_embedded_default_agents() -> Vec<AgentConfig> {
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

/// Failure modes of [`resolve_agent`].
///
/// Why: `resolve_agent`'s callers need to distinguish "nothing named `name`
/// exists anywhere" (a caller/LLM mistake — worth listing available agents
/// for) from "a config for `name` exists but failed to load/compose" (a real
/// bug in that specific file, on disk or embedded) — the same two shapes
/// `RunnerError::UnknownAgent`/`ConfigLoad` already distinguish for the
/// runner's own narrower `<dir>/<name>.md`-only lookup (#2897 Slice D).
/// What: `NotFound` carries the searched `dir` and a precomputed
/// `available` hint (disk ∪ embedded names, already joined for display —
/// see [`available_agent_names`]). `Load` wraps the underlying
/// `md_loader`/`compose` error, whether it came from an existing disk file
/// or a compiled-in embedded asset.
/// Test: `resolve_agent_unknown_name_lists_available_agents`,
/// `resolve_agent_disk_parse_error_does_not_fall_back_to_embedded`.
#[derive(Debug, thiserror::Error)]
pub enum ResolveAgentError {
    /// No config for `name` was found on disk or in the embedded roster.
    #[error(
        "unknown agent '{name}': no config found in {} and no embedded default \
         named '{name}'. Available agents: {available}",
        dir.display()
    )]
    NotFound {
        /// The requested agent slug.
        name: String,
        /// The directory that was searched for `<name>.md`.
        dir: PathBuf,
        /// Disk ∪ embedded agent names, sorted/deduped and comma-joined.
        available: String,
    },
    /// A config for `name` was found (on disk or embedded) but failed to
    /// load, parse, or compose.
    #[error("failed to load agent config for '{name}': {source}")]
    Load {
        /// The requested agent slug.
        name: String,
        /// The underlying load/parse/compose error.
        #[source]
        source: anyhow::Error,
    },
}

/// Resolve a named agent's config, disk taking precedence over the embedded
/// default roster (#3046 — the last gap in the #2958 embedded-roster arc).
///
/// Why: Slice E1-E3 (#2958) built a working embedded fallback for
/// [`load_all_agents`] (the dir-wide scan), but five other production call
/// sites — `runner::in_process::InProcessAgentRunner::load_agent`,
/// `run_task::execute_run_task`'s and `task::executor`'s PM-config loads,
/// `run_task::resolve_agent_model_slug`, and
/// `tools::delegate::DelegateToAgentTool`'s pre-flight check — read
/// `<dir>/<name>.md` directly by single-agent name, with no embedded
/// consultation at all, so the 31-agent roster was unreachable from a real
/// CLI run on a fresh project with no `.claude/agents/`. This is the ONE
/// shared resolution helper all of them now route through, so the
/// disk-wins/embedded-fallback precedence is written and tested exactly
/// once.
/// What: If `<dir>/<name>.md` EXISTS, loads it via [`md_loader::load_md_agent`]
/// — a parse/compose error on an EXISTING disk file is returned as-is
/// (`ResolveAgentError::Load`), NEVER silently falling through to an
/// embedded copy of the same name: a project that deliberately overrode
/// `name` gets its own broken override surfaced, not masked. If the disk
/// file does not exist, looks `name` up in `crate::assets::DEFAULT_AGENTS`:
/// a `Direct` entry projects via [`md_loader::project_embedded_md`]
/// (infallible); a `Composed` entry projects via
/// [`md_loader::project_embedded_md_with_extends`], and on `Err` (a
/// build-time asset defect — see [`load_embedded_default_agents`]'s
/// identical handling) logs `tracing::error!` and falls through to the
/// not-found case below rather than propagating. If nothing matches
/// anywhere, returns `ResolveAgentError::NotFound` with the disk ∪ embedded
/// name list ([`available_agent_names`]) for a caller to surface.
/// A namespaced `<plugin>:<local-name>` (issue #3539 — Phase 1 Claude Code
/// plugin support) routes to [`crate::plugins::agents::find_plugin_agent_config`]
/// instead of the disk/embedded precedence below: plugin agents are an
/// independent, additive tier, never falling back to (or being shadowed
/// by) disk/embedded resolution of the bare local name.
/// Test: `resolve_agent_disk_wins_over_embedded`,
/// `resolve_agent_falls_back_to_embedded_when_disk_misses`,
/// `resolve_agent_disk_parse_error_does_not_fall_back_to_embedded`,
/// `resolve_agent_unknown_name_lists_available_agents`,
/// `resolve_agent_namespaced_name_resolves_plugin_agent`,
/// `resolve_agent_namespaced_unknown_plugin_is_not_found`.
pub fn resolve_agent(dir: &Path, name: &str) -> Result<AgentConfig, ResolveAgentError> {
    if let Some((plugin, agent_name)) = name.split_once(':') {
        return resolve_plugin_agent(dir, plugin, agent_name, name);
    }

    let disk_path = dir.join(format!("{name}.md"));
    if disk_path.exists() {
        return md_loader::load_md_agent(&disk_path).map_err(|source| ResolveAgentError::Load {
            name: name.to_string(),
            source,
        });
    }

    if let Some(embedded) = crate::assets::DEFAULT_AGENTS
        .iter()
        .find(|a| a.name() == name)
    {
        match embedded {
            crate::assets::EmbeddedAgent::Direct { name: n, md } => {
                return Ok(md_loader::project_embedded_md(n, md));
            }
            crate::assets::EmbeddedAgent::Composed { name: n } => {
                match md_loader::project_embedded_md_with_extends(n) {
                    Ok(cfg) => return Ok(cfg),
                    Err(e) => {
                        tracing::error!(
                            "embedded roster agent '{n}' failed to compose \
                             (build-time asset defect, not a runtime condition): {e}"
                        );
                        // Fall through to the not-found error below, mirroring
                        // `load_embedded_default_agents`'s own handling.
                    }
                }
            }
        }
    }

    Err(ResolveAgentError::NotFound {
        name: name.to_string(),
        dir: dir.to_path_buf(),
        available: available_agent_names(dir).join(", "),
    })
}

/// Resolve a namespaced `<plugin>:<agent_name>` for [`resolve_agent`]
/// (issue #3539).
///
/// Why: split out so `resolve_agent`'s own precedence chain (disk ->
/// embedded -> not-found) stays a single, unbranched read for the common
/// unnamespaced case.
/// What: recovers a project root from `dir` via
/// [`crate::plugins::project_root_two_levels_up`] (works whether `dir` is
/// project- or user-scoped — see that function's docs), then delegates to
/// [`crate::plugins::agents::find_plugin_agent_config`]. `None` (no such
/// plugin, or no such agent within it) maps to the same `NotFound` shape
/// `resolve_agent`'s bare-name path uses, `full_name` preserving the
/// caller's original namespaced spelling; `Some(Err)` (found but failed to
/// parse) maps to `Load`.
/// Test: `resolve_agent_namespaced_name_resolves_plugin_agent`,
/// `resolve_agent_namespaced_unknown_plugin_is_not_found`,
/// `resolve_agent_namespaced_unknown_agent_is_not_found`.
fn resolve_plugin_agent(
    dir: &Path,
    plugin: &str,
    agent_name: &str,
    full_name: &str,
) -> Result<AgentConfig, ResolveAgentError> {
    let found = crate::plugins::project_root_two_levels_up(dir).and_then(|root| {
        crate::plugins::agents::find_plugin_agent_config(&root, plugin, agent_name)
    });

    match found {
        Some(Ok(cfg)) => Ok(cfg),
        Some(Err(source)) => Err(ResolveAgentError::Load {
            name: full_name.to_string(),
            source,
        }),
        None => Err(ResolveAgentError::NotFound {
            name: full_name.to_string(),
            dir: dir.to_path_buf(),
            available: available_agent_names(dir).join(", "),
        }),
    }
}

/// List every agent name resolvable via [`resolve_agent`] for `dir`: disk ∪
/// embedded, sorted and deduped.
///
/// Why: Both `resolve_agent`'s own not-found error and
/// `tools::delegate::DelegateToAgentTool::available_agents` (the
/// "Available agents: ..." hint an LLM sees after naming an unknown agent)
/// need the same union — computing it in one place means the hint always
/// names real dispatchable agents, on disk or embedded, never just one or
/// the other.
/// What: `discover_agents(dir)`'s names, plus every
/// `crate::assets::DEFAULT_AGENTS` name not already present, sorted and
/// deduped.
/// Test: `available_agent_names_unions_disk_and_embedded`.
pub fn available_agent_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = discover_agents(dir).into_iter().map(|(n, _)| n).collect();
    for embedded in crate::assets::DEFAULT_AGENTS {
        let n = embedded.name().to_string();
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.sort();
    names.dedup();
    names
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The 32-name default embedded roster, in `crate::assets::DEFAULT_AGENTS`'s
    /// declared order (Slice E3, #2958; `pm` added for #3437) — shared by
    /// every fallback test in this module so the expected list is written
    /// once, not duplicated per-test with the risk of one copy drifting from
    /// another.
    ///
    /// Why: three separate fallback scenarios (empty dir, all-invalid dir,
    /// only-orphaned-toml dir) all assert the identical 32-name outcome; a
    /// single source avoids a silent typo in one copy passing review
    /// unnoticed.
    /// What: tcode's original 4 (`engineer`, `qa-agent`, `code-reviewer`,
    /// `pm`) followed by the 28 roster names, alphabetical, matching
    /// `DEFAULT_AGENTS`'s literal declaration order.
    /// Test: every `load_all_agents_falls_back_*` test below.
    fn expected_default_agent_names() -> Vec<&'static str> {
        vec![
            "engineer",
            "qa-agent",
            "code-reviewer",
            "pm",
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

    /// `resolve_agent`: a disk override wins over an embedded agent of the
    /// SAME name (#3046).
    ///
    /// Why: "project overrides always win" is the whole point of putting
    /// disk-existence first in `resolve_agent`'s precedence.
    /// What: `engineer` is BOTH on disk (with a distinguishing marker
    /// description) and in the embedded roster; asserts the disk copy's
    /// content came back, not the embedded one's.
    /// Test: this test.
    #[test]
    fn resolve_agent_disk_wins_over_embedded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.md"),
            "---\nname: engineer\ndescription: DISK OVERRIDE MARKER\n---\n\nDisk body.\n",
        )
        .expect("write");

        let cfg = resolve_agent(tmp.path(), "engineer").expect("resolve");
        assert_eq!(
            cfg.agent.description.as_deref(),
            Some("DISK OVERRIDE MARKER")
        );
        assert_eq!(cfg.system_prompt.content, "Disk body.");
    }

    /// `resolve_agent`: a disk miss falls back to the embedded roster
    /// (#3046).
    ///
    /// Why: this is the literal #3046 repro at the unit level — an empty
    /// disk dir must still resolve a roster name like `rust-engineer`
    /// (an `EmbeddedAgent::Composed` entry).
    /// What: an empty/nonexistent agents dir; `resolve_agent(dir,
    /// "rust-engineer")` must succeed and carry rust-engineer's known
    /// composed content.
    /// Test: this test.
    #[test]
    fn resolve_agent_falls_back_to_embedded_when_disk_misses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = resolve_agent(tmp.path(), "rust-engineer").expect("resolve embedded");
        assert_eq!(cfg.agent.name, "rust-engineer");
        assert!(
            cfg.system_prompt.content.contains("toolchains-rust-core"),
            "expected rust-engineer's composed body, got: {:?}",
            cfg.system_prompt.content
        );
    }

    /// `resolve_agent`: a parse/compose error on an EXISTING disk file must
    /// error, never silently fall back to the embedded copy of the same
    /// name (#3046).
    ///
    /// Why: masking a real on-disk config bug behind a silently-substituted
    /// embedded default would hide the exact kind of typo/misconfiguration
    /// this validation exists to surface.
    /// What: a disk `rust-engineer.md` with a broken `extends:` chain (a
    /// name that resolves to nothing); asserts `resolve_agent` errors with
    /// `ResolveAgentError::Load`, not `Ok` with the embedded content.
    /// Test: this test.
    #[test]
    fn resolve_agent_disk_parse_error_does_not_fall_back_to_embedded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("rust-engineer.md"),
            "---\nname: rust-engineer\nextends: does-not-exist\n---\n\nBroken.\n",
        )
        .expect("write");

        let err = resolve_agent(tmp.path(), "rust-engineer")
            .expect_err("a broken disk override must error, not fall back");
        assert!(
            matches!(err, ResolveAgentError::Load { .. }),
            "expected Load, got: {err:?}"
        );
    }

    /// `resolve_agent`: a name absent from disk AND the embedded roster
    /// errors with a message listing available agents (#3046).
    ///
    /// Why: pins the `NotFound` shape callers rely on to build a helpful
    /// "unknown agent, did you mean one of..." message.
    /// What: an empty disk dir; `resolve_agent(dir, "totally-bogus")`
    /// errors, and the error text names at least one real embedded agent
    /// (`engineer`).
    /// Test: this test.
    #[test]
    fn resolve_agent_unknown_name_lists_available_agents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_agent(tmp.path(), "totally-bogus").expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("totally-bogus"), "got: {msg}");
        assert!(
            msg.contains("engineer"),
            "expected the available-agents hint to name a real embedded agent, got: {msg}"
        );
    }

    /// `resolve_agent` routes a namespaced `<plugin>:<name>` to
    /// `plugins::agents::find_plugin_agent_config`, recovering the project
    /// root from the `.claude/agents` dir it was given (#3539).
    ///
    /// Why: this is the acceptance criterion for the dispatch half of
    /// #3539's namespacing decision — a namespaced name must actually
    /// dispatch, not just appear in `agents.list`.
    /// Test: this test.
    #[test]
    fn resolve_agent_namespaced_name_resolves_plugin_agent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents_dir = tmp.path().join(".claude").join("agents");
        let plugin_agents_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("my-plugin")
            .join("agents");
        std::fs::create_dir_all(&plugin_agents_dir).expect("mkdir");
        std::fs::write(
            plugin_agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Plugin reviewer\n---\n\nReview things.\n",
        )
        .expect("write");

        let cfg = resolve_agent(&agents_dir, "my-plugin:reviewer").expect("resolve plugin agent");
        assert_eq!(cfg.agent.name, "my-plugin:reviewer");
        assert_eq!(cfg.agent.description.as_deref(), Some("Plugin reviewer"));
        assert_eq!(cfg.system_prompt.content, "Review things.");
    }

    /// A namespaced name whose plugin does not exist errors `NotFound`, not
    /// a silent fall-through to disk/embedded resolution of the bare name.
    ///
    /// Test: this test.
    #[test]
    fn resolve_agent_namespaced_unknown_plugin_is_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents_dir = tmp.path().join(".claude").join("agents");

        let err = resolve_agent(&agents_dir, "no-such-plugin:reviewer")
            .expect_err("unknown plugin must not resolve");
        assert!(
            matches!(err, ResolveAgentError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    /// A namespaced name for a known plugin but an unknown local agent name
    /// errors `NotFound`.
    ///
    /// Test: this test.
    #[test]
    fn resolve_agent_namespaced_unknown_agent_is_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents_dir = tmp.path().join(".claude").join("agents");
        let plugin_agents_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("my-plugin")
            .join("agents");
        std::fs::create_dir_all(&plugin_agents_dir).expect("mkdir");

        let err = resolve_agent(&agents_dir, "my-plugin:ghost")
            .expect_err("unknown agent within a known plugin must not resolve");
        assert!(
            matches!(err, ResolveAgentError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    /// `available_agent_names` unions disk and embedded names, sorted and
    /// deduped (#3046).
    ///
    /// Why: shared by `resolve_agent`'s not-found hint and
    /// `tools::delegate::DelegateToAgentTool::available_agents` — both need
    /// the identical union, computed once.
    /// What: one disk-only custom agent plus the embedded roster; asserts
    /// both the disk name and a known embedded name are present, with no
    /// duplicates.
    /// Test: this test.
    #[test]
    fn available_agent_names_unions_disk_and_embedded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("custom.md"),
            "---\nname: custom\n---\n\nBody.\n",
        )
        .expect("write");

        let names = available_agent_names(tmp.path());
        assert!(names.contains(&"custom".to_string()), "got: {names:?}");
        assert!(
            names.contains(&"rust-engineer".to_string()),
            "got: {names:?}"
        );
        let mut deduped = names.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(names, deduped, "must contain no duplicates");
    }
}

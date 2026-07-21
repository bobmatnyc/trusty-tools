//! Claude Code plugin ingestion, Phase 1: local-directory agents + skills
//! only (issue #3539).
//!
//! Why: Claude Code plugins package reusable agents/skills (and, in later
//! phases, commands/hooks/MCP servers) for drop-in use by a harness. tcode
//! already parses the exact on-disk formats a plugin ships agents
//! (`.md`+frontmatter, see `agents::md_loader`) and skills (`SKILL.md`,
//! progressive disclosure, see `crate::skills`) in — so Phase 1 is a
//! discovery + namespacing layer over those existing parsers, not a new
//! format. Phase 1 is deliberately narrow (#3539's locked scope): a plugin
//! is a LOCAL directory under `<project_root>/.claude/plugins/<plugin>/`
//! (no marketplace/git fetch), and only `agents/` + `skills/` are ingested
//! (no commands/hooks/MCP — later phases). Plugin entries are ADDITIVE
//! ONLY: they are namespaced `<plugin>:<name>` in every surface that lists
//! or resolves them, so they can never collide with — and therefore never
//! override — an unnamespaced project or embedded/bundled name.
//! What: [`PluginRoot`] is one discovered plugin (resolved name + its
//! agents/skills directories, honoring `.claude-plugin/plugin.json`'s
//! `name`/`agents`/`skills` overrides when present). [`discover_plugin_roots`]
//! scans `<project_root>/.claude/plugins/` for plugin subdirectories.
//! [`project_root_two_levels_up`] is the shared seam every integration point
//! (`agents::protocol::agents_list`, `agents::resolve_agent`,
//! `skills::protocol::skills_list`, `skills::FsSkillResolver`) uses to
//! recover a project root from the `.claude/agents` or `.claude/skills`
//! directory it already holds, without threading a new parameter through
//! every call site. Submodules [`agents`] and [`skills`] do the actual
//! agent/skill discovery, namespacing, and resolution.
//! Test: `tests::*` here cover manifest parsing (present/absent/malformed,
//! name/agents/skills overrides, later-phase key detection) and
//! `discover_plugin_roots` scanning; `agents::tests`/`skills::tests` cover
//! the per-domain ingestion contracts.

pub mod agents;
pub mod skills;

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `.claude/plugins` directory name, relative to a project root.
const PLUGINS_DIRNAME: &str = "plugins";

/// `.claude-plugin/plugin.json` keys Phase 1 recognizes as later-phase
/// surfaces (commands/hooks/MCP) — present but intentionally unimplemented,
/// per #3539's scope.
///
/// Why: names the exact set [`load_plugin_root`] checks for so a plugin
/// author sees a debug log explaining why e.g. its `hooks` declaration had
/// no effect, rather than silent nothing.
const LATER_PHASE_MANIFEST_KEYS: &[&str] = &["commands", "hooks", "mcpServers", "mcp"];

/// One discovered plugin: its resolved name and the directories its agents
/// and skills live under.
///
/// Why: the resolved `name` (manifest `name:`, falling back to the
/// directory name) is what every namespaced `<plugin>:<local-name>` entry
/// uses — it may differ from the on-disk subdirectory name, so callers must
/// always go through this struct rather than re-deriving a plugin's
/// identity from its path.
/// What: `root` is the plugin's own directory
/// (`<project_root>/.claude/plugins/<dir>`); `agents_dir`/`skills_dir` are
/// already-joined, ready-to-scan paths (manifest overrides applied, or the
/// `agents`/`skills` convention when absent) — neither is guaranteed to
/// exist on disk.
/// Test: `tests::discover_plugin_roots_*`.
#[derive(Debug, Clone)]
pub struct PluginRoot {
    /// Resolved plugin name — manifest `name:`, or the directory name.
    pub name: String,
    /// The plugin's own root directory.
    pub root: PathBuf,
    /// Where this plugin's `*.md` agents live.
    pub agents_dir: PathBuf,
    /// Where this plugin's `<skill>/SKILL.md` skills live.
    pub skills_dir: PathBuf,
}

/// The subset of `.claude-plugin/plugin.json` Phase 1 understands.
///
/// Why: a plugin manifest is optional (#3539 — a plugin with no manifest
/// falls back to the directory name + the `agents`/`skills` convention);
/// when present, Phase 1 only acts on `name`/`agents`/`skills` — every
/// other key (description, version, author, and the later-phase
/// `commands`/`hooks`/`mcpServers`) is captured in `other` purely so
/// [`load_plugin_root`] can detect and log the later-phase ones, never to
/// drive behavior.
/// Test: `tests::manifest_parses_name_and_overrides`,
/// `tests::manifest_flattens_unknown_keys`.
#[derive(Debug, Default, Deserialize)]
struct PluginManifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    agents: Option<String>,
    #[serde(default)]
    skills: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// Discover every plugin under `<project_root>/.claude/plugins/`.
///
/// Why: the single scan point [`agents::discover_plugin_agents`],
/// [`skills::discover_plugin_skills`], and the namespaced-name resolvers in
/// both submodules all build on.
/// What: each immediate subdirectory of `.claude/plugins/` is one plugin
/// root, resolved via [`load_plugin_root`]. A missing/unreadable
/// `.claude/plugins/` directory yields an empty list (never an error — a
/// project simply using no plugins is the common case, mirroring
/// `agents::discover_agents`'s missing-dir handling). Non-directory entries
/// are skipped. Sorted by resolved name.
/// Test: `tests::discover_plugin_roots_missing_dir_is_empty`,
/// `tests::discover_plugin_roots_finds_subdirs`,
/// `tests::discover_plugin_roots_honors_manifest_name_override`.
pub fn discover_plugin_roots(project_root: &Path) -> Vec<PluginRoot> {
    let plugins_dir = project_root.join(".claude").join(PLUGINS_DIRNAME);
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
        tracing::debug!(
            "plugins dir not found or unreadable: {}",
            plugins_dir.display()
        );
        return Vec::new();
    };

    let mut roots: Vec<PluginRoot> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let path = entry.path();
            let dir_name = path.file_name()?.to_str()?.to_string();
            Some(load_plugin_root(&path, &dir_name))
        })
        .collect();
    roots.sort_by(|a, b| a.name.cmp(&b.name));
    roots
}

/// Resolve one plugin subdirectory into a [`PluginRoot`].
///
/// Why: factored out of [`discover_plugin_roots`] so the
/// manifest-or-convention resolution (name, agents dir, skills dir) is a
/// single, independently readable unit.
/// What: reads `<plugin_dir>/.claude-plugin/plugin.json` if present; a
/// missing or malformed manifest degrades to `dir_name` +
/// the `agents`/`skills` directory convention (never an error — see the
/// module doc's "no manifest" case). When a manifest IS present and
/// declares any [`LATER_PHASE_MANIFEST_KEYS`], logs one aggregated
/// `tracing::debug!` naming them.
/// Test: `tests::discover_plugin_roots_honors_manifest_name_override`,
/// `tests::discover_plugin_roots_honors_path_overrides`,
/// `tests::discover_plugin_roots_falls_back_without_manifest`,
/// `tests::discover_plugin_roots_warns_on_later_phase_keys`.
fn load_plugin_root(plugin_dir: &Path, dir_name: &str) -> PluginRoot {
    let manifest_path = plugin_dir.join(".claude-plugin").join("plugin.json");
    let manifest: Option<PluginManifest> = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    let name = manifest
        .as_ref()
        .and_then(|m| m.name.clone())
        .unwrap_or_else(|| dir_name.to_string());
    let agents_rel = manifest
        .as_ref()
        .and_then(|m| m.agents.clone())
        .unwrap_or_else(|| "agents".to_string());
    let skills_rel = manifest
        .as_ref()
        .and_then(|m| m.skills.clone())
        .unwrap_or_else(|| "skills".to_string());

    if let Some(m) = &manifest {
        let later_phase: Vec<&str> = LATER_PHASE_MANIFEST_KEYS
            .iter()
            .copied()
            .filter(|k| m.other.contains_key(*k))
            .collect();
        if !later_phase.is_empty() {
            tracing::debug!(
                "plugin '{name}' declares plugin.json key(s) {} — tcode Phase 1 ingests \
                 agents+skills only; ignored until a later phase (#3539)",
                later_phase.join(", ")
            );
        }
    }

    PluginRoot {
        agents_dir: plugin_dir.join(agents_rel),
        skills_dir: plugin_dir.join(skills_rel),
        root: plugin_dir.to_path_buf(),
        name,
    }
}

/// Recover a project root from a `.claude/agents` or `.claude/skills`
/// directory two levels below it.
///
/// Why: `agents::protocol::AgentsCatalogState`/`skills::protocol::SkillsCatalogState`
/// and `skills::FsSkillResolver` already hold a resolved `.claude/agents` or
/// `.claude/skills` directory (see `binding::ProjectBinding::agents_dir`,
/// `skills::locate_skills_dir`) — both are always exactly `<root>/.claude/<x>`.
/// Deriving the root from that existing path lets every plugin-aware call
/// site (`agents::resolve_agent`, `agents::protocol::agents_list`,
/// `skills::protocol::skills_list`, `skills::FsSkillResolver::new`) opt into
/// plugin discovery without a new `project_root` parameter threaded through
/// their existing signatures/constructors.
/// What: `dir.parent().parent()` — works identically whether `dir` is the
/// `.claude/agents` convention or the legacy `.open-mpm/agents` one, since
/// both are exactly two segments below the root. Returns `None` when `dir`
/// has fewer than two ancestors (e.g. a bare relative path in tests).
/// Test: `tests::project_root_two_levels_up_recovers_root`,
/// `tests::project_root_two_levels_up_none_when_too_shallow`.
pub(crate) fn project_root_two_levels_up(dot_claude_child_dir: &Path) -> Option<PathBuf> {
    dot_claude_child_dir
        .parent()?
        .parent()
        .map(Path::to_path_buf)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

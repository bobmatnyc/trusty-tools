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
//!
//! ## Security: plugin content is HOSTILE input (issue #3547)
//!
//! Everything under `.claude/plugins/` — `plugin.json`, agent/skill
//! directory names, and frontmatter content — is THIRD-PARTY, not authored
//! by the project owner the way `.claude/agents|skills/` is. Two path-join
//! sites are therefore treated as adversarial and validated BEFORE any
//! filesystem join, exactly like `agents::protocol::validate_agent_name`
//! already treats a caller-supplied catalog name:
//! [`load_plugin_root`] validates a `plugin.json` `agents`/`skills`
//! override (a `PathBuf::join` of an absolute or `..`-bearing override
//! escapes `plugin_dir` outright — code-critic PR #3547 review, CRITICAL 1)
//! and [`is_valid_namespaced_name`] validates every caller-supplied
//! `<plugin>:<local>` dispatch name (a raw local segment containing `/` or
//! `..` escapes the plugin's `agents_dir`/`skills_dir` — CRITICAL 2 / HIGH
//! 3) before [`agents::find_plugin_agent_config`]/
//! [`skills::resolve_plugin_skill_body`] ever join it onto a directory.
//! A third, independent hole (re-review, CRITICAL 5, CWE-59): the
//! DIRECTORY-path and NAME guards above say nothing about the LEAF FILE a
//! validated name resolves to — `std::fs::read_to_string`,
//! `Path::is_dir`/`is_file` all FOLLOW symlinks, so a plugin can ship
//! `agents/leak.md` (or a `skills/<name>/SKILL.md`, or even `skills/<name>`
//! itself) as a symlink to an arbitrary host file/directory (e.g.
//! `~/.ssh/id_rsa`); discovery would read the TARGET's content as that
//! agent's system prompt or skill body, which then reaches an LLM prompt
//! verbatim once dispatched. [`path_is_contained`] closes this: every
//! point a discovered `.md`/`SKILL.md` file's content is actually read
//! (`agents::load_plugin_agent`, `skills::discover_one_plugin_skills`,
//! `skills::resolve_plugin_skill_body`) canonicalizes the full leaf path
//! and requires it stay contained within the already-validated
//! `agents_dir`/`skills_dir` — since `canonicalize` resolves every symlink
//! in the path, not just the final component, one check catches a
//! symlinked leaf file, a symlinked intermediate directory, or both.

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
/// `tracing::debug!` naming them. `agents`/`skills` overrides route through
/// [`safe_plugin_subdir`] — `plugin.json` is hostile input (#3547).
/// Test: `tests::discover_plugin_roots_honors_manifest_name_override`,
/// `tests::discover_plugin_roots_honors_path_overrides`,
/// `tests::discover_plugin_roots_falls_back_without_manifest`,
/// `tests::discover_plugin_roots_warns_on_later_phase_keys`,
/// `tests::load_plugin_root_rejects_absolute_agents_override`,
/// `tests::load_plugin_root_rejects_dotdot_skills_override`.
fn load_plugin_root(plugin_dir: &Path, dir_name: &str) -> PluginRoot {
    let manifest_path = plugin_dir.join(".claude-plugin").join("plugin.json");
    let manifest: Option<PluginManifest> = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    let name = manifest
        .as_ref()
        .and_then(|m| m.name.clone())
        .unwrap_or_else(|| dir_name.to_string());

    let agents_dir = match manifest.as_ref().and_then(|m| m.agents.clone()) {
        Some(rel) => safe_plugin_subdir(plugin_dir, &rel, "agents", &name, "agents"),
        None => plugin_dir.join("agents"),
    };
    let skills_dir = match manifest.as_ref().and_then(|m| m.skills.clone()) {
        Some(rel) => safe_plugin_subdir(plugin_dir, &rel, "skills", &name, "skills"),
        None => plugin_dir.join("skills"),
    };

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
        agents_dir,
        skills_dir,
        root: plugin_dir.to_path_buf(),
        name,
    }
}

/// Validate and resolve a `plugin.json` `agents`/`skills` path override
/// against `plugin_dir`, rejecting anything that could escape it
/// (code-critic PR #3547 review, CRITICAL 1).
///
/// Why: `plugin.json` is THIRD-PARTY, caller-controlled content.
/// `PathBuf::join` REPLACES the base entirely when the joined component is
/// absolute (`plugin_dir.join("/etc")` == `/etc`), and a relative
/// `../../..` override escapes upward even though it isn't absolute —
/// either lets a malicious `plugin.json` point `agents`/`skills` at an
/// arbitrary host directory (e.g. `~/.ssh`), which discovery would then
/// scan and load `.md`/`SKILL.md` files out of.
/// What: rejects `rel` outright (before any join) when it is absolute or
/// contains a `..` component — this alone blocks every practical escape and
/// keeps the warning specific about which shape failed. As defense in
/// depth, the accepted candidate is additionally required to canonicalize
/// to a path contained within `plugin_dir`'s own canonicalization (this
/// also catches a symlink planted inside `plugin_dir` that resolves
/// outside it). Any rejection logs one `tracing::warn!` naming the plugin,
/// field, and offending value, and falls back to `plugin_dir.join(default)`
/// — the un-overridden convention — rather than ever returning the
/// untrusted path. `default` itself (`"agents"`/`"skills"`, never
/// attacker-controlled) is never routed through this validation.
/// Test: `tests::load_plugin_root_rejects_absolute_agents_override`,
/// `tests::load_plugin_root_rejects_dotdot_skills_override`,
/// `tests::discover_plugin_roots_honors_path_overrides`.
fn safe_plugin_subdir(
    plugin_dir: &Path,
    rel: &str,
    default: &str,
    plugin_name: &str,
    field: &str,
) -> PathBuf {
    let fallback = || plugin_dir.join(default);

    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        tracing::warn!(
            "plugin '{plugin_name}' plugin.json '{field}' override {rel:?} is absolute or \
             contains '..' — rejected as a path-traversal attempt, falling back to the \
             default '{default}' convention (#3547)"
        );
        return fallback();
    }

    let joined = plugin_dir.join(rel);
    match (plugin_dir.canonicalize(), joined.canonicalize()) {
        (Ok(canon_root), Ok(canon_joined)) if canon_joined.starts_with(&canon_root) => joined,
        _ => {
            tracing::warn!(
                "plugin '{plugin_name}' plugin.json '{field}' override {rel:?} does not resolve \
                 within the plugin directory — rejected, falling back to the default \
                 '{default}' convention (#3547)"
            );
            fallback()
        }
    }
}

/// Whether `name` is a well-formed `<plugin>:<local>` namespaced dispatch
/// name — the ONE shape every plugin-aware caller (`agents::resolve_agent`,
/// `plugins::agents::find_plugin_agent_config`,
/// `plugins::skills::resolve_plugin_skill_body`,
/// `skills::FsSkillResolver::resolve`, `tools::delegate::DelegateToAgentTool`,
/// the CLI's `run-task` validation, `runner::agent_config_exists`) accepts
/// (code-critic PR #3547 review, CRITICAL 2 / HIGH 3 / HIGH 4).
///
/// Why: a namespaced name's `local` segment is joined onto a plugin's
/// `agents_dir`/`skills_dir` — e.g. `plugins::skills::resolve_plugin_skill_body`
/// builds `skills_dir.join(skill_name).join("SKILL.md")` directly from the
/// caller-supplied (and, via `use_skill`, LLM-supplied) segment. Restricting
/// BOTH segments to `agents::protocol::validate_agent_name`'s existing safe
/// charset (`[a-z0-9-]+`, 1-64 chars — reused, not re-derived, so the two
/// concepts of "a safe catalog name" never drift apart) makes a traversal
/// payload like `foo:../../../../x` or `foo:/etc/passwd` syntactically
/// impossible to construct: the charset contains no `/` and no `.` at all,
/// so no join built from a validated segment can ever leave the directory
/// it was joined onto, regardless of what the discovered catalog happens to
/// list (closes the residual gap where a plugin's own frontmatter could
/// declare a spoofed `name:` that would otherwise appear "known").
/// What: `true` only for EXACTLY two `:`-separated segments, each accepted
/// by `agents::protocol::validate_agent_name`. Zero or one segment (a plain
/// name), more than two (multiple colons), or either segment failing the
/// charset/length check all return `false`.
/// Test: `tests::is_valid_namespaced_name_accepts_two_safe_segments`,
/// `tests::is_valid_namespaced_name_rejects_traversal_segment`,
/// `tests::is_valid_namespaced_name_rejects_absolute_segment`,
/// `tests::is_valid_namespaced_name_rejects_plain_name`,
/// `tests::is_valid_namespaced_name_rejects_extra_colon`.
pub fn is_valid_namespaced_name(name: &str) -> bool {
    let segments: Vec<&str> = name.splitn(3, ':').collect();
    match segments.as_slice() {
        [plugin, local] => {
            crate::agents::protocol::validate_agent_name(plugin).is_ok()
                && crate::agents::protocol::validate_agent_name(local).is_ok()
        }
        _ => false,
    }
}

/// Whether `leaf`'s canonicalized path is contained within `container`'s
/// own canonicalization — the LEAF-FILE-IDENTITY guard neither
/// [`safe_plugin_subdir`] (guards a `plugin.json` directory override) nor
/// [`is_valid_namespaced_name`] (guards the dispatch NAME) provides
/// (code-critic PR #3547 re-review, CRITICAL 5, CWE-59).
///
/// Why: `std::fs::read_to_string`, `Path::is_dir`, and `Path::is_file` all
/// FOLLOW symlinks. A validated directory (`agents_dir`/`skills_dir`) and a
/// validated namespaced NAME together still say nothing about what the
/// resulting `<dir>/<name>` (or `<dir>/<name>/SKILL.md`) path actually
/// points AT on disk — a hostile plugin can make that exact leaf entry (or
/// an intermediate path component, e.g. `skills/<name>` itself) a symlink
/// to an arbitrary host file/directory (`~/.ssh/id_rsa`). Reading it would
/// feed the target's content into an LLM prompt verbatim once the
/// (legitimately namespaced, legitimately-directoried) name is dispatched.
/// What: canonicalizes both `leaf` and `container`; `true` only when BOTH
/// canonicalize successfully AND the canonicalized `leaf` starts with the
/// canonicalized `container`. A leaf that fails to canonicalize (broken
/// symlink, race, doesn't exist) is treated as NOT contained — fail
/// closed, never fail open into "assume safe". Because `canonicalize`
/// resolves EVERY symlink in the path (not merely the final component),
/// ONE call here catches a symlinked leaf file, a symlinked intermediate
/// directory, or both, uniformly — callers do not need a separate check
/// for "is the containing directory itself a symlink".
/// Test: `tests::path_is_contained_accepts_real_file_within_container`,
/// `tests::path_is_contained_rejects_symlink_escaping_container`,
/// `tests::path_is_contained_rejects_nonexistent_leaf`.
pub(crate) fn path_is_contained(leaf: &Path, container: &Path) -> bool {
    match (leaf.canonicalize(), container.canonicalize()) {
        (Ok(canon_leaf), Ok(canon_container)) => canon_leaf.starts_with(&canon_container),
        _ => false,
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

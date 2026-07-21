//! Phase-1 plugin AGENT ingestion: discovery, namespacing, and resolution
//! (issue #3539).
//!
//! Why: a plugin's `agents/*.md` files are the exact same
//! Markdown+frontmatter format `agents::md_loader` already parses for
//! project/embedded agents — the only new work is namespacing
//! (`<plugin>:<name>`) and two Phase-1 leaf-only guarantees #3539 locks in:
//! unsupported frontmatter fields (`effort`/`maxTurns`/`memory`/`isolation`/
//! `disallowedTools` — trusty-mpm's own agent schema, which a plugin may
//! carry over) are dropped with a warning rather than failing the load, and
//! an `extends:` chain is never composed (a plugin agent is always a leaf —
//! cross-catalog compose is a later-phase item).
//! What: [`discover_plugin_agents`] lists every plugin agent as a fully
//! projected [`AgentConfig`] (used by `agents::protocol::agents_list`'s
//! `plugin` tier); [`find_plugin_agent_config`] resolves one namespaced
//! `<plugin>:<name>` on demand (used by `agents::resolve_agent`, the
//! dispatch path). Both route through [`load_plugin_agent`], which reuses
//! `agents::md_loader`'s frontmatter/body parsing
//! ([`crate::agents::md_loader::extract_body`],
//! [`crate::agents::md_loader::project_to_agent_config`]) rather than
//! duplicating the projection — the only difference from a disk agent load
//! is the namespaced name and the two Phase-1 guards above.
//! Test: `tests::*` — discovery finds and namespaces agents, unsupported
//! fields are dropped with a warning, `extends:` is warned-and-ignored,
//! resolution finds/misses a namespaced name, a base-agent-named plugin
//! agent is excluded from listing.

use std::path::Path;

use anyhow::Context;
use trusty_agents_common::agents::metadata::agent_metadata_from_str;

use crate::agents::AgentConfig;
use crate::agents::md_loader::{extract_body, project_to_agent_config};

use super::discover_plugin_roots;

/// trusty-mpm agent-frontmatter fields tcode's `AgentConfig` has no slot
/// for. Present on a plugin agent, they are dropped with one aggregated
/// warning rather than failing the load (#3539's locked Phase-1 contract).
const UNSUPPORTED_AGENT_FIELDS: &[&str] = &[
    "effort",
    "maxTurns",
    "memory",
    "isolation",
    "disallowedTools",
];

/// List every plugin agent across every discovered plugin, fully projected
/// and namespaced.
///
/// Why: `agents::protocol::agents_list`'s `plugin` tier needs the same
/// `AgentConfig` shape (for `description`/`model` in its wire entry) the
/// embedded and disk tiers already produce.
/// What: for each [`PluginRoot`], scans `agents_dir` via
/// `agents::discover_agents` (the same `.md`-only, `.toml`-warns scan disk
/// agents use), skips any local name `agents::protocol::is_base_agent`
/// recognizes (a plugin literally shipping `base-engineer.md` etc. is
/// excluded exactly like the embedded/disk tiers — #3539's base-filter
/// interaction clause), and loads the rest via
/// [`load_plugin_agent`]. A load failure is logged at WARN and skipped
/// (never aborts the whole scan), mirroring `agents::load_all_agents`'s
/// per-file resilience. Sorted by the final namespaced name.
/// Test: `tests::discover_plugin_agents_namespaces_entries`,
/// `tests::discover_plugin_agents_excludes_base_named_agent`,
/// `tests::discover_plugin_agents_skips_unparseable_agent`.
pub fn discover_plugin_agents(project_root: &Path) -> Vec<AgentConfig> {
    let mut out: Vec<AgentConfig> = Vec::new();
    for root in discover_plugin_roots(project_root) {
        for (agent_name, path) in crate::agents::discover_agents(&root.agents_dir) {
            if crate::agents::protocol::is_base_agent(&agent_name) {
                continue;
            }
            match load_plugin_agent(&root.name, &agent_name, &path) {
                Ok(cfg) => out.push(cfg),
                Err(e) => tracing::warn!("skipping plugin agent '{}:{agent_name}': {e}", root.name),
            }
        }
    }
    out.sort_by(|a, b| a.agent.name.cmp(&b.agent.name));
    out
}

/// Load one plugin agent file, namespaced `<plugin>:<agent_name>`.
///
/// Why: the single load path both [`discover_plugin_agents`] (listing) and
/// [`find_plugin_agent_config`] (dispatch resolution) share, so the
/// namespacing + unsupported-field-drop + extends-warn contract is written
/// exactly once.
/// What: reads `path`, parses frontmatter via
/// `trusty_agents_common::agents::metadata::agent_metadata_from_str` (no
/// `compose_agent` call — a plugin agent is a leaf, #3539), warns and drops
/// any [`UNSUPPORTED_AGENT_FIELDS`] present (see [`warn_unsupported_fields`]),
/// warns (never errors) on a present `extends:` — the agent is still loaded
/// as a direct/leaf document with no inheritance resolved — then projects
/// via `agents::md_loader::project_to_agent_config` exactly as a disk agent
/// would, overriding only `agent.name` with the namespaced form. An
/// unreadable file is the only real error path.
/// Test: `tests::load_plugin_agent_projects_fields`,
/// `tests::load_plugin_agent_warns_and_drops_unsupported_fields`,
/// `tests::load_plugin_agent_warns_on_extends_and_treats_as_leaf`,
/// `tests::load_plugin_agent_missing_file_errors`.
pub(crate) fn load_plugin_agent(
    plugin: &str,
    agent_name: &str,
    path: &Path,
) -> anyhow::Result<AgentConfig> {
    let namespaced = format!("{plugin}:{agent_name}");
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading plugin agent {}", path.display()))?;

    warn_unsupported_fields(&namespaced, &raw);

    let meta = agent_metadata_from_str(&raw);
    if let Some(parent) = &meta.extends {
        tracing::warn!(
            "plugin agent '{namespaced}' declares extends: '{parent}' — tcode Phase 1 plugin \
             agents are leaf-only (no cross-catalog compose, #3539); loading its own \
             frontmatter/body only, the parent is not resolved"
        );
    }

    let body = extract_body(&raw);
    let mut cfg = project_to_agent_config(agent_name, meta, body);
    cfg.agent.name = namespaced;
    Ok(cfg)
}

/// Warn once (one aggregated line) when `raw`'s frontmatter declares any
/// [`UNSUPPORTED_AGENT_FIELDS`].
///
/// Why: `#3539` requires "DROP unsupported plugin fields ... with a
/// one-line warn per agent" — one line naming every dropped field, not one
/// line per field (mirrors `agents::warn_on_orphaned_toml`'s aggregation
/// convention). Uses `skills::frontmatter::parse_frontmatter`'s generic
/// flat key/value scan (reused across the agents/skills domain split
/// rather than re-implemented) purely to detect KEY PRESENCE — the
/// supported-field values themselves are parsed separately via
/// `agent_metadata_from_str`.
/// What: no-op when none of the fields are present.
/// Test: `tests::load_plugin_agent_warns_and_drops_unsupported_fields`.
fn warn_unsupported_fields(namespaced: &str, raw: &str) {
    let map = crate::skills::frontmatter::parse_frontmatter(raw);
    let dropped: Vec<&str> = UNSUPPORTED_AGENT_FIELDS
        .iter()
        .copied()
        .filter(|k| map.contains_key(*k))
        .collect();
    if !dropped.is_empty() {
        tracing::warn!(
            "plugin agent '{namespaced}' declares unsupported field(s) {} — dropped (tcode \
             Phase 1 plugin support maps role/description/model/max_tokens/tools only, #3539)",
            dropped.join(", ")
        );
    }
}

/// Resolve one namespaced `<plugin>:<agent_name>` to its [`AgentConfig`], or
/// `None` when no such plugin/agent exists.
///
/// Why: `agents::resolve_agent`'s dispatch-time entry point for a
/// namespaced name — mirrors that function's own `NotFound`
/// (nothing found, `None`) vs `Load` (found but failed to parse, `Some(Err)`)
/// distinction so its caller can build the identical error shape.
/// What: validates `plugin:agent_name` via
/// `plugins::is_valid_namespaced_name` FIRST, before any path is built
/// (code-critic PR #3547 review, HIGH 3 — the guard belongs to this
/// function, not merely to an upstream caller that happens to reject `:`
/// today) — `None` on a traversal/unsafe-charset payload like
/// `plugin:../../etc`. Then finds the [`PluginRoot`] whose resolved `name`
/// equals `plugin` (not the directory name, which may differ — see
/// [`PluginRoot`]'s doc); `None` if no such plugin. Within it, `None` if
/// `<agent_name>.md` does not exist; otherwise `Some` wrapping
/// [`load_plugin_agent`]'s result.
/// Test: `tests::find_plugin_agent_config_resolves_known_agent`,
/// `tests::find_plugin_agent_config_unknown_plugin_is_none`,
/// `tests::find_plugin_agent_config_unknown_agent_is_none`,
/// `tests::find_plugin_agent_config_rejects_traversal_name`.
pub fn find_plugin_agent_config(
    project_root: &Path,
    plugin: &str,
    agent_name: &str,
) -> Option<anyhow::Result<AgentConfig>> {
    if !super::is_valid_namespaced_name(&format!("{plugin}:{agent_name}")) {
        return None;
    }
    let root = discover_plugin_roots(project_root)
        .into_iter()
        .find(|p| p.name == plugin)?;
    let path = root.agents_dir.join(format!("{agent_name}.md"));
    if !path.exists() {
        return None;
    }
    Some(load_plugin_agent(&root.name, agent_name, &path))
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;

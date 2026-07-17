//! Renders `references/agents.md` from the bundled agent roster (source #3
//! of the issue #2913 design-research brief).
//!
//! Why: `bundle::ALL` (filtered to `agents/*.md`) plus
//! `agent_metadata::agent_metadata_from_str` reuse the exact frontmatter
//! grammar `agent_builder` uses at compose time, so this table can never
//! drift from what actually deploys — no independent parsing to keep in
//! sync.
//! What: [`render`] lists every concrete (non-`BASE-*`) bundled agent with
//! its role, `extends` chain, description, and declared `skills:`.
//! Test: `agents_render_contains_known_agent`,
//! `agents_render_excludes_base_files`.

use std::fmt::Write as _;

use trusty_mpm::core::agent_metadata::agent_metadata_from_str;
use trusty_mpm::core::bundle::ALL;

/// Render the full agent roster reference.
///
/// Why: an operator/agent choosing a delegate needs the exact declared
/// `skills:` set and `extends` chain without opening each `.md` source file.
/// What: filters `ALL` to `agents/*.md` minus the five `BASE-*` foundation
/// files (those are the shared foundation every row below extends, not
/// independently delegatable agents), sorts by stem for a stable table, and
/// renders one row per agent.
/// Test: `agents_render_contains_known_agent`,
/// `agents_render_excludes_base_files`.
pub(crate) fn render() -> String {
    let mut agents: Vec<(String, trusty_mpm::core::agent_metadata::AgentMetadata)> = ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/") && !a.rel_path.starts_with("agents/BASE-"))
        .map(|a| {
            let stem = a
                .rel_path
                .strip_prefix("agents/")
                .and_then(|s| s.strip_suffix(".md"))
                .unwrap_or(a.rel_path)
                .to_string();
            (stem, agent_metadata_from_str(a.contents))
        })
        .collect();
    agents.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut out = String::new();
    out.push_str("# Agent Roster Reference\n\n");
    out.push_str(
        "Generated from `bundle::ALL` (filtered to `agents/*.md`) + \
         `agent_metadata::agent_metadata_from_str` — the same frontmatter \
         parser `agent_builder` uses at compose time. Regenerate with \
         `tm generate capabilities`.\n\n",
    );
    out.push_str(
        "Every agent below transitively `extends: base-agent` (directly, or \
         via `base-engineer`/`base-qa`/`base-ops`/`base-research`) — see \
         `agents/BASE-AGENT.md` and its four role-specific bases for the \
         shared foundation, not repeated per row here.\n\n",
    );
    let _ = writeln!(out, "{} concrete agents.\n", agents.len());

    out.push_str(
        "| Agent | Role | Extends | Description | Declared Skills |\n|---|---|---|---|---|\n",
    );
    for (stem, meta) in &agents {
        let role = meta.role.clone().unwrap_or_default();
        let extends = meta.extends.clone().unwrap_or_default();
        let description = meta.description.clone().unwrap_or_default();
        let skills = meta.skills.join(", ");
        let _ = writeln!(
            out,
            "| `{stem}` | {role} | {extends} | {description} | {skills} |"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_render_contains_known_agent() {
        let rendered = render();
        assert!(rendered.contains("`rust-engineer`"), "{rendered}");
        assert!(rendered.contains("`code-critic`"), "{rendered}");
    }

    #[test]
    fn agents_render_excludes_base_files() {
        let rendered = render();
        assert!(!rendered.contains("`BASE-AGENT`"), "{rendered}");
        assert!(!rendered.contains("`BASE-ENGINEER`"), "{rendered}");
    }

    #[test]
    fn agents_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}

//! Renders `references/agents.md` from the bundled agent roster (source #3
//! of the issue #2913 design-research brief).
//!
//! Why: `bundle::ALL` (filtered to `agents/*.md`) plus
//! `agent_metadata::agent_metadata_from_str` reuse the exact frontmatter
//! grammar `agent_builder` uses at compose time, so this table can never
//! drift from what actually deploys — no independent parsing to keep in
//! sync. Since #4760 the DEPLOYMENT CATEGORY, and since #4765 the GATE
//! CONDITION with it, come from the bundled `framework-manifest.toml` rather
//! than being inferred here, so a reader of this reference sees WHY an agent
//! deploys — and the reference cannot disagree with the file the deployer
//! reads. This file is the rendered VIEW of that manifest; the manifest is the
//! authority.
//! What: [`render`] lists every concrete (non-`BASE-*`) bundled agent with
//! its deployment category, its declared gate, role, `extends` chain,
//! description, and declared `skills:`.
//! Test: `agents_render_contains_known_agent`,
//! `agents_render_excludes_base_files`, `agents_render_carries_categories`,
//! `agents_render_carries_markers`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use trusty_mpm::core::agent_metadata::agent_metadata_from_str;
use trusty_mpm::core::bundle::ALL;
use trusty_mpm::core::manifest::framework_agent_categories;

/// Map every declared agent stem to its deployment category and gate condition.
///
/// Why: both the category AND the condition are the manifest's answer, not this
/// renderer's — deriving either here would recreate exactly the
/// computed-by-complement definition #4760 removed, and would put a second copy
/// of the markers in a generated file, which is the drift #4765 removed.
/// What: inverts the five category lists into `stem -> (category, gate)`, where
/// `gate` is the entry's declared markers rendered as inline code, or
/// `always`/`never` for the two ungated categories. An unusable manifest yields
/// an empty map, so the columns render blank rather than aborting a
/// documentation regeneration; the deploy path is where an unusable manifest
/// fails loudly.
/// Test: `agents_render_carries_categories`, `agents_render_carries_markers`.
fn gate_by_stem() -> BTreeMap<String, (&'static str, String)> {
    let Ok(categories) = framework_agent_categories() else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    for stem in &categories.universal {
        map.insert(stem.clone(), ("universal", "always".to_string()));
    }
    for stem in &categories.deprecated {
        map.insert(stem.clone(), ("deprecated", "never".to_string()));
    }
    for (label, list) in [
        ("language", &categories.language),
        ("framework", &categories.framework),
        ("platform", &categories.platform),
    ] {
        for entry in list {
            let markers = entry
                .markers
                .iter()
                .map(|marker| format!("`{marker}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            map.insert(entry.stem.clone(), (label, markers));
        }
    }
    map
}

/// Render the full agent roster reference.
///
/// Why: an operator/agent choosing a delegate needs the exact declared
/// `skills:` set and `extends` chain without opening each `.md` source file.
/// What: filters `ALL` to `agents/*.md` minus the five `BASE-*` foundation
/// files (those are the shared foundation every row below extends, not
/// independently delegatable agents), sorts by stem for a stable table, and
/// renders one row per agent with the deployment category
/// `framework-manifest.toml` declares for it.
/// Test: `agents_render_contains_known_agent`,
/// `agents_render_excludes_base_files`, `agents_render_carries_categories`.
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
         parser `agent_builder` uses at compose time — with the **Category** \
         and **Deploys When** columns read from the bundled \
         `framework-manifest.toml`, the same file the deployer consults — this \
         table is that manifest\'s rendered view, never a second copy of it. \
         Regenerate with `tm generate capabilities`.\n\n",
    );
    out.push_str(
        "**Category** is the DEPLOYMENT gate (#4760) and **Deploys When** is \
         the condition it resolves to (#4765): `universal` deploys to every \
         project with no detection; `language`, `framework`, and `platform` \
         deploy only when one of the listed markers is present at the project \
         root or at a declared workspace member; `deprecated` never deploys. A \
         marker written `path::needle` is a bounded content probe of that \
         file. This axis is distinct from ADR-0025's four-category agent \
         model, which classifies by who authored an agent and where it lives — \
         every row below is an ADR-0025 category-1 or category-2 bundled \
         agent.\n\n",
    );
    out.push_str(
        "Every agent below transitively `extends: base-agent` (directly, or \
         via `base-engineer`/`base-qa`/`base-ops`/`base-research`) — see \
         `agents/BASE-AGENT.md` and its four role-specific bases for the \
         shared foundation, not repeated per row here.\n\n",
    );
    let _ = writeln!(out, "{} concrete agents.\n", agents.len());

    let gates = gate_by_stem();
    out.push_str(
        "| Agent | Category | Deploys When | Role | Extends | Description | Declared Skills |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for (stem, meta) in &agents {
        let (category, gate) = gates.get(stem).cloned().unwrap_or(("", String::new()));
        let role = meta.role.clone().unwrap_or_default();
        let extends = meta.extends.clone().unwrap_or_default();
        let description = meta.description.clone().unwrap_or_default();
        let skills = meta.skills.join(", ");
        let _ = writeln!(
            out,
            "| `{stem}` | {category} | {gate} | {role} | {extends} | {description} | {skills} |"
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
    fn agents_render_carries_categories() {
        // The category column must come from the manifest — pin one row of each
        // of the four gated categories plus the deprecated one.
        let rendered = render();
        for (stem, category) in [
            ("engineer", "universal"),
            ("rust-engineer", "language"),
            ("react-engineer", "framework"),
            ("vercel-ops", "platform"),
            ("elixir-engineer", "language"),
        ] {
            assert!(
                rendered.contains(&format!("| `{stem}` | {category} |")),
                "expected `{stem}` to render as {category}:\n{rendered}"
            );
        }
    }

    #[test]
    fn agents_render_carries_markers() {
        // #4765: the gate CONDITION is rendered from the manifest too, so the
        // reference states why an agent deploys without a second copy of the
        // markers living in a Rust table.
        let rendered = render();
        for (stem, gate) in [
            ("rust-engineer", "| language | `Cargo.toml` |"),
            ("engineer", "| universal | always |"),
            ("vercel-ops", "`vercel.json`"),
        ] {
            assert!(
                rendered.contains(gate),
                "expected `{stem}` to render its gate {gate}:\n{rendered}"
            );
        }
    }

    #[test]
    fn agents_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}

//! Renders `references/skills.md` from the bundled skill catalog (source #4
//! of the issue #2913 design-research brief).
//!
//! Why: `bundle::ALL` (filtered to top-level `skills/*.md` entries — nested
//! `skills/<name>/references/*.md` siblings excluded, since those are a
//! skill's own reference material, not separate catalog entries) is the same
//! canonical table [`super::agents`] reads; skills use the same
//! `---`-delimited YAML-ish frontmatter grammar as agents, so this reuses
//! [`trusty_agents_common::agents::frontmatter::parse_kv_line`] (the shared,
//! colon-safe line parser) rather than hand-rolling a second parser that
//! could silently diverge.
//! What: [`render`] lists every bundled skill with its description, category,
//! and whether it is user-invocable (`/skill-name`) or agent-reference-only.
//! Test: `skills_render_contains_known_skill`,
//! `skills_render_excludes_reference_files`.

use std::fmt::Write as _;

use trusty_agents_common::agents::frontmatter::parse_kv_line;
use trusty_mpm::core::bundle::ALL;

/// One bundled skill's frontmatter fields relevant to the catalog.
struct SkillMeta {
    description: String,
    category: String,
    user_invocable: bool,
}

/// Parse a skill `.md` document's frontmatter into [`SkillMeta`].
///
/// Why: skills declare `name`/`description`/`user-invocable`/`category`/
/// `tags`/`effort` in the same `---`-delimited block agents use, but there is
/// no shared `SkillMetadata` struct today (per the issue #2913 brief) — this
/// is the minimal, format-agnostic reuse of [`parse_kv_line`] the brief
/// recommends, scoped to exactly the four fields the catalog renders.
/// What: a best-effort parse — a missing or malformed frontmatter block
/// yields empty/`false` defaults rather than an error, matching
/// `agent_metadata_from_str`'s "never a hard dependency" convention.
/// Test: `parse_skill_frontmatter_reads_known_fields`,
/// `parse_skill_frontmatter_malformed_is_default`.
fn parse_skill_frontmatter(contents: &str) -> SkillMeta {
    let mut meta = SkillMeta {
        description: String::new(),
        category: String::new(),
        user_invocable: false,
    };

    let mut lines = contents.lines();
    if lines.next() != Some("---") {
        return meta;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = parse_kv_line(line) else {
            continue;
        };
        match key.as_str() {
            "description" => meta.description = value,
            "category" => meta.category = value,
            "user-invocable" => meta.user_invocable = value == "true",
            _ => {}
        }
    }
    meta
}

/// `true` when `rel_path` is a top-level skill entry (`skills/<name>.md`),
/// not a nested `skills/<name>/references/<file>.md` sibling.
///
/// `pub(super)`: also used by [`super::entry::render`] to compute the
/// skill-count headline without duplicating the filter predicate.
pub(super) fn is_top_level_skill(rel_path: &str) -> bool {
    match rel_path.strip_prefix("skills/") {
        Some(rest) => rest.ends_with(".md") && !rest.contains('/'),
        None => false,
    }
}

/// Render the full skill catalog reference.
///
/// Why: an operator/agent choosing which skill to load needs the exact
/// description + invocability without opening every bundled `.md` file.
/// What: filters `ALL` to top-level `skills/*.md` entries, sorts by stem,
/// and renders one row per skill.
/// Test: `skills_render_contains_known_skill`,
/// `skills_render_excludes_reference_files`.
pub(crate) fn render() -> String {
    let mut skills: Vec<(String, SkillMeta)> = ALL
        .iter()
        .filter(|a| is_top_level_skill(a.rel_path))
        .map(|a| {
            let stem = a
                .rel_path
                .strip_prefix("skills/")
                .and_then(|s| s.strip_suffix(".md"))
                .unwrap_or(a.rel_path)
                .to_string();
            (stem, parse_skill_frontmatter(a.contents))
        })
        .collect();
    skills.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut out = String::new();
    out.push_str("# Skill Catalog Reference\n\n");
    out.push_str(
        "Generated from `bundle::ALL` (filtered to top-level `skills/*.md` \
         entries) + a shared frontmatter line parser. Regenerate with \
         `tm generate capabilities`.\n\n",
    );
    let _ = writeln!(out, "{} bundled skills.\n", skills.len());

    out.push_str("| Skill | Category | User-invocable | Description |\n|---|---|---|---|\n");
    for (stem, meta) in &skills {
        let invocable = if meta.user_invocable { "yes" } else { "no" };
        let _ = writeln!(
            out,
            "| `{stem}` | {} | {invocable} | {} |",
            meta.category, meta.description
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_render_contains_known_skill() {
        let rendered = render();
        assert!(rendered.contains("`tm`"), "{rendered}");
        assert!(rendered.contains("`systematic-debugging`"), "{rendered}");
    }

    #[test]
    fn skills_render_excludes_reference_files() {
        let rendered = render();
        assert!(!rendered.contains("references/workflow"), "{rendered}");
    }

    #[test]
    fn parse_skill_frontmatter_reads_known_fields() {
        let doc = "---\nname: foo\ndescription: A test skill\nuser-invocable: true\ncategory: pm-reference\n---\n\nBody.\n";
        let meta = parse_skill_frontmatter(doc);
        assert_eq!(meta.description, "A test skill");
        assert_eq!(meta.category, "pm-reference");
        assert!(meta.user_invocable);
    }

    #[test]
    fn parse_skill_frontmatter_malformed_is_default() {
        let meta = parse_skill_frontmatter("no frontmatter here");
        assert!(meta.description.is_empty());
        assert!(!meta.user_invocable);
    }

    #[test]
    fn skills_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}

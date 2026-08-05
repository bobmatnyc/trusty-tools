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
//! Since #4765 the ROSTER is the bundled `framework-manifest.toml`'s
//! `[skill_categories]` declaration, not this renderer's own filter — the
//! manifest is the authority for which skills are bundled, and
//! `parse_framework_skills` makes an undeclared bundled skill a hard error, so
//! the two cannot disagree.
//! Test: `skills_render_contains_known_skill`,
//! `skills_render_excludes_reference_files`,
//! `skills_render_matches_the_declared_roster`.

use std::fmt::Write as _;

use trusty_agents_common::agents::frontmatter::parse_kv_line;
use trusty_mpm::core::bundle::ALL;
use trusty_mpm::core::manifest::framework_skill_categories;

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
/// What: filters `ALL` to the top-level `skills/*.md` entries the manifest
/// DECLARES, sorts by stem, and renders one row per skill. An unusable manifest
/// falls back to the unfiltered bundle so a documentation regeneration degrades
/// to "renders everything" rather than to "renders nothing"; the declaration is
/// enforced by `bundled_skill_roster_is_valid`, which fails the build first.
/// Test: `skills_render_contains_known_skill`,
/// `skills_render_excludes_reference_files`,
/// `skills_render_matches_the_declared_roster`.
pub(crate) fn render() -> String {
    let declared = framework_skill_categories()
        .map(|categories| categories.universal.into_iter().collect::<Vec<_>>())
        .ok();
    let mut skills: Vec<(String, SkillMeta)> = ALL
        .iter()
        .filter(|a| is_top_level_skill(a.rel_path))
        .filter(|a| match &declared {
            Some(list) => a
                .rel_path
                .strip_prefix("skills/")
                .and_then(|s| s.strip_suffix(".md"))
                .is_some_and(|stem| list.iter().any(|d| d == stem)),
            None => true,
        })
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
        "Generated from the bundled `framework-manifest.toml`'s \
         `[skill_categories]` roster — the authority for which skills are \
         bundled — joined to `bundle::ALL` for each skill\'s frontmatter via a \
         shared line parser. Every declared skill is `universal`: it deploys to \
         every project, with no detection. Regenerate with \
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
    fn skills_render_matches_the_declared_roster() {
        // #4765: the rendered roster is the manifest's declaration, one row per
        // declared stem — not a second filter that could drift from it.
        let declared = framework_skill_categories().expect("bundled roster must be valid");
        let rendered = render();
        assert!(
            rendered.contains(&format!("{} bundled skills.", declared.universal.len())),
            "headline count must equal the declared roster size:\n{rendered}"
        );
        for stem in &declared.universal {
            assert!(
                rendered.contains(&format!("| `{stem}` |")),
                "declared skill `{stem}` is missing from the rendered catalog"
            );
        }
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

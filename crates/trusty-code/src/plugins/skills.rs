//! Phase-1 plugin SKILL ingestion: discovery, namespacing, and body
//! resolution (issue #3539).
//!
//! Why: a plugin's `skills/<name>/SKILL.md` files are the exact same
//! progressive-disclosure format `crate::skills` already parses for project
//! skills — cheap frontmatter (`name`/`description`) at discovery time, the
//! full body only on demand. This module adds namespacing
//! (`<plugin>:<name>`) on top; it deliberately does NOT interact with
//! `crate::skills::discover_skill_metadata`'s bundled/project
//! whole-catalog-replacement threshold (PR #3465) — plugin skills are an
//! independent additive tier that is listed and resolved regardless of
//! which side of that threshold a project's own `.claude/skills/` falls on
//! (#3539's locked precedence-interaction requirement).
//! What: [`discover_plugin_skills`] lists every plugin skill's cheap
//! metadata, namespaced (used by `skills::protocol::skills_list`'s `plugin`
//! tier and `skills::FsSkillResolver`'s cached catalog).
//! [`resolve_plugin_skill_body`] lazily loads one namespaced skill's full
//! body on demand (used by `skills::FsSkillResolver::resolve`).
//! Test: `tests::*` — discovery finds and namespaces skills, a skill
//! directory without `SKILL.md` is skipped, body resolution finds/misses a
//! namespaced name, a symlinked `<name>`/`SKILL.md` is rejected at both the
//! discovery and resolution paths (issue #3547 CRITICAL 5).

use std::path::Path;

use crate::skills::SkillMetadata;

use super::{PluginRoot, discover_plugin_roots};

/// List every plugin skill's cheap metadata across every discovered plugin,
/// namespaced `<plugin>:<local-name>`.
///
/// Why: the additive `plugin` tier `skills::protocol::skills_list` and
/// `skills::FsSkillResolver`'s cached catalog both need.
/// What: for each [`PluginRoot`], scans immediate subdirectories of
/// `skills_dir` for a readable `SKILL.md`; a subdirectory without one is
/// skipped (mirrors `skills::discover_skill_metadata`'s graceful-skip
/// convention, never an error). `name`/`description` come from frontmatter,
/// falling back to the skill's directory name when `name:` is absent.
/// Sorted by the final namespaced name.
/// Test: `tests::discover_plugin_skills_namespaces_entries`,
/// `tests::discover_plugin_skills_skips_dir_without_skill_md`,
/// `tests::discover_plugin_skills_missing_plugins_dir_is_empty`.
pub fn discover_plugin_skills(project_root: &Path) -> Vec<SkillMetadata> {
    let mut out: Vec<SkillMetadata> = discover_plugin_roots(project_root)
        .iter()
        .flat_map(discover_one_plugin_skills)
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Scan a single plugin's `skills_dir` for its `<name>/SKILL.md` skills.
///
/// Why: factored out of [`discover_plugin_skills`] so the per-plugin scan
/// (mirroring `skills::load_metadata_from_skill_dir`'s shape) is
/// independently readable. This is the ONE place a plugin skill's
/// `SKILL.md` content is read for listing purposes, so it is also the
/// enforcement point for the leaf-file-identity guard (code-critic PR
/// #3547 re-review, CRITICAL 5, CWE-59) — see [`super::path_is_contained`].
/// What: a missing/unreadable `skills_dir` yields an empty `Vec` (not an
/// error — most plugins ship agents only, or vice versa). `path.is_dir()`
/// FOLLOWS symlinks (so a symlinked `<name>` directory is not rejected by
/// this check alone), but every candidate's full `<name>/SKILL.md` leaf
/// path is additionally required to canonicalize within `skills_dir`'s own
/// canonicalization before its content is read — this single check catches
/// a symlinked `<name>` directory, a symlinked `SKILL.md` leaf file inside
/// a real `<name>` directory, or both, uniformly (see
/// [`super::path_is_contained`]'s doc). A rejected candidate is skipped
/// with a `WARN`, never surfacing the escaped target's content.
/// Test: covered via `discover_plugin_skills`'s tests above,
/// `tests::discover_plugin_skills_excludes_symlinked_skill_md`.
fn discover_one_plugin_skills(root: &PluginRoot) -> Vec<SkillMetadata> {
    let Ok(entries) = std::fs::read_dir(&root.skills_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let dirname = path.file_name()?.to_str()?.to_string();
            let skill_md = path.join("SKILL.md");
            if !super::path_is_contained(&skill_md, &root.skills_dir) {
                tracing::warn!(
                    "plugin '{}' skill dir '{dirname}' SKILL.md at {} is a symlink (or resolves \
                     through one) outside the plugin's skills directory — rejected as a \
                     potential host-file disclosure (#3547)",
                    root.name,
                    skill_md.display()
                );
                return None;
            }
            let raw = std::fs::read_to_string(&skill_md).ok()?;
            let front = crate::skills::frontmatter::parse_frontmatter(&raw);
            let local_name = front.get("name").cloned().unwrap_or(dirname);
            let description = front.get("description").cloned().unwrap_or_default();
            Some(SkillMetadata {
                name: format!("{}:{local_name}", root.name),
                description,
            })
        })
        .collect()
}

/// Lazily load one namespaced plugin skill's full body (frontmatter
/// stripped), or `None` when the plugin or the skill is unknown.
///
/// Why: `skills::FsSkillResolver::resolve`'s namespaced-name entry point —
/// the "on invoke" half of progressive disclosure for a plugin skill,
/// mirroring `skills::load_skill_body`'s lazy-load contract. This is the
/// LLM-reachable path — `tools::skill::UseSkillTool::execute` forwards the
/// model's raw `name` argument straight through
/// `skills::FsSkillResolver::resolve` to here — so `skill_name` is HOSTILE
/// input (code-critic PR #3547 review, CRITICAL 2).
/// What: validates `plugin:skill_name` via `plugins::is_valid_namespaced_name`
/// FIRST, before any path is built — rejects a traversal/unsafe-charset
/// payload like `skill_name = "../../../../x"` outright (this alone makes
/// escaping `skills_dir` syntactically impossible, since the accepted
/// charset contains no `/` or `.`). Then finds the [`PluginRoot`] whose
/// resolved `name` equals `plugin`; as a second, independent guard,
/// additionally requires `<plugin>:<skill_name>` to be a name
/// [`discover_plugin_skills`] actually found on disk this scan — mirroring
/// `skills::load_skill_body`'s "reject anything not in `known`" contract —
/// which also closes the residual gap where a plugin's own `SKILL.md`
/// frontmatter could declare a spoofed `name:` that passed the charset
/// check but was never a real, scanned skill. Because
/// [`discover_one_plugin_skills`] itself now rejects a symlinked
/// `<name>`/`SKILL.md` (code-critic PR #3547 re-review, CRITICAL 5), a
/// symlinked skill is never "found on disk" and this membership check
/// alone already excludes it — a THIRD, independent
/// [`super::path_is_contained`] check is still applied immediately before
/// the read below as defense in depth, so the guard holds even if the two
/// callers of [`discover_one_plugin_skills`] ever drift. Only once all
/// guards pass does it read `<skills_dir>/<skill_name>/SKILL.md` and strip
/// the frontmatter fence via `skills::frontmatter::strip_frontmatter` (the
/// same generic stripper project/embedded skill bodies use). `None` on any
/// miss (failed validation, unknown plugin, unknown/symlinked skill,
/// unreadable file) — the resolver trait's `Option`-returning API has no
/// separate error channel, matching `FsSkillResolver::resolve`'s existing
/// unknown-name handling.
/// Test: `tests::resolve_plugin_skill_body_returns_body`,
/// `tests::resolve_plugin_skill_body_unknown_plugin_is_none`,
/// `tests::resolve_plugin_skill_body_unknown_skill_is_none`,
/// `tests::resolve_plugin_skill_body_rejects_traversal_name`,
/// `tests::resolve_plugin_skill_body_rejects_symlinked_file`.
pub fn resolve_plugin_skill_body(
    project_root: &Path,
    plugin: &str,
    skill_name: &str,
) -> Option<String> {
    if !super::is_valid_namespaced_name(&format!("{plugin}:{skill_name}")) {
        return None;
    }
    let root = discover_plugin_roots(project_root)
        .into_iter()
        .find(|p| p.name == plugin)?;

    let namespaced = format!("{}:{skill_name}", root.name);
    if !discover_one_plugin_skills(&root)
        .iter()
        .any(|m| m.name == namespaced)
    {
        return None;
    }

    let skill_md = root.skills_dir.join(skill_name).join("SKILL.md");
    if !super::path_is_contained(&skill_md, &root.skills_dir) {
        tracing::warn!(
            "plugin skill '{namespaced}' SKILL.md at {} is a symlink (or resolves through one) \
             outside the plugin's skills directory — rejected as a potential host-file \
             disclosure (#3547)",
            skill_md.display()
        );
        return None;
    }

    let raw = std::fs::read_to_string(&skill_md).ok()?;
    Some(
        crate::skills::frontmatter::strip_frontmatter(&raw)
            .trim()
            .to_string(),
    )
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;

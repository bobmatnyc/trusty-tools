//! Filesystem discovery, frontmatter parsing, and search-path policy for the
//! skill registry (#363 split from `registry/mod.rs`).
//!
//! Why: The recursive directory walk, the "external repo" guard, the minimal
//! frontmatter parser, and the search-path policy are all I/O-facing helpers
//! that the registry's `load` orchestrates. Keeping them in their own module
//! isolates the filesystem concerns from the in-memory ranking logic.
//! What: Exposes `visit_dir`, `looks_like_external_skill_dir`,
//! `parse_skill_meta`, the `ParseSkillError` enum, and the public
//! `skill_search_paths` / `skill_index_path` policy functions.
//! Test: `registry_skips_files_without_frontmatter`, `skill_search_paths_order`,
//! `looks_like_external_skill_dir_passes_trusty_agents_layout`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;

use super::meta::{
    LARGE_DIR_MD_THRESHOLD, MAX_SKILLS_PER_SOURCE, PER_SOURCE_SCAN_TIMEOUT, SkillMeta,
    default_effectiveness,
};

/// Detect "this is an external skill repo, skip it" (#184).
///
/// Why: claude-mpm's `~/.claude/skills/` directory has 700+ markdown files
/// organized in subdirectories with no `.toml` manifests — scanning it
/// recursively at every trusty-agents startup hangs for tens of minutes. Operators
/// who genuinely want those skills indexed should add the path explicitly to
/// `.trusty-agents/skill-sources.toml` so the opt-in is visible.
/// What: Returns `true` when the directory contains `>= LARGE_DIR_MD_THRESHOLD`
/// `.md` files (anywhere in the tree, sampled with an early-exit walk) AND
/// no `*.toml` skill manifests at the top level. The check itself is bounded
/// by both the count threshold and `PER_SOURCE_SCAN_TIMEOUT` so a giant tree
/// can't make the *probe* hang either.
/// Test: `looks_like_external_skill_dir_flags_claude_skills_layout`,
/// `looks_like_external_skill_dir_passes_trusty_agents_layout`.
pub(super) fn looks_like_external_skill_dir(dir: &Path) -> bool {
    // Top-level TOML manifest = "this is an trusty-agents-shaped source".
    let has_toml = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("toml")),
        Err(_) => return false,
    };
    if has_toml {
        return false;
    }
    // Otherwise, count `.md` files (with budgets so the probe itself is cheap).
    let started = Instant::now();
    let mut count = 0usize;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if started.elapsed() >= PER_SOURCE_SCAN_TIMEOUT {
            // Probe ran out of time; assume "external" so we don't hang the
            // real scan downstream.
            return true;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                count += 1;
                if count >= LARGE_DIR_MD_THRESHOLD {
                    return true;
                }
            }
        }
    }
    false
}

/// Recurse through `dir` inserting every `.md` file as a skill entry.
///
/// Why: Bundled skills live in nested subdirectories (`languages/rust.md`,
/// `frameworks/fastapi.md`, `workflow/tdd.md`); a recursive walk keeps the
/// search-path config flat (one dir per source) while still picking up
/// organized layouts.
/// What: Silently skips unreadable entries; logs WARN on malformed
/// frontmatter. First writer wins on name conflict inside the same source.
pub(super) fn visit_dir(
    dir: &Path,
    skills: &mut IndexMap<String, SkillMeta>,
    source_root: &Path,
    started: &Instant,
) {
    // #184: Count skills already loaded from THIS source root so we can
    // enforce `MAX_SKILLS_PER_SOURCE` without breaking earlier sources.
    let source_root_owned = source_root.to_path_buf();
    fn count_for_root(skills: &IndexMap<String, SkillMeta>, root: &Path) -> usize {
        let root_str = root.to_string_lossy();
        skills
            .values()
            .filter(|m| {
                m.source_path
                    .to_string_lossy()
                    .starts_with(root_str.as_ref())
            })
            .count()
    }
    if count_for_root(skills, &source_root_owned) >= MAX_SKILLS_PER_SOURCE {
        return;
    }
    if started.elapsed() >= PER_SOURCE_SCAN_TIMEOUT {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "failed to read skill dir");
            return;
        }
    };
    for entry in entries.flatten() {
        // #184: Re-check budgets each iteration so a deep tree can't blow past
        // the cap or timeout silently.
        if count_for_root(skills, &source_root_owned) >= MAX_SKILLS_PER_SOURCE {
            tracing::debug!(
                source = %source_root.display(),
                cap = MAX_SKILLS_PER_SOURCE,
                "skill source hit per-source cap; skipping remaining files"
            );
            return;
        }
        if started.elapsed() >= PER_SOURCE_SCAN_TIMEOUT {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            visit_dir(&path, skills, source_root, started);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match parse_skill_meta(&path) {
            Ok(meta) => {
                if skills.contains_key(&meta.name) {
                    tracing::debug!(
                        skill = %meta.name,
                        shadowed = %path.display(),
                        "lower-priority skill shadowed by earlier dir"
                    );
                    continue;
                }
                tracing::debug!(
                    skill = %meta.name,
                    source = %path.display(),
                    "discovered skill"
                );
                skills.insert(meta.name.clone(), meta);
            }
            Err(ParseSkillError::MissingFrontmatter) => {
                tracing::warn!(
                    path = %path.display(),
                    "skill file has no YAML frontmatter; skipping"
                );
            }
            Err(ParseSkillError::MissingField(field)) => {
                tracing::warn!(
                    path = %path.display(),
                    field = %field,
                    "skill file missing required frontmatter field; skipping"
                );
            }
            Err(ParseSkillError::Io(e)) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read skill file; skipping"
                );
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum ParseSkillError {
    Io(std::io::Error),
    MissingFrontmatter,
    MissingField(&'static str),
}

/// Parse `name`, `description`, `tags` out of a skill file's frontmatter.
///
/// Why: The YAML frontmatter we emit is dead simple (flat keys, one inline
/// list); pulling in a full YAML parser just for three keys is overkill and
/// would widen the dependency tree. A hand-rolled parser keeps the build fast
/// and the dependency graph tight.
/// What: Reads the file, locates the `---` / `---` fence block, extracts the
/// three keys, trims quotes. Returns `MissingField` when `name` or `tags` is
/// absent (those two are required so the registry can index by them).
/// Test: `registry_skips_files_without_frontmatter`.
pub(super) fn parse_skill_meta(path: &Path) -> Result<SkillMeta, ParseSkillError> {
    let content = std::fs::read_to_string(path).map_err(ParseSkillError::Io)?;
    let fm = extract_frontmatter(&content).ok_or(ParseSkillError::MissingFrontmatter)?;
    let name = extract_value(fm, "name").ok_or(ParseSkillError::MissingField("name"))?;
    let description = extract_value(fm, "description").unwrap_or_default();
    let tags = extract_list(fm, "tags");
    if tags.is_empty() {
        return Err(ParseSkillError::MissingField("tags"));
    }
    Ok(SkillMeta {
        name,
        description,
        tags,
        source_path: path.to_path_buf(),
        effectiveness_score: default_effectiveness(),
        use_count: 0,
        last_used: None,
    })
}

/// Return the text between the opening and closing `---` fences, or `None`.
fn extract_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end_rel = rest.find("\n---")?;
    Some(&rest[..end_rel])
}

fn extract_value(fm: &str, key: &str) -> Option<String> {
    for line in fm.lines() {
        let trimmed = line.trim();
        let prefix = format!("{key}:");
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_list(fm: &str, key: &str) -> Vec<String> {
    for line in fm.lines() {
        let trimmed = line.trim();
        let prefix = format!("{key}:");
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim();
            if let Some(inner) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                return inner
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Compute skill-source search paths in priority order (highest first).
///
/// Why: Centralizes the discovery policy so `main.rs`, `skills list`, and
/// future integrations all agree on where to look. Mirrors
/// `agents::registry::agent_search_paths`. The sibling `trusty-common/skills`
/// directory (when present alongside this repo) is included so cross-project
/// skill libraries authored in trusty-common are visible to trusty-agents without
/// duplicating files.
/// What: Returns, in order: `.trusty-agents/skills`, `.claude/skills`,
/// `../trusty-common/skills` (sibling repo, if it exists),
/// `~/.trusty-agents/skills`, `~/.claude/skills`, `<config_dir>/skills`.
/// Test: `skill_search_paths_order`.
pub fn skill_search_paths(config_dir: &Path) -> Vec<PathBuf> {
    // #184: When `TAGENT_SKILLS_PROJECT_LOCAL_ONLY=1` (set by CTRL for its
    // lightweight LLM turns), restrict discovery to the project-local
    // `.trusty-agents/skills` directory and the bundled fallback. This skips
    // `~/.claude/skills/` (claude-mpm's 700+-file repo) which previously
    // hung CTRL's startup for 30+ minutes.
    if crate::env_compat::env_var(
        "TAGENT_SKILLS_PROJECT_LOCAL_ONLY",
        "OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY",
    )
    .ok()
    .filter(|v| !v.is_empty() && v != "0")
    .is_some()
    {
        return vec![
            PathBuf::from(".trusty-agents/skills"),
            config_dir.join("skills"),
        ];
    }
    let mut paths = Vec::new();
    paths.push(PathBuf::from(".trusty-agents/skills"));
    paths.push(PathBuf::from(".claude/skills"));
    // Sibling `trusty-common/skills` repo: cross-project shared skill library.
    // Only included when the directory actually exists so users without the
    // sibling checkout don't see warnings. Project-local skills (above) still
    // win on name collisions.
    let trusty_common = PathBuf::from("../trusty-common/skills");
    if trusty_common.is_dir() {
        paths.push(trusty_common);
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home.clone()).join(".trusty-agents/skills"));
        paths.push(PathBuf::from(home).join(".claude/skills"));
    }
    paths.push(config_dir.join("skills"));
    paths
}

/// Canonical path of the persisted skill effectiveness index (#171).
///
/// Why: Centralizes the `~/.trusty-agents/skills/index.json` location so startup
/// (merge_index) and post-run (save_index) callers agree on the same file.
/// What: Returns `~/.trusty-agents/skills/index.json` when `$HOME` is set, else
/// `.trusty-agents/skills/index.json` relative to the CWD as a fallback.
/// Test: Indirect via `merge_index_restores_effectiveness_after_reload`.
pub fn skill_index_path() -> PathBuf {
    let base = if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".trusty-agents").join("skills")
    } else {
        PathBuf::from(".trusty-agents").join("skills")
    };
    base.join("index.json")
}

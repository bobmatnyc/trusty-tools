//! Authored skill manifests: `.md` frontmatter that names the tool it wraps.
//!
//! Why: This closes the exact gap DOC-57 §5.1 opened the spec with.
//! `.trusty-agents/skills/web-search.md` documents `web_search`, its backends
//! and its API keys — while `[tools].allow` grants `web_search` independently.
//! Delete the skill and the tool works unexplained; delete the grant and the
//! skill promises a capability the agent does not have. Two declarations, no
//! link, either one silently wrong. A `tools:` key in that frontmatter IS the
//! link, and it is why an authored manifest supersedes the built-in row: the
//! prose and the binding then come from one file.
//! What: [`load_from_paths`] scans skill-source directories for `.md` files
//! whose frontmatter carries `tools:`, and returns one [`SkillManifest`] per
//! wrapped tool. Files without `tools:` are ignored here (they remain ordinary
//! prompt-injection skills — this module adds a binding, it does not take one
//! away). Parsing is best-effort and total: an unreadable or malformed file is
//! skipped with a warn, never an error, because a hand-edited skill file must
//! not stop an agent from booting.
//! Test: `super::tests::authored_manifest_overrides_builtin_by_tool`,
//! `authored_file_without_tools_key_is_ignored`.

use std::path::{Path, PathBuf};

use super::{SkillKind, SkillManifest, SkillOrigin};

/// Load every authored manifest found under the given skill-source paths.
///
/// Why: `SkillSourceRegistry::resolved_paths()` already computes the
/// priority-ordered directory list; taking `&[PathBuf]` rather than the registry
/// keeps this function pure enough to test against a `tempdir` with no config
/// file at all.
/// What: Scans each directory non-recursively for `*.md`, parses frontmatter,
/// and keeps the files carrying a non-empty `tools:` list. Later paths do NOT
/// override earlier ones — the caller passes them highest-priority-first and
/// [`super::SkillCatalog::with_authored`] resolves collisions by tool.
/// Test: `authored_scan_reads_tools_key`, `authored_scan_skips_missing_dir`.
pub fn load_from_paths(paths: &[PathBuf]) -> Vec<SkillManifest> {
    let mut out = Vec::new();
    for dir in paths {
        let Ok(entries) = std::fs::read_dir(dir) else {
            tracing::debug!(dir = %dir.display(), "skill source dir not readable; skipped");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => out.extend(parse(&path, &content)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skill file unreadable");
                }
            }
        }
    }
    out
}

/// Parse one skill file into zero or more manifests.
///
/// Why: A file listing two tools under `tools:` would break the 1:1 rule if it
/// produced one skill; it instead produces one manifest PER tool, each named
/// `"<name> — <tool>"` so the cards stay distinguishable. A one-tool file — the
/// expected shape — keeps its authored name verbatim.
///
/// `display_name` exists because `name` is load-bearing elsewhere: it is the key
/// the prompt-injection registry indexes by (`skills/registry/scan.rs`), so a
/// migrating skill cannot rename itself to something human without breaking
/// `load_skill("<old-name>")`. `display_name` carries the pane's name and leaves
/// `name` alone.
/// What: Requires `name`; `display_name` falls back to `name`; `description`
/// defaults to empty; `kind` defaults to `action`. Returns empty when there is
/// no frontmatter, no `name`, or no non-empty `tools:` list.
/// Test: `authored_scan_reads_tools_key`,
/// `authored_display_name_overrides_the_registry_name`,
/// `authored_multi_tool_file_splits_into_one_skill_per_tool`.
fn parse(path: &Path, content: &str) -> Vec<SkillManifest> {
    let Some(fm) = frontmatter(content) else {
        return Vec::new();
    };
    let Some(name) = value(fm, "name") else {
        return Vec::new();
    };
    let name = value(fm, "display_name").unwrap_or(name);
    let declared = list(fm, "tools");
    if declared.is_empty() {
        return Vec::new();
    }
    // PRIVILEGE GATE (see `is_exact_tool_name`). A `.md` in any skill source is
    // ordinary content — it is not reviewed like `agent.toml` — yet its `tools:`
    // entries are merged into the patterns `match_any_glob` consumes. An entry
    // of `*` or `mcp_*` would therefore let one innocuous `[skills].allow` id
    // grant wildcard authority behind a card that renders as a single narrow
    // capability: strictly worse than the raw glob, because `agent.toml` shows
    // the reviewer no wildcard at all. DOC-57 S-1 already forbids globs here;
    // this is the enforcement.
    //
    // The whole manifest is dropped rather than silently narrowed to its valid
    // entries: a file asking for a wildcard is either hostile or badly wrong,
    // and partially honouring it would leave an author believing a grant that
    // was quietly rewritten. Loud in the log, absent from the catalog.
    if let Some(bad) = declared.iter().find(|t| !super::is_exact_tool_name(t)) {
        tracing::warn!(
            path = %path.display(),
            tool = %bad,
            "skill manifest declares a glob-shaped `tools:` entry; entries must be \
             exact tool names (DOC-57 S-1). Manifest ignored."
        );
        return Vec::new();
    }
    let tools = declared;
    let description = value(fm, "description").unwrap_or_default();
    let kind = match value(fm, "kind").as_deref() {
        Some("knowledge") => SkillKind::Knowledge,
        Some("system") => SkillKind::System,
        _ => SkillKind::Action,
    };
    let id_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();
    let multi = tools.len() > 1;
    tools
        .into_iter()
        .map(|tool| SkillManifest {
            id: if multi {
                format!("{id_stem}:{tool}")
            } else {
                id_stem.clone()
            },
            name: if multi {
                format!("{name} — {tool}")
            } else {
                name.clone()
            },
            description: description.clone(),
            kind,
            tools: vec![tool],
            provider: None,
            origin: SkillOrigin::Authored {
                path: path.display().to_string(),
            },
        })
        .collect()
}

/// Return the text between the opening and closing `---` fences, or `None`.
///
/// Why/What: Mirrors `skills::registry::scan::extract_frontmatter` rather than
/// pulling in a YAML parser — the frontmatter dialect is flat keys plus inline
/// or dashed lists, and a third dependency for three keys is not worth it.
/// Test: `authored_file_without_frontmatter_is_ignored`.
fn frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Read a scalar frontmatter key, trimming surrounding quotes.
fn value(fm: &str, key: &str) -> Option<String> {
    for line in fm.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(key)
            && let Some(rest) = rest.strip_prefix(':')
        {
            let v = rest.trim().trim_matches(['"', '\'']).trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Read a frontmatter list key in either inline (`[a, b]`) or dashed form.
///
/// Why: Both shapes appear in the checked-in skill corpus (`tags: [a, b]` is
/// the common one; DOC-57's `tools:` example uses the dashed form), so the
/// parser accepts both rather than forcing authors to remember which.
/// What: Returns the trimmed, unquoted entries; empty when the key is absent.
/// Test: `authored_scan_reads_dashed_tools_list`.
fn list(fm: &str, key: &str) -> Vec<String> {
    let mut lines = fm.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix(key)
            .and_then(|r| r.strip_prefix(':'))
            .map(str::trim)
        else {
            continue;
        };
        if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            return inner
                .split(',')
                .map(|s| s.trim().trim_matches(['"', '\'']).to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if rest.is_empty() {
            let mut out = Vec::new();
            for next in lines.by_ref() {
                let Some(item) = next.trim().strip_prefix("- ") else {
                    break;
                };
                let item = item.trim().trim_matches(['"', '\'']).trim();
                if !item.is_empty() {
                    out.push(item.to_string());
                }
            }
            return out;
        }
    }
    Vec::new()
}

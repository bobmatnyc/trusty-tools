//! Skill index records and the relevance-ranking `SkillRegistry` (#363 split
//! from `skills/mod.rs`).
//!
//! Why: `SkillEntry` + the substring-ranking `SkillRegistry` form the
//! lightweight discovery layer used by the `list_skills`/`load_skill` tools and
//! by automatic per-task injection. Isolating them from the LLM selector and
//! the workflow `SkillsLoader` keeps each concern independently testable.
//! What: Defines `SkillEntry`, `SkillRegistry`, the minimal YAML frontmatter
//! parser (`parse_skill_file`), and the public `strip_frontmatter` helper.
//! Test: See the unit tests in `skills/mod_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Indexed record for one Markdown skill file.
///
/// Why: `SkillRegistry::search` ranks candidates without re-reading files;
/// holding tags/description in memory keeps lookups cheap and lets file IO
/// happen only when a skill is actually loaded.
/// What: Name, short description, tag list, and absolute path to the `.md`
/// file on disk. All fields originate from the YAML frontmatter if present.
/// Test: `parse_skill_file_extracts_frontmatter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub path: PathBuf,
}

impl SkillEntry {
    /// Score relevance of this skill to a lowercase query string (0.0..=1.0).
    ///
    /// Why: Agents need a lightweight ranker that works without an embedding
    /// model — exact substring matches across name/description/tags are enough
    /// to surface the right skill for the task at hand and avoid wasting
    /// context window on irrelevant skills.
    /// What: Splits `query` on whitespace; each word contributes +0.4 for a
    /// name hit, +0.2 for a description hit, +0.4 for any tag hit. Sum is
    /// capped at 1.0.
    /// Test: `registry_search_ranks_by_tag_and_description`.
    pub fn relevance_score(&self, query: &str) -> f32 {
        let q = query.to_lowercase();
        let name_lc = self.name.to_lowercase();
        let desc_lc = self.description.to_lowercase();
        let tags_lc: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();
        let mut score = 0.0f32;
        for word in q.split_whitespace() {
            if word.is_empty() {
                continue;
            }
            if name_lc.contains(word) {
                score += 0.4;
            }
            if desc_lc.contains(word) {
                score += 0.2;
            }
            if tags_lc.iter().any(|t| t.contains(word)) {
                score += 0.4;
            }
        }
        score.min(1.0)
    }
}

/// In-memory index of all skills discoverable on disk.
///
/// Why: Replaces ad-hoc reads of `config/skills/*.md` with a single scan-once
/// structure that the workflow engine and tool executors share. Keeps the
/// hot path (relevance ranking) allocation-free relative to the file count.
/// What: Holds a `Vec<SkillEntry>` built by `load`. Read-only after load.
/// Test: `registry_load_skips_missing_dir`, `registry_search_ranks_*`.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    pub skills: Vec<SkillEntry>,
}

impl SkillRegistry {
    /// Build an empty registry. Useful for tests and graceful fallback.
    pub fn empty() -> Self {
        Self { skills: Vec::new() }
    }

    /// Scan `skills_dir` for `*.md` files and build the index.
    ///
    /// Why: Graceful degradation is critical — a missing `config/skills/` dir
    /// (common on first run or in tests) must not abort startup; unreadable
    /// files must not prevent the rest from loading.
    /// What: If `skills_dir` does not exist, logs debug and returns an empty
    /// registry. Otherwise reads every `.md` file, parses minimal frontmatter,
    /// and pushes one `SkillEntry` per file. Read errors on individual files
    /// are logged at `warn` and the file is skipped.
    /// Test: `registry_load_skips_missing_dir`, `registry_load_parses_files`.
    pub async fn load(skills_dir: &Path) -> anyhow::Result<Self> {
        let mut skills: Vec<SkillEntry> = Vec::new();

        if !skills_dir.exists() {
            tracing::debug!(
                dir = %skills_dir.display(),
                "skills dir not found; registry will be empty"
            );
            return Ok(Self { skills });
        }

        let mut entries = tokio::fs::read_dir(skills_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let skill = parse_skill_file(&path, &content);
                    tracing::debug!(
                        name = %skill.name,
                        tags = ?skill.tags,
                        "skill loaded"
                    );
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to read skill file; skipping"
                    );
                }
            }
        }

        tracing::info!(count = skills.len(), "skill registry loaded");
        Ok(Self { skills })
    }

    /// Return up to `top_n` skills whose relevance score is > 0, highest first.
    ///
    /// Why: Bounds auto-injection to the most relevant skills and keeps the
    /// tool responses short when an agent calls `load_skill` with a query.
    /// What: Empty registry returns an empty vector; ties are broken by
    /// insertion order (stable sort not required but consistent with
    /// partial_cmp on equal scores).
    /// Test: `registry_search_ranks_by_tag_and_description`.
    pub fn search(&self, query: &str, top_n: usize) -> Vec<&SkillEntry> {
        if self.skills.is_empty() || top_n == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(&SkillEntry, f32)> = self
            .skills
            .iter()
            .map(|s| (s, s.relevance_score(query)))
            .filter(|(_, score)| *score > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(top_n).map(|(s, _)| s).collect()
    }

    /// Load and return the full Markdown content of a skill by exact name.
    pub async fn load_content(&self, name: &str) -> anyhow::Result<String> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow::anyhow!("skill not found: {name}"))?;
        Ok(tokio::fs::read_to_string(&skill.path).await?)
    }

    /// Render the full index as a human-readable bulleted list for LLM tools.
    pub fn format_index(&self) -> String {
        if self.skills.is_empty() {
            return "No skills available.".to_string();
        }
        self.skills
            .iter()
            .map(|s| {
                format!(
                    "**{}** — {} [tags: {}]",
                    s.name,
                    if s.description.is_empty() {
                        "(no description)"
                    } else {
                        s.description.as_str()
                    },
                    s.tags.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Build a prompt prefix with up to `max_skills` skill bodies relevant to `task`.
    ///
    /// Why: The workflow engine calls this to transparently inject relevant
    /// domain knowledge into each phase's prompt without the agent needing to
    /// call a tool first, trading a small prompt tax for a much higher hit
    /// rate on framework-specific guidance.
    /// What: Returns an empty string when no skills match; otherwise a Markdown
    /// block titled "## Relevant Skills" containing each matched skill's body
    /// (with frontmatter stripped).
    /// Test: `auto_inject_builds_prefix_when_matches_exist`,
    /// `auto_inject_returns_empty_when_no_matches`.
    pub async fn auto_inject(&self, task: &str, max_skills: usize) -> String {
        let matches = self.search(task, max_skills);
        if matches.is_empty() {
            return String::new();
        }

        let mut sections: Vec<String> = Vec::with_capacity(matches.len() + 1);
        sections.push("## Relevant Skills".to_string());
        for skill in matches {
            match tokio::fs::read_to_string(&skill.path).await {
                Ok(content) => {
                    let body = strip_frontmatter(&content);
                    sections.push(format!("### Skill: {}\n{}", skill.name, body));
                }
                Err(e) => {
                    tracing::warn!(
                        name = %skill.name,
                        error = %e,
                        "auto_inject: failed to read skill body; skipping"
                    );
                }
            }
        }
        sections.join("\n\n")
    }

    /// Load skills from project-local AND global discovery paths.
    ///
    /// Why: The global skills cache at `~/.open-mpm/skills/files/` and
    /// `~/Projects/skillset-mcp` contain cross-project skills that should be
    /// available to any project without duplicating files. Project-local skills
    /// shadow global ones with the same name so local overrides always win.
    /// What: Loads project-local `.open-mpm/skills/` first (highest priority);
    /// then appends entries from `~/.open-mpm/skills/files/` and
    /// `~/Projects/skillset-mcp` that do not already exist by name.
    /// Test: Write a skill in `~/.open-mpm/skills/files/`; call with a project
    /// that has no local skill by that name; assert the global skill appears.
    pub async fn load_with_global_cache(project_dir: &Path) -> anyhow::Result<Self> {
        // Project-local skills have highest priority.
        let local_dir = project_dir.join(".open-mpm").join("skills");
        let mut registry = Self::load(&local_dir).await?;

        // Global discovery paths, in priority order after local.
        let home = dirs::home_dir().unwrap_or_default();
        let global_dirs = [
            home.join(".open-mpm").join("skills").join("files"),
            home.join("Projects").join("skillset-mcp"),
        ];

        for dir in &global_dirs {
            if !dir.exists() {
                continue;
            }
            match Self::load(dir).await {
                Ok(global) => {
                    for skill in global.skills {
                        // Project-local wins on name conflict.
                        let already = registry.skills.iter().any(|s| s.name == skill.name);
                        if !already {
                            registry.skills.push(skill);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "global skills: skipping unreadable directory"
                    );
                }
            }
        }

        tracing::info!(
            count = registry.skills.len(),
            "skill registry loaded (project + global paths)"
        );
        Ok(registry)
    }

    /// Scan claude-mpm skill directories and merge them into the registry.
    ///
    /// Why: claude-mpm deploys skills as bare `.md` files under
    /// `~/.claude/skills/` (user-level) and `.claude/skills/` (project-level).
    /// Picking them up dynamically removes the need to copy or symlink into
    /// `config/skills/` to get them recognized.
    /// What: Loads existing skills from `skills_dir` first; then appends
    /// entries from `~/.claude/skills/` (lower priority) and
    /// `<project_dir>/.claude/skills/` (higher priority). Already-registered
    /// skill names are not overwritten — earlier (higher-priority) entries win.
    /// Test: Indirect; covered by `load_additional_dir` plus existing load tests.
    #[allow(dead_code)]
    pub async fn load_with_claude_mpm_skills(skills_dir: &Path, project_dir: &Path) -> Self {
        let mut registry = Self::load(skills_dir).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "skills: load failed; starting empty");
            Self::empty()
        });

        // Project-level `.claude/skills/` has highest priority for claude-mpm
        // content, so load it first.
        registry
            .load_additional_dir(&project_dir.join(".claude").join("skills"))
            .await;

        // User-level `~/.claude/skills/` fills in anything still missing.
        let home = dirs::home_dir().unwrap_or_default();
        registry
            .load_additional_dir(&home.join(".claude").join("skills"))
            .await;

        tracing::info!(
            count = registry.skills.len(),
            "skill registry loaded (project + claude-mpm paths)"
        );
        registry
    }

    /// Merge `.md` files from `dir` into this registry without overwriting.
    ///
    /// Why: Layered sources (project > user > global) all need the same
    /// "first writer wins" rule so higher-priority loaders can run first.
    /// What: Silently skips missing/unreadable directories and non-`.md`
    /// files. For each markdown file, parses as a skill keyed by the parsed
    /// name (or file stem when frontmatter is absent) and inserts only if
    /// the name is not already present.
    /// Test: `load_additional_dir_respects_existing`.
    #[allow(dead_code)]
    pub async fn load_additional_dir(&mut self, dir: &Path) {
        if !dir.exists() {
            return;
        }
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "skills: read_dir failed");
                return;
            }
        };
        loop {
            let next = match entries.next_entry().await {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!(error = %e, "skills: dir iter error");
                    break;
                }
            };
            let path = next.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let skill = parse_skill_file(&path, &content);
                    if self.skills.iter().any(|s| s.name == skill.name) {
                        continue;
                    }
                    tracing::debug!(
                        name = %skill.name,
                        source = %path.display(),
                        "skills: added from claude-mpm dir"
                    );
                    self.skills.push(skill);
                }
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "skills: read failed");
                }
            }
        }
    }

    /// Return the number of indexed skills.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// True if the registry has no skills.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Parse a skill file on disk into an in-memory record.
///
/// Why: Centralizes the (minimal) YAML frontmatter parser so tests can drive
/// it directly without touching the filesystem.
/// What: If `content` starts with `---` and contains a second `---`, the block
/// between is treated as frontmatter and searched for `name`, `description`,
/// and `tags` keys. Missing keys default to filename / empty string / empty
/// list.
/// Test: `parse_skill_file_extracts_frontmatter`,
/// `parse_skill_file_missing_frontmatter_uses_filename`.
pub(super) fn parse_skill_file(path: &Path, content: &str) -> SkillEntry {
    if let Some(fm) = extract_frontmatter(content) {
        let name = extract_fm_value(fm, "name").unwrap_or_else(|| path_to_name(path));
        let description = extract_fm_value(fm, "description").unwrap_or_default();
        let tags = extract_fm_list(fm, "tags");
        return SkillEntry {
            name,
            description,
            tags,
            path: path.to_path_buf(),
        };
    }

    SkillEntry {
        name: path_to_name(path),
        description: String::new(),
        tags: Vec::new(),
        path: path.to_path_buf(),
    }
}

/// Return the text between the opening and closing `---` fences, or `None`
/// when frontmatter is absent / malformed.
fn extract_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    // Accept `---\n` or `---\r\n` after the opening fence.
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    // Find the next `---` on its own line (with optional trailing newline).
    let end_rel = rest.find("\n---")?;
    Some(&rest[..end_rel])
}

fn extract_fm_value(fm: &str, key: &str) -> Option<String> {
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

fn extract_fm_list(fm: &str, key: &str) -> Vec<String> {
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

/// Strip the leading YAML frontmatter block (if any) and return the remaining
/// Markdown body. Exposed publicly so the `load_skill` tool can render clean
/// bodies without duplicating the parser.
pub fn strip_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    let Some(rest) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return content;
    };
    // Find the closing `---` fence.
    match rest.find("\n---") {
        Some(idx) => {
            // Skip past `\n---` (4 chars) and any trailing newline.
            let after = &rest[idx + 4..];
            after
                .strip_prefix('\n')
                .or_else(|| after.strip_prefix("\r\n"))
                .unwrap_or(after)
        }
        None => content,
    }
}

fn path_to_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

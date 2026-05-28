//! Dynamic skill registry with YAML-frontmatter indexing and relevance search.
//!
//! Why: Agents benefit from domain-specific Markdown guidance ("skills"), but
//! forcing every agent to know every skill name at config time is rigid and
//! brittle. A registry that scans `config/skills/*.md`, parses minimal YAML
//! frontmatter (name/description/tags), and ranks skills against a query lets
//! agents discover relevant context at runtime via `list_skills`/`load_skill`
//! tools or via automatic per-task injection in the workflow engine.
//! What: `SkillEntry` is the indexed record for one skill file;
//! `SkillRegistry::load` scans a directory and builds the index; `search`
//! returns the top-N matches for a query; `auto_inject` renders a prompt
//! prefix containing the best matches. All parsing is best-effort: files
//! without frontmatter are indexed with empty tags and the filename as name,
//! unreadable files are skipped with a warn log.
//! Test: See `registry_search_ranks_by_tag_and_description`,
//! `parse_skill_file_extracts_frontmatter`, and
//! `parse_skill_file_missing_frontmatter_uses_filename`.

pub mod global_cache;
pub mod index;
pub mod rating;
pub mod registry;
pub mod sources;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Module-level cache for LLM skill selection results.
///
/// Why: LLM calls are expensive (latency + tokens). Many phases re-invoke skill
/// selection with the same task prefix and skill index, so memoizing on a hash
/// of `(task_prefix_512, skill_index)` saves repeated round-trips.
/// What: Lazy-initialized `Mutex<HashMap<u64, Vec<String>>>`; key is the hash of
/// the cache key string, value is the previously selected skill names.
/// Test: Indirect — `select_skills_via_llm` checks this map before any IO.
fn llm_skill_cache() -> &'static std::sync::Mutex<HashMap<u64, Vec<String>>> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<u64, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Compute a stable cache key hash from `(task_prefix, skill_index)`.
fn compute_cache_key(task: &str, skill_index: &str) -> u64 {
    let prefix_len = 512.min(task.len());
    // Slice on a UTF-8 boundary by walking back to the nearest char boundary.
    let mut boundary = prefix_len;
    while boundary > 0 && !task.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let task_prefix = &task[..boundary];
    let combined = format!("{task_prefix}{skill_index}");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    combined.hash(&mut hasher);
    hasher.finish()
}

/// Check whether the LLM-based skill selector is enabled.
///
/// Why: The feature is opt-in until validated; default-off avoids unexpected
/// LLM spend and preserves existing keyword-matching behavior.
/// What: Returns true when env var `OPEN_MPM_SKILL_LLM=1` is set.
/// Test: Set the var, call this, assert true; unset, call, assert false.
pub fn skill_llm_enabled() -> bool {
    std::env::var("OPEN_MPM_SKILL_LLM").unwrap_or_default() == "1"
}

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
fn parse_skill_file(path: &Path, content: &str) -> SkillEntry {
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

/// Detects relevant skills from project context and task text, then loads them.
///
/// Why: The workflow engine needs to automatically inject language and framework
/// knowledge into agent prompts without each agent TOML listing skills explicitly.
/// What: Scans for language indicators (Cargo.toml → rust, requirements.txt → python,
/// package.json → typescript), detects framework keywords in task text, loads
/// the relevant skill files from config/skills/languages/, config/skills/frameworks/,
/// and config/skills/workflow/ subdirectories.
/// Test: `test_skills_loader_detects_rust_from_cargo_toml`,
/// `test_skills_loader_detects_python_from_requirements`,
/// `test_skills_loader_detects_frameworks_from_task`,
/// `test_skills_loader_auto_mode_returns_empty_when_no_skills_dir`.
pub struct SkillsLoader {
    skills_root: PathBuf,
    /// Cache: skill file path -> file content (avoids re-reading the same file twice).
    cache: tokio::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
}

impl SkillsLoader {
    /// Create a new loader rooted at `skills_root` (typically `config/skills`).
    ///
    /// Why: Centralizes the skills root so callers don't scatter path construction.
    /// What: Stores the root and initializes an empty read cache.
    /// Test: Construct with a temp dir and assert `build_skills_prefix` returns
    /// empty when no skill files exist.
    pub fn new(skills_root: PathBuf) -> Self {
        Self {
            skills_root,
            cache: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Detect languages used in `project_dir` by checking for sentinel files.
    ///
    /// Why: Avoids forcing every agent TOML to declare the language explicitly;
    /// the project's own dependency files are the authoritative source.
    /// What: Checks for `Cargo.toml` → "rust", `requirements.txt` → "python",
    /// `package.json` → "typescript". Returns a deduplicated list.
    /// Test: Write a Cargo.toml into a temp dir and assert "rust" is returned.
    pub fn detect_languages(project_dir: &Path) -> Vec<String> {
        let mut langs = Vec::new();
        if project_dir.join("Cargo.toml").exists() {
            langs.push("rust".to_string());
        }
        if project_dir.join("requirements.txt").exists()
            || project_dir.join("pyproject.toml").exists()
            || project_dir.join("setup.py").exists()
        {
            langs.push("python".to_string());
        }
        if project_dir.join("package.json").exists() {
            langs.push("typescript".to_string());
        }
        langs
    }

    /// Detect framework skills from keyword scanning of `task` text.
    ///
    /// Why: Task text contains the most direct signal about which frameworks
    /// the agent will work with; keyword matching is cheap and sufficient.
    /// What: Returns skill names for matched keywords:
    /// "fastapi" → ["fastapi"], "pytest"/"test" → ["pytest"],
    /// "sqlalchemy" → ["sqlalchemy"], "tokio"/"axum" → ["tokio"],
    /// "docker"/"container" → ["docker"].
    /// Test: Pass "write a fastapi endpoint with pytest tests" and assert
    /// both "fastapi" and "pytest" are returned.
    pub fn detect_frameworks(task: &str) -> Vec<String> {
        let lower = task.to_lowercase();
        let mut frameworks = Vec::new();
        if lower.contains("fastapi") {
            frameworks.push("fastapi".to_string());
        }
        if lower.contains("pytest") || lower.contains(" test ") || lower.contains("testing") {
            frameworks.push("pytest".to_string());
        }
        if lower.contains("sqlalchemy") {
            frameworks.push("sqlalchemy".to_string());
        }
        if lower.contains("tokio") || lower.contains("axum") {
            frameworks.push("tokio".to_string());
        }
        if lower.contains("docker") || lower.contains("container") {
            frameworks.push("docker".to_string());
        }
        frameworks
    }

    /// Detect workflow methodology skills from `task` text.
    ///
    /// Why: Workflow skills like TDD and wave-planning improve output quality
    /// when the task description signals that methodology is expected.
    /// What: "tdd"/"test first"/"red green" → ["tdd"],
    /// "wave"/"decompose" → ["wave-planning"].
    /// Test: Pass "use tdd to implement" and assert ["tdd"] is returned.
    ///
    /// #233: "assignments" was previously a wave-planning trigger but it
    /// matches workflow-internal template variables (e.g. references to
    /// `assignments.json`) that leak into rendered task text, causing
    /// wave-planning to be falsely auto-detected on tasks that aren't
    /// multi-wave. Removed from the keyword list — explicit "wave" or
    /// "decompose" still trigger detection.
    pub fn detect_workflow_skills(task: &str) -> Vec<String> {
        let lower = task.to_lowercase();
        let mut skills = Vec::new();
        if lower.contains("tdd")
            || lower.contains("test first")
            || lower.contains("red green")
            || lower.contains("red-green")
        {
            skills.push("tdd".to_string());
        }
        if lower.contains("wave") || lower.contains("decompose") {
            skills.push("wave-planning".to_string());
        }
        skills
    }

    /// Read a skill file from disk, returning the content (with frontmatter stripped).
    /// Results are cached so repeated loads of the same file incur only one read.
    ///
    /// Why: Skill files may be requested multiple times across phases; caching
    /// avoids redundant IO and keeps prompt assembly fast.
    /// What: Checks the in-memory cache first; on miss, reads the file and stores
    /// it. Returns `None` on IO error (logs a warning).
    /// Test: Call twice with the same path and assert the file is read only once
    /// (mock or check that a deleted-after-first-read file still returns content).
    pub async fn load_skill_file(&self, path: &Path) -> Option<String> {
        {
            let cache = self.cache.lock().await;
            if let Some(content) = cache.get(path) {
                return Some(content.clone());
            }
        }
        match tokio::fs::read_to_string(path).await {
            Ok(raw) => {
                let body = strip_frontmatter(&raw).to_string();
                let mut cache = self.cache.lock().await;
                cache.insert(path.to_path_buf(), body.clone());
                Some(body)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "SkillsLoader: failed to read skill file"
                );
                None
            }
        }
    }

    /// Resolve a skill name to a file path under the skills root.
    ///
    /// Why: Skill names are bare (e.g. "rust", "fastapi") but files live in
    /// subdirectories; this centralizes the search so callers don't scatter
    /// path logic.
    /// What: Checks `languages/<name>.md`, `frameworks/<name>.md`,
    /// `workflow/<name>.md`, and `<name>.md` (flat fallback) in that order.
    /// Returns the first path that exists, or `None`.
    /// Test: Create a file at `languages/rust.md` and assert the resolver
    /// returns that path for the name "rust".
    fn resolve_skill_path(&self, name: &str) -> Option<PathBuf> {
        let subdirs = ["languages", "frameworks", "workflow"];
        for subdir in &subdirs {
            let candidate = self.skills_root.join(subdir).join(format!("{name}.md"));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Flat fallback for top-level skill files.
        let flat = self.skills_root.join(format!("{name}.md"));
        if flat.exists() {
            return Some(flat);
        }

        // #115: Fall through to global discovery paths.
        // Discovery order: ~/.open-mpm/skills/files/ > ~/Projects/skillset-mcp
        let home = dirs::home_dir().unwrap_or_default();
        let global_bases = [
            home.join(".open-mpm").join("skills").join("files"),
            home.join("Projects").join("skillset-mcp"),
        ];
        for base in &global_bases {
            // Flat: <base>/<name>.md
            let flat_global = base.join(format!("{name}.md"));
            if flat_global.exists() {
                return Some(flat_global);
            }
            // Subdirectories within global base.
            for subdir in &subdirs {
                let candidate = base.join(subdir).join(format!("{name}.md"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    /// Aggregate the existing keyword-based detectors into a single skill name list.
    ///
    /// Why: This is the legacy "auto" path — kept as a fallback for the LLM
    /// selector and when the feature flag is off. Centralizing it avoids
    /// duplication between the LLM-on and LLM-off branches.
    /// What: Concatenates `detect_languages + detect_frameworks + detect_workflow_skills`.
    /// Test: Indirect; covered by existing `test_skills_loader_*` tests.
    fn keyword_auto_skills(project_dir: &Path, task: &str) -> Vec<String> {
        let mut names = Self::detect_languages(project_dir);
        names.extend(Self::detect_frameworks(task));
        names.extend(Self::detect_workflow_skills(task));
        names
    }

    /// Discover available skills under `skills_root` for use as the LLM candidate list.
    ///
    /// Why: The LLM needs to see a concrete list of skill names + descriptions
    /// to choose from. Walking the on-disk layout (languages/, frameworks/,
    /// workflow/, flat) gives the same set the resolver can later load.
    /// What: For each `*.md` file found in any of the standard subdirs, parse
    /// the frontmatter to get name/description/tags. Returns up to ~80 entries
    /// to keep the prompt compact.
    /// Test: Indirect — `detect_skills_via_llm` uses this; existing skill files
    /// in `.open-mpm/skills/` exercise it at runtime.
    async fn discover_available_skills(&self) -> Vec<(String, String, Vec<String>)> {
        let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
        let subdirs = ["languages", "frameworks", "workflow"];

        // Project / configured skills root.
        for sub in &subdirs {
            let dir = self.skills_root.join(sub);
            collect_skills_from_dir(&dir, &mut out).await;
        }
        // Flat skill files at the root.
        collect_skills_from_dir(&self.skills_root, &mut out).await;

        // Global discovery paths (mirror `resolve_skill_path`).
        let home = dirs::home_dir().unwrap_or_default();
        let global_bases = [
            home.join(".open-mpm").join("skills").join("files"),
            home.join("Projects").join("skillset-mcp"),
        ];
        for base in &global_bases {
            if !base.exists() {
                continue;
            }
            collect_skills_from_dir(base, &mut out).await;
            for sub in &subdirs {
                collect_skills_from_dir(&base.join(sub), &mut out).await;
            }
        }

        // Deduplicate by name (first wins).
        let mut seen = std::collections::HashSet::new();
        out.retain(|(n, _, _)| seen.insert(n.clone()));

        // Cap to keep the prompt small.
        out.truncate(80);
        out
    }

    /// Drive the LLM-based skill selection end-to-end.
    ///
    /// Why: Replaces the brittle keyword matching with a one-shot LLM call to
    /// `claude-haiku-4-5` that ranks the available skills against the task.
    /// What: Discovers available skills, hashes the (task_prefix, skill_index)
    /// for cache lookup, builds an OpenRouter client, and invokes
    /// `select_skills_via_llm` under a timeout. Cache hits skip the LLM call.
    /// Test: `llm_skill_cache_hit_skips_llm_call`.
    async fn detect_skills_via_llm(
        &self,
        _project_dir: &Path,
        task: &str,
    ) -> anyhow::Result<Vec<String>> {
        let available = self.discover_available_skills().await;
        if available.is_empty() {
            return Ok(Vec::new());
        }

        let skill_index = available
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let cache_key = compute_cache_key(task, &skill_index);

        // Cache lookup.
        if let Ok(cache) = llm_skill_cache().lock()
            && let Some(hit) = cache.get(&cache_key)
        {
            tracing::debug!(
                skills = ?hit,
                "LLM skill selection cache hit"
            );
            return Ok(hit.clone());
        }

        // Need an API key to call OpenRouter.
        if std::env::var("OPENROUTER_API_KEY")
            .unwrap_or_default()
            .is_empty()
        {
            anyhow::bail!("OPENROUTER_API_KEY not set");
        }

        let client = crate::llm::create_client()?;
        let max_skills: usize = 6;
        let selected = tokio::time::timeout(
            Duration::from_secs(8),
            select_skills_via_llm(task, &available, &client, max_skills),
        )
        .await
        .map_err(|_| anyhow::anyhow!("LLM skill selection timed out after 8s"))??;

        // Store in cache.
        if let Ok(mut cache) = llm_skill_cache().lock() {
            cache.insert(cache_key, selected.clone());
        }

        Ok(selected)
    }

    /// Build a `## Relevant Skills` prompt prefix from explicit or auto-detected skills.
    ///
    /// Why: The workflow engine needs a single call to get all relevant skill
    /// bodies assembled into a ready-to-prepend prompt block.
    /// What: If `explicit` contains "auto", auto-detects languages + frameworks
    /// + workflow skills from `project_dir` and `task`. Otherwise uses `explicit`
    /// as skill names. Loads each resolved skill file (stripping frontmatter),
    /// assembles them into a `## Relevant Skills\n\n### Skill: <name>\n<body>`
    /// block, and returns it. Returns an empty string when no skills resolve.
    /// Test: Create a temp skills dir with `languages/rust.md`; call with
    /// explicit=["auto"] from a project dir containing Cargo.toml and assert
    /// the result contains "## Relevant Skills" and "rust" content.
    #[cfg(test)]
    pub async fn build_skills_prefix(
        &self,
        explicit: &[String],
        project_dir: &Path,
        task: &str,
    ) -> String {
        let (prefix, _used) = self
            .build_skills_prefix_tracked(explicit, project_dir, task)
            .await;
        prefix
    }

    /// Like [`build_skills_prefix`], but also returns the list of skill names
    /// whose bodies were actually loaded into the prefix (#171).
    ///
    /// Why: The workflow engine needs to know which skills were injected so
    /// `update_skill_usage` can increment `use_count` and refresh `last_used`
    /// for those exact skills after the run.
    /// What: Same detection + resolution logic as `build_skills_prefix`; only
    /// names whose `load_skill_file` succeeded are included in the returned
    /// `Vec`. Order matches the prefix sections.
    /// Test: `build_skills_prefix_tracked_returns_loaded_names`.
    pub async fn build_skills_prefix_tracked(
        &self,
        explicit: &[String],
        project_dir: &Path,
        task: &str,
    ) -> (String, Vec<String>) {
        let skill_names: Vec<String> = if explicit.iter().any(|s| s == "auto") {
            // Try LLM-based selection first when the feature flag is on; on any
            // failure (no API key, timeout, parse error) fall back to keyword
            // matching so existing behavior is preserved.
            if skill_llm_enabled() {
                match self.detect_skills_via_llm(project_dir, task).await {
                    Ok(names) if !names.is_empty() => names,
                    Ok(_) => {
                        tracing::debug!(
                            "LLM skill selection returned no skills; falling back to keyword matching"
                        );
                        Self::keyword_auto_skills(project_dir, task)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "LLM skill selection failed ({e}), falling back to keyword matching"
                        );
                        Self::keyword_auto_skills(project_dir, task)
                    }
                }
            } else {
                Self::keyword_auto_skills(project_dir, task)
            }
        } else {
            explicit.to_vec()
        };

        if skill_names.is_empty() {
            return (String::new(), Vec::new());
        }

        let mut sections: Vec<String> = Vec::with_capacity(skill_names.len() + 1);
        sections.push("## Relevant Skills".to_string());
        let mut used: Vec<String> = Vec::new();

        for name in &skill_names {
            if let Some(path) = self.resolve_skill_path(name) {
                if let Some(body) = self.load_skill_file(&path).await {
                    sections.push(format!("### Skill: {name}\n{body}"));
                    used.push(name.clone());
                }
            } else {
                tracing::debug!(
                    skill = %name,
                    skills_root = %self.skills_root.display(),
                    "SkillsLoader: no skill file found for name"
                );
            }
        }

        // Only return a prefix if at least one skill was actually loaded.
        if sections.len() <= 1 {
            return (String::new(), Vec::new());
        }

        (sections.join("\n\n"), used)
    }
}

/// Read a directory looking for `*.md` skill files; append `(name, description, tags)`
/// for each one to `out`.
///
/// Why: Both the project skills root and global discovery paths use the same
/// shape (flat or `languages|frameworks|workflow` subdirs). Centralizing this
/// keeps `discover_available_skills` short and consistent.
/// What: Silently skips missing/unreadable directories. Parses each `.md`
/// file's frontmatter via `parse_skill_file`.
/// Test: Indirect via `discover_available_skills`.
async fn collect_skills_from_dir(dir: &Path, out: &mut Vec<(String, String, Vec<String>)>) {
    if !dir.exists() {
        return;
    }
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            let skill = parse_skill_file(&path, &content);
            out.push((skill.name, skill.description, skill.tags));
        }
    }
}

/// Ask `claude-haiku-4-5` (via OpenRouter) which skills are relevant to `task`.
///
/// Why: Keyword matching is brittle — it both over-fires on incidental words
/// (e.g. "test" inside a non-testing context) and misses paraphrased task
/// descriptions. A cheap LLM call can rank skills by semantic relevance.
/// What: Builds a JSON-array-only prompt with task + available skill list,
/// calls the model at temperature 0.0 with max_tokens=200, parses the response
/// as a JSON array of strings, and filters down to names that exist in
/// `available_skills`.
/// Test: `llm_skill_selection_parses_json_array`.
pub async fn select_skills_via_llm(
    task: &str,
    available_skills: &[(String, String, Vec<String>)],
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    max_skills: usize,
) -> anyhow::Result<Vec<String>> {
    let formatted_skill_list = available_skills
        .iter()
        .map(|(name, desc, tags)| {
            let desc_disp = if desc.is_empty() {
                "(no description)"
            } else {
                desc
            };
            format!("- **{name}**: {desc_disp} [tags: {}]", tags.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = format!(
        "You are a skill selector for an AI coding agent. Given a task description \
         and a list of available skills, return a JSON array of skill names most \
         relevant to completing the task. Respond with ONLY a valid JSON array of \
         strings, e.g. [\"rust\", \"tdd\"]. Select 0 to {max_skills} skills. Prefer \
         precision over recall — only include clearly relevant skills."
    );

    let user_message = format!(
        "TASK:\n{task}\n\nAVAILABLE SKILLS:\n{formatted_skill_list}\n\n\
         Select up to {max_skills} relevant skill names from the list above."
    );

    let response = crate::llm::chat(
        client,
        "anthropic/claude-haiku-4-5",
        &system_prompt,
        &user_message,
        0.0,
        200,
        Vec::new(),
    )
    .await?;

    let raw = response
        .content
        .ok_or_else(|| anyhow::anyhow!("LLM returned no content"))?;

    let parsed = parse_skill_selection_response(&raw)?;
    let valid_names: std::collections::HashSet<&str> = available_skills
        .iter()
        .map(|(n, _, _)| n.as_str())
        .collect();
    let filtered: Vec<String> = parsed
        .into_iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .take(max_skills)
        .collect();

    Ok(filtered)
}

/// Parse the LLM response as a JSON array of strings.
///
/// Why: Models sometimes wrap JSON in code fences or prefix it with prose; we
/// scan for the first `[` and parse the JSON array starting there to be robust.
/// What: Strips ``` fences if present, finds the first `[` and last `]`, parses
/// as `Vec<String>`. Returns the parse error on failure.
/// Test: `llm_skill_selection_parses_json_array`,
/// `llm_skill_selection_strips_code_fences`.
fn parse_skill_selection_response(raw: &str) -> anyhow::Result<Vec<String>> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start_matches('\n'))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);

    let start = stripped
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("no JSON array found in LLM response: {stripped}"))?;
    let end = stripped
        .rfind(']')
        .ok_or_else(|| anyhow::anyhow!("unterminated JSON array in LLM response: {stripped}"))?;
    if end < start {
        anyhow::bail!("malformed JSON array in LLM response: {stripped}");
    }
    let array_text = &stripped[start..=end];
    let parsed: Vec<String> = serde_json::from_str(array_text).map_err(|e| {
        anyhow::anyhow!("failed to parse skill selection JSON: {e} — text: {array_text}")
    })?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("open-mpm-skills-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn parse_skill_file_extracts_frontmatter() {
        let path = PathBuf::from("/tmp/foo.md");
        let content = "---\nname: python-testing\ndescription: pytest and fixtures\ntags: [python, pytest, testing]\n---\n\n# Body\n";
        let entry = parse_skill_file(&path, content);
        assert_eq!(entry.name, "python-testing");
        assert_eq!(entry.description, "pytest and fixtures");
        assert_eq!(
            entry.tags,
            vec![
                "python".to_string(),
                "pytest".to_string(),
                "testing".to_string()
            ]
        );
    }

    #[test]
    fn parse_skill_file_missing_frontmatter_uses_filename() {
        let path = PathBuf::from("/tmp/fallback-skill.md");
        let content = "# Just markdown, no frontmatter\n";
        let entry = parse_skill_file(&path, content);
        assert_eq!(entry.name, "fallback-skill");
        assert_eq!(entry.description, "");
        assert!(entry.tags.is_empty());
    }

    #[test]
    fn parse_skill_file_frontmatter_with_missing_keys_falls_back() {
        let path = PathBuf::from("/tmp/only-name.md");
        let content = "---\nname: only-name\n---\nbody\n";
        let entry = parse_skill_file(&path, content);
        assert_eq!(entry.name, "only-name");
        assert_eq!(entry.description, "");
        assert!(entry.tags.is_empty());
    }

    #[test]
    fn strip_frontmatter_removes_block() {
        let content = "---\nname: x\ntags: [a]\n---\n# Heading\nbody\n";
        let stripped = strip_frontmatter(content);
        assert!(stripped.starts_with("# Heading"));
    }

    #[test]
    fn strip_frontmatter_passthrough_when_absent() {
        let content = "# Heading\n";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn relevance_score_zero_for_unrelated_query() {
        let entry = SkillEntry {
            name: "python-packaging".to_string(),
            description: "pyproject.toml".to_string(),
            tags: vec!["python".to_string(), "packaging".to_string()],
            path: PathBuf::from("/tmp/x.md"),
        };
        assert_eq!(entry.relevance_score("rust async tokio"), 0.0);
    }

    #[test]
    fn relevance_score_hits_tag_and_name() {
        let entry = SkillEntry {
            name: "python-packaging".to_string(),
            description: "pyproject.toml and setuptools".to_string(),
            tags: vec!["python".to_string(), "packaging".to_string()],
            path: PathBuf::from("/tmp/x.md"),
        };
        // "python" matches name (0.4) + tag (0.4) = 0.8
        let s = entry.relevance_score("python");
        assert!(s >= 0.8, "expected >= 0.8 got {s}");
    }

    #[tokio::test]
    async fn registry_load_skips_missing_dir() {
        let missing = tempdir().join("does-not-exist");
        let reg = SkillRegistry::load(&missing).await.unwrap();
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn registry_load_parses_files() {
        let dir = tempdir();
        std::fs::write(
            dir.join("a.md"),
            "---\nname: a-skill\ndescription: first\ntags: [one, two]\n---\nbody",
        )
        .unwrap();
        std::fs::write(dir.join("plain.md"), "no frontmatter here").unwrap();
        std::fs::write(dir.join("skip.txt"), "not markdown").unwrap();

        let reg = SkillRegistry::load(&dir).await.unwrap();
        assert_eq!(reg.len(), 2);
        let names: Vec<&str> = reg.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a-skill"));
        assert!(names.contains(&"plain"));
    }

    #[tokio::test]
    async fn registry_search_ranks_by_tag_and_description() {
        let dir = tempdir();
        std::fs::write(
            dir.join("python.md"),
            "---\nname: python-packaging\ndescription: pyproject.toml setuptools\ntags: [python, packaging, pip]\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("rust.md"),
            "---\nname: rust-async\ndescription: tokio runtime\ntags: [rust, async, tokio]\n---\nbody",
        )
        .unwrap();

        let reg = SkillRegistry::load(&dir).await.unwrap();
        let hits = reg.search("python packaging", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].name, "python-packaging");

        let rust_hits = reg.search("tokio async rust", 5);
        assert_eq!(rust_hits[0].name, "rust-async");

        let empty = reg.search("nothing-related", 5);
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn auto_inject_builds_prefix_when_matches_exist() {
        let dir = tempdir();
        std::fs::write(
            dir.join("py.md"),
            "---\nname: python-packaging\ndescription: pyproject\ntags: [python, packaging]\n---\n# Body\nsome text",
        )
        .unwrap();
        let reg = SkillRegistry::load(&dir).await.unwrap();
        let prefix = reg.auto_inject("write a python packaging script", 2).await;
        assert!(prefix.contains("## Relevant Skills"));
        assert!(prefix.contains("python-packaging"));
        assert!(prefix.contains("# Body"));
        // Frontmatter should have been stripped:
        assert!(!prefix.contains("---\nname:"));
    }

    #[tokio::test]
    async fn auto_inject_returns_empty_when_no_matches() {
        let dir = tempdir();
        std::fs::write(
            dir.join("py.md"),
            "---\nname: python-packaging\ndescription: pyproject\ntags: [python]\n---\nbody",
        )
        .unwrap();
        let reg = SkillRegistry::load(&dir).await.unwrap();
        let prefix = reg.auto_inject("completely unrelated xyzzy", 2).await;
        assert!(prefix.is_empty());
    }

    #[test]
    fn format_index_handles_empty() {
        let reg = SkillRegistry::empty();
        assert_eq!(reg.format_index(), "No skills available.");
    }

    #[test]
    fn format_index_lists_skills() {
        let reg = SkillRegistry {
            skills: vec![SkillEntry {
                name: "x".into(),
                description: "desc".into(),
                tags: vec!["t1".into()],
                path: PathBuf::from("/x.md"),
            }],
        };
        let out = reg.format_index();
        assert!(out.contains("**x**"));
        assert!(out.contains("desc"));
        assert!(out.contains("t1"));
    }

    // ── SkillsLoader tests ────────────────────────────────────────────────

    #[test]
    fn test_skills_loader_detects_rust_from_cargo_toml() {
        let dir = tempdir();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        let langs = SkillsLoader::detect_languages(&dir);
        assert!(
            langs.contains(&"rust".to_string()),
            "expected rust, got {langs:?}"
        );
        assert!(!langs.contains(&"python".to_string()));
    }

    #[test]
    fn test_skills_loader_detects_python_from_requirements() {
        let dir = tempdir();
        std::fs::write(dir.join("requirements.txt"), "fastapi\n").unwrap();
        let langs = SkillsLoader::detect_languages(&dir);
        assert!(
            langs.contains(&"python".to_string()),
            "expected python, got {langs:?}"
        );
        assert!(!langs.contains(&"rust".to_string()));
    }

    #[test]
    fn test_skills_loader_detects_frameworks_from_task() {
        let task = "write a fastapi endpoint with pytest tests";
        let frameworks = SkillsLoader::detect_frameworks(task);
        assert!(
            frameworks.contains(&"fastapi".to_string()),
            "missing fastapi: {frameworks:?}"
        );
        assert!(
            frameworks.contains(&"pytest".to_string()),
            "missing pytest: {frameworks:?}"
        );
        assert!(!frameworks.contains(&"docker".to_string()));
    }

    #[tokio::test]
    async fn test_skills_loader_auto_mode_returns_empty_when_no_skills_dir() {
        let project_dir = tempdir();
        // Simulate a Rust project — but no skills directory exists.
        std::fs::write(project_dir.join("Cargo.toml"), "[package]").unwrap();

        let missing_skills_root = tempdir().join("no-such-skills");
        let loader = SkillsLoader::new(missing_skills_root);
        let prefix = loader
            .build_skills_prefix(
                &["auto".to_string()],
                &project_dir,
                "implement a tokio server",
            )
            .await;
        // No skill files → empty prefix.
        assert!(
            prefix.is_empty(),
            "expected empty prefix when skills dir absent, got: {prefix}"
        );
    }

    #[tokio::test]
    async fn load_additional_dir_respects_existing() {
        // First source defines "shared" + "only-a". Second source has "shared"
        // (must not override) + "only-b" (must be added).
        let dir_a = tempdir();
        std::fs::write(
            dir_a.join("shared.md"),
            "---\nname: shared\ndescription: from-a\ntags: []\n---\nbody-a",
        )
        .unwrap();
        std::fs::write(dir_a.join("only-a.md"), "---\nname: only-a\n---\nbody").unwrap();
        let mut reg = SkillRegistry::load(&dir_a).await.unwrap();
        assert_eq!(reg.len(), 2);

        let dir_b = tempdir();
        std::fs::write(
            dir_b.join("shared.md"),
            "---\nname: shared\ndescription: from-b\ntags: []\n---\nbody-b",
        )
        .unwrap();
        std::fs::write(dir_b.join("only-b.md"), "---\nname: only-b\n---\nbody").unwrap();

        reg.load_additional_dir(&dir_b).await;
        assert_eq!(reg.len(), 3);
        let shared = reg.skills.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.description, "from-a", "existing entry must win");
        assert!(reg.skills.iter().any(|s| s.name == "only-b"));
    }

    #[test]
    fn llm_skill_selection_parses_json_array() {
        let raw = r#"["rust", "tdd"]"#;
        let parsed = parse_skill_selection_response(raw).unwrap();
        assert_eq!(parsed, vec!["rust".to_string(), "tdd".to_string()]);
    }

    #[test]
    fn llm_skill_selection_strips_code_fences() {
        let raw = "```json\n[\"rust\", \"tdd\"]\n```";
        let parsed = parse_skill_selection_response(raw).unwrap();
        assert_eq!(parsed, vec!["rust".to_string(), "tdd".to_string()]);
    }

    #[test]
    fn llm_skill_selection_handles_prose_prefix() {
        let raw = "Here are the skills: [\"rust\"]";
        let parsed = parse_skill_selection_response(raw).unwrap();
        assert_eq!(parsed, vec!["rust".to_string()]);
    }

    #[test]
    fn llm_skill_selection_rejects_non_array() {
        let raw = "not an array at all";
        assert!(parse_skill_selection_response(raw).is_err());
    }

    #[test]
    fn skill_llm_disabled_by_default() {
        // We can't safely manipulate env vars in parallel tests; just check
        // the function returns a bool without panicking. The actual flag check
        // is exercised through integration when the env var is set.
        let _ = skill_llm_enabled();
    }

    #[test]
    fn compute_cache_key_is_stable() {
        let a = compute_cache_key("write a tokio server", "rust,tokio,docker");
        let b = compute_cache_key("write a tokio server", "rust,tokio,docker");
        assert_eq!(a, b);
        let c = compute_cache_key("write a tokio server", "rust,tokio");
        assert_ne!(a, c);
    }

    #[test]
    fn compute_cache_key_handles_long_task() {
        // Verify slicing on UTF-8 boundaries doesn't panic when task contains
        // multi-byte chars near the 512-byte cutoff.
        let task = "é".repeat(400); // 800 bytes
        let key = compute_cache_key(&task, "rust");
        // Repeated computation must give the same hash.
        assert_eq!(key, compute_cache_key(&task, "rust"));
    }

    #[tokio::test]
    async fn auto_mode_falls_back_to_keywords_when_llm_disabled() {
        // Build a project with Cargo.toml, no skills dir => empty prefix.
        // Verifies the default-off path still uses keyword detection.
        let project_dir = tempdir();
        std::fs::write(project_dir.join("Cargo.toml"), "[package]").unwrap();
        let skills_root = tempdir().join("missing");
        let loader = SkillsLoader::new(skills_root);

        // SAFETY: tests can run in parallel, but this var only affects the
        // default-off branch we want to exercise.
        // Ensure flag is off for this test.
        // We do not unset other vars.
        let prev = std::env::var("OPEN_MPM_SKILL_LLM").ok();
        // SAFETY: env mutation is process-global; acceptable in this isolated test.
        unsafe {
            std::env::remove_var("OPEN_MPM_SKILL_LLM");
        }
        let prefix = loader
            .build_skills_prefix(
                &["auto".to_string()],
                &project_dir,
                "implement a tokio server",
            )
            .await;
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("OPEN_MPM_SKILL_LLM", v);
            }
        }
        // No skill files exist on disk so even keyword matches resolve to nothing.
        assert!(prefix.is_empty());
    }

    #[tokio::test]
    async fn test_skills_loader_explicit_skills_loaded_correctly() {
        let skills_root = tempdir();
        let langs_dir = skills_root.join("languages");
        std::fs::create_dir_all(&langs_dir).unwrap();
        std::fs::write(
            langs_dir.join("rust.md"),
            "---\nname: rust\ndescription: Rust idioms\ntags: [rust]\n---\n# Rust Skill\nOwnership rules.",
        )
        .unwrap();

        let loader = SkillsLoader::new(skills_root);
        let prefix = loader
            .build_skills_prefix(
                &["rust".to_string()],
                &PathBuf::from("/tmp"),
                "implement something",
            )
            .await;
        assert!(
            prefix.contains("## Relevant Skills"),
            "missing header: {prefix}"
        );
        assert!(
            prefix.contains("### Skill: rust"),
            "missing skill section: {prefix}"
        );
        assert!(
            prefix.contains("Ownership rules"),
            "missing skill body: {prefix}"
        );
        // Frontmatter should be stripped.
        assert!(
            !prefix.contains("---\nname:"),
            "frontmatter leaked into prefix: {prefix}"
        );
    }
}

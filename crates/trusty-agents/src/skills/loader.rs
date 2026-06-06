//! Context-driven skill detection and prompt-prefix assembly (`SkillsLoader`)
//! (#363 split from `skills/mod.rs`).
//!
//! Why: The workflow engine needs to inject language/framework/workflow skill
//! bodies into agent prompts without each agent TOML enumerating skills. This
//! loader detects relevant skills (keyword or LLM-driven) and assembles the
//! `## Relevant Skills` prompt block.
//! What: Defines `SkillsLoader` and the `collect_skills_from_dir` discovery
//! helper.
//! Test: See `test_skills_loader_*` and `build_skills_prefix_*` in
//! `skills/mod_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::llm::{compute_cache_key, llm_skill_cache, select_skills_via_llm, skill_llm_enabled};
use super::types::{parse_skill_file, strip_frontmatter};

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
        // Discovery order: ~/.trusty-agents/skills/files/ > ~/Projects/skillset-mcp
        let home = dirs::home_dir().unwrap_or_default();
        let global_bases = [
            home.join(".trusty-agents").join("skills").join("files"),
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
    /// in `.trusty-agents/skills/` exercise it at runtime.
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
            home.join(".trusty-agents").join("skills").join("files"),
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

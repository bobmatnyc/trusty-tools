//! Tag-indexed skill registry with configurable sources (#168).
//!
//! Why: Skills were previously discovered ad-hoc from `config/skills/` with no
//! way for operators to point the harness at per-project or per-user skill
//! overrides, and agents had to do free-form `query` search to find relevant
//! skills (slow + unreliable). This registry mirrors the `AgentRegistry`
//! pattern (#167): scan a priority-ordered list of directories at startup,
//! parse minimal YAML frontmatter, index skills by name AND by tag so
//! `list_skills(tags=[...])` is O(1) per tag and deterministically ranked by
//! tag-overlap score. No LLM or embedding call is on the hot path.
//! What: `SkillMeta` is a thin metadata record (name, description, tags,
//! source_path); `SkillRegistry` owns an ordered name → meta map plus an
//! inverted tag → Vec<name> index. Discovery is failure-tolerant: missing
//! directories and malformed frontmatter log at `warn` and are skipped.
//! Test: See the unit tests in `registry/tests.rs`.

mod meta;
mod scan;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;

use meta::{LARGE_DIR_MD_THRESHOLD, PER_SOURCE_SCAN_TIMEOUT, SkillIndex, default_effectiveness};
use scan::{looks_like_external_skill_dir, visit_dir};

// Re-export the public surface so external callers keep using
// `skills::registry::{SkillMeta, MAX_SKILLS_PER_SOURCE, skill_search_paths, ...}`.
pub use meta::{MAX_SKILLS_PER_SOURCE, SKILL_INDEX_SCHEMA_VERSION, SkillMeta};
pub use scan::{skill_index_path, skill_search_paths};

/// In-memory, tag-indexed catalog of every discovered skill.
///
/// Why: `list_skills(tags=[...])` is called on every agent's first tool-using
/// turn; an O(1) inverted-tag index beats re-scanning the filesystem or the
/// skill list each call. Priority ordering (first source wins) gives operators
/// a predictable override story: project-local beats user-level beats bundled.
/// What: `skills` preserves discovery order so `list()` is deterministic;
/// `tag_index` is an inverted map `tag → [skill_names]` built during `load`.
/// Test: `registry_tag_overlap_ranking`, `registry_higher_priority_source_wins`.
pub struct SkillRegistry {
    /// Canonical name → metadata (ordered by discovery).
    skills: IndexMap<String, SkillMeta>,
    /// Inverted index: tag → skill names that carry that tag.
    tag_index: HashMap<String, Vec<String>>,
    /// BM25 semantic search index over all discovered skills (#483).
    ///
    /// Why: Tag lookup requires the caller to already know the right tags;
    /// per-turn dynamic injection instead has only the free-text user message.
    /// A BM25 index ranks skills against that message so the harness can pick
    /// the few most relevant skills to inject without an embedding model.
    /// What: Built from the same search paths as the tag index; `None` until
    /// `attach_bm25_index` is called so existing call sites that build the
    /// registry without the index keep working unchanged.
    bm25: Option<super::index::SkillIndex>,
}

impl SkillRegistry {
    /// Scan `search_paths` in priority order and build the registry.
    ///
    /// Why: Mirrors `AgentRegistry::load` — earlier entries shadow later ones
    /// on name conflict so a `.trusty-agents/skills/fastapi.md` override wins over
    /// the bundled `config/skills/frameworks/fastapi.md`. Missing directories
    /// are a graceful no-op because operators don't want to pre-create every
    /// layer of the hierarchy just to run the harness.
    /// What: Walks each directory recursively for `*.md` files; parses
    /// frontmatter (`name`, `description`, `tags`); inserts the first
    /// occurrence of each name and ignores later duplicates. Files without a
    /// frontmatter block or without both `name` and `tags` fields log a WARN
    /// and are skipped. Builds `tag_index` once at the end.
    /// Test: `registry_finds_skills_by_tag`, `registry_skips_files_without_frontmatter`,
    /// `registry_higher_priority_source_wins`.
    pub fn load(search_paths: &[PathBuf]) -> Self {
        let mut skills: IndexMap<String, SkillMeta> = IndexMap::new();

        for dir in search_paths {
            if !dir.is_dir() {
                tracing::debug!(path = %dir.display(), "skill search path missing, skipping");
                continue;
            }
            // #184: Skip directories that look like external skill repos
            // (e.g. claude-mpm's ~/.claude/skills/ with 700+ files). Operators
            // who want them must opt in via skill-sources.toml.
            if looks_like_external_skill_dir(dir) {
                tracing::warn!(
                    path = %dir.display(),
                    threshold = LARGE_DIR_MD_THRESHOLD,
                    "Skipping large skill directory (>={LARGE_DIR_MD_THRESHOLD} files): \
                     {} — add to skill-sources.toml to opt in",
                    dir.display()
                );
                continue;
            }
            // #184: Per-source budget enforcement (count cap + wall-clock).
            let started = Instant::now();
            let count_before = skills.len();
            visit_dir(dir, &mut skills, dir, &started);
            let added = skills.len() - count_before;
            if started.elapsed() >= PER_SOURCE_SCAN_TIMEOUT {
                tracing::warn!(
                    path = %dir.display(),
                    added,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "skill scan timeout for {}, skipping remaining files",
                    dir.display()
                );
            }
        }

        let mut tag_index: HashMap<String, Vec<String>> = HashMap::new();
        for (name, meta) in &skills {
            for tag in &meta.tags {
                let key = tag.to_lowercase();
                tag_index.entry(key).or_default().push(name.clone());
            }
        }

        // #483: Build the BM25 search index from the same source directories so
        // `search` can rank skills against a free-text query. Failure-tolerant —
        // a build error leaves `bm25` as `None` and `search` returns empty.
        let bm25 = match super::index::SkillIndex::build(search_paths) {
            Ok(idx) => Some(idx),
            Err(e) => {
                tracing::warn!(error = %e, "skills: BM25 index build failed; search disabled");
                None
            }
        };

        Self {
            skills,
            tag_index,
            bm25,
        }
    }

    /// Return up to `n` skill names ranked by BM25 relevance to `query` (#483).
    ///
    /// Why: Per-turn dynamic skill injection needs to rank ALL discoverable
    /// skills against the current free-text user message, which tag lookup
    /// cannot do. This delegates to the BM25 index built during `load`.
    /// What: Returns an empty vector when the BM25 index is absent (registry
    /// built via `empty`), the query is blank, or no skill matches.
    /// Test: `registry_search_delegates_to_bm25_index`.
    pub fn search(&self, query: &str, n: usize) -> Vec<String> {
        match &self.bm25 {
            Some(idx) => idx.search(query, n),
            None => Vec::new(),
        }
    }

    /// Scan paths from a `SkillSourceRegistry`, falling back to `bundled_dir`
    /// at the end (#172).
    ///
    /// Why: Operator-configured sources should be primary; the bundled
    /// `<config_dir>/skills` tree is the safety net so missing/empty configs
    /// still produce a useful catalog.
    /// What: Calls `sources.resolved_paths()` to get the priority-ordered list
    /// of on-disk dirs, appends `bundled_dir` last, and delegates to `load`.
    /// Skips bundled fallback when it duplicates an already-listed path so the
    /// "first dir wins" rule in `load` doesn't accidentally shadow itself.
    /// Test: Indirect — covered by `SkillSourceRegistry` unit tests plus the
    /// existing `SkillRegistry::load` priority/shadow tests.
    pub fn from_sources(sources: &super::sources::SkillSourceRegistry, bundled_dir: &Path) -> Self {
        let mut paths = sources.resolved_paths();
        if !paths.iter().any(|p| p == bundled_dir) {
            paths.push(bundled_dir.to_path_buf());
        }
        Self::load(&paths)
    }

    /// Scan the canonical search paths under `config_dir` AND merge the
    /// persisted effectiveness/usage index in one call (#171/#173).
    ///
    /// Why: Four startup paths (PM `build_registries`, the post-run usage
    /// updater, the workflow `load_tag_skill_registry`, and sub-agent dispatch)
    /// all need "scan the bundled+local skills, then layer the persisted
    /// `~/.trusty-agents/skills/index.json` learning back on top." Duplicating the
    /// load + `merge_index` + WARN-on-failure dance at each site invites drift
    /// (sub-agents previously skipped the merge entirely, so they never saw the
    /// persisted index). This constructor is the single wiring point so every
    /// boot path consults the persistent index identically.
    /// What: Calls `load(skill_search_paths(config_dir))`, then `merge_index`
    /// against `skill_index_path()`. A merge failure is logged at WARN and
    /// swallowed so a corrupt/stale index never aborts startup — the registry
    /// continues with freshly-scanned defaults.
    /// Test: `load_with_index_merges_persisted_effectiveness` in
    /// `registry/tests_persistence.rs`.
    pub fn load_with_index(config_dir: &Path) -> Self {
        let mut reg = Self::load(&skill_search_paths(config_dir));
        let index_path = skill_index_path();
        if let Err(e) = reg.merge_index(&index_path) {
            tracing::warn!(
                error = %e,
                path = %index_path.display(),
                "tag skill registry: failed to merge persisted effectiveness index (continuing with defaults)"
            );
        }
        reg
    }

    /// Build an empty registry (useful for tests and graceful fallbacks).
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            skills: IndexMap::new(),
            tag_index: HashMap::new(),
            bm25: None,
        }
    }

    /// Number of indexed skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// True when no skills were discovered.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Find skills matching ANY of the given tags, ranked by an effectiveness-
    /// weighted overlap score (#171).
    ///
    /// Why: Pure tag-overlap counts can't distinguish a stale, low-quality
    /// skill from a fresh, high-impact one. Multiplying the raw overlap by
    /// `effectiveness_score` lets a 1-tag-match high-quality skill outrank a
    /// 2-tag-match low-quality skill, so the system improves as outcomes
    /// feed back into the score.
    /// What: Case-insensitive tag lookup; deduplicates skill names across
    /// matching tags; computes `effective_score = tag_count * effectiveness`
    /// and sorts that descending, breaking ties by insertion (discovery)
    /// order for determinism.
    /// Test: `registry_tag_overlap_ranking`,
    /// `effectiveness_score_influences_ranking`.
    pub fn find_by_tags(&self, tags: &[&str]) -> Vec<&SkillMeta> {
        if tags.is_empty() || self.skills.is_empty() {
            return Vec::new();
        }
        // name -> number of tag hits.
        let mut scores: HashMap<&str, usize> = HashMap::new();
        for want in tags {
            let key = want.to_lowercase();
            if let Some(names) = self.tag_index.get(&key) {
                for n in names {
                    *scores.entry(n.as_str()).or_insert(0) += 1;
                }
            }
        }
        if scores.is_empty() {
            return Vec::new();
        }
        // Stable ranking: primary = -effective_score (higher first); secondary
        // = index position in the IndexMap (earlier discovery wins ties).
        // `effective_score = tag_count * effectiveness_score`. Tie-breaker uses
        // total_cmp so NaN never panics (defensive — effectiveness is bounded).
        let mut ordered: Vec<(&str, f32, usize)> = scores
            .into_iter()
            .map(|(name, count)| {
                let idx = self.skills.get_index_of(name).unwrap_or(usize::MAX);
                let eff = self
                    .skills
                    .get(name)
                    .map(|m| m.effectiveness_score)
                    .unwrap_or(default_effectiveness());
                (name, count as f32 * eff, idx)
            })
            .collect();
        ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
        ordered
            .into_iter()
            .filter_map(|(name, _, _)| self.skills.get(name))
            .collect()
    }

    /// Update a skill's effectiveness score with a new outcome observation
    /// using exponential moving average (#171).
    ///
    /// Why: Each successful run is one weak signal — replacing the score on
    /// every observation would be too noisy. EMA with alpha=0.3 smooths
    /// outliers while still tracking trend changes within a few runs.
    /// What: `new = 0.3 * score + 0.7 * old`, with `score` clamped to
    /// `[0.0, 1.0]`. No-op when `skill_name` is unknown.
    /// Test: `update_effectiveness_ema`.
    pub fn update_effectiveness(&mut self, skill_name: &str, score: f32) {
        if let Some(meta) = self.skills.get_mut(skill_name) {
            let clamped = score.clamp(0.0, 1.0);
            meta.effectiveness_score = 0.3 * clamped + 0.7 * meta.effectiveness_score;
        }
    }

    /// Increment `use_count` and refresh `last_used` for `skill_name` (#171).
    ///
    /// Why: Persistence functions need a primitive to record an injection
    /// without callers reaching into private fields.
    /// What: Increments `use_count` (saturating) and sets `last_used` to the
    /// supplied ISO-8601 timestamp. No-op when `skill_name` is unknown.
    /// Test: Indirect via `update_skill_usage` integration.
    pub fn record_use(&mut self, skill_name: &str, timestamp_iso: &str) {
        if let Some(meta) = self.skills.get_mut(skill_name) {
            meta.use_count = meta.use_count.saturating_add(1);
            meta.last_used = Some(timestamp_iso.to_string());
        }
    }

    /// Persist the in-memory skill map to a JSON index on disk (#171).
    ///
    /// Why: Effectiveness/usage learning is only valuable if it survives
    /// process restarts. The index is a versioned wrapper containing a flat
    /// `name -> SkillMeta` map keyed by canonical name so a future load can
    /// merge it back over a fresh scan. The `schema_version` field lets future
    /// schema changes be detected and old files discarded cleanly.
    /// What: Creates parent directories if needed, then writes
    /// `serde_json::to_string_pretty` of the `SkillIndex` wrapper. Errors are
    /// returned for the caller to log/swallow at a higher level.
    /// Test: `save_and_load_index_roundtrip`.
    pub fn save_index(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let map: HashMap<String, SkillMeta> = self
            .skills
            .iter()
            .map(|(name, meta)| (name.clone(), meta.clone()))
            .collect();
        let index = SkillIndex {
            schema_version: SKILL_INDEX_SCHEMA_VERSION,
            skills: map,
        };
        let text = serde_json::to_string_pretty(&index)?;
        // #198: Use state_writer for advisory-locked atomic write so a second
        // trusty-agents process can't truncate the index mid-write. The previous
        // tmp+rename was atomic on its own filesystem but had no inter-process
        // coordination — two writers could each rename a different `.tmp`
        // alternately and the persisted state would race.
        crate::state_writer::atomic_write(path, text.as_bytes())?;
        Ok(())
    }

    /// Merge persisted effectiveness/usage fields from a JSON index into the
    /// already-scanned registry (#171, #197).
    ///
    /// Why: The on-disk scan rebuilds `SkillMeta` with defaults; without a
    /// merge step, every restart would wipe the learned effectiveness scores.
    /// What: Reads the JSON at `path` (no-op when absent). If deserialization
    /// fails (stale schema, missing `description` field, etc.) the stale file
    /// is deleted and the registry continues with its freshly-scanned defaults
    /// — no error is propagated to the caller. On success, copies
    /// `effectiveness_score`, `use_count`, and `last_used` from the persisted
    /// map over the matching freshly-scanned entries. Skills present only in
    /// the index but missing on disk are ignored.
    /// Test: `merge_index_restores_effectiveness_after_reload`,
    ///       `merge_index_stale_file_is_deleted_and_noop`.
    pub fn merge_index(&mut self, path: &Path) -> anyhow::Result<()> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        // Try the versioned wrapper first; fall back to the legacy flat map
        // format for indexes written by versions before #197.
        let map: HashMap<String, SkillMeta> = if let Ok(idx) =
            serde_json::from_str::<SkillIndex>(&text)
        {
            idx.skills
        } else {
            match serde_json::from_str::<HashMap<String, SkillMeta>>(&text) {
                Ok(m) => m,
                Err(e) => {
                    // #197 / #216: Stale/corrupt index — delete it and
                    // proceed with fresh defaults.  Log at DEBUG so that
                    // normal schema migrations (e.g. a field added in a
                    // new release) are silent on startup; only genuinely
                    // unexpected corruption surfaces here.
                    tracing::debug!(
                        error = %e,
                        path = %path.display(),
                        "skills index schema mismatch — deleting stale index and using fresh defaults"
                    );
                    let _ = std::fs::remove_file(path);
                    return Ok(());
                }
            }
        };
        for (name, persisted) in map {
            if let Some(meta) = self.skills.get_mut(&name) {
                meta.effectiveness_score = persisted.effectiveness_score;
                meta.use_count = persisted.use_count;
                meta.last_used = persisted.last_used;
            }
        }
        Ok(())
    }

    /// Return the match score (overlap count) for a specific skill name,
    /// when the registry is queried with `tags`. Used by tool callers that
    /// want to include the score in their response JSON.
    pub fn tag_overlap_score(&self, name: &str, tags: &[&str]) -> usize {
        let Some(meta) = self.skills.get(name) else {
            return 0;
        };
        let have: HashSet<String> = meta.tags.iter().map(|t| t.to_lowercase()).collect();
        tags.iter()
            .filter(|t| have.contains(&t.to_lowercase()))
            .count()
    }

    /// Fetch the full Markdown content of a skill by name.
    ///
    /// Why: The registry only indexes metadata; full content is read on
    /// demand to keep startup fast and memory flat.
    /// What: Returns `None` when the name is unknown or the file can no
    /// longer be read (rare; indicates the file was deleted after startup).
    /// Test: Covered by the existing `load_skill` integration path.
    #[allow(dead_code)] // Wired into the `load_skill` tool in a follow-up PR.
    pub fn get_content(&self, name: &str) -> Option<String> {
        let meta = self.skills.get(name)?;
        match std::fs::read_to_string(&meta.source_path) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(
                    name = %name,
                    path = %meta.source_path.display(),
                    error = %e,
                    "skill: failed to read file at load time"
                );
                None
            }
        }
    }

    /// List every discovered skill in priority/discovery order.
    pub fn list(&self) -> Vec<&SkillMeta> {
        self.skills.values().collect()
    }

    /// Look up a single skill's metadata by exact name.
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.get(name)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_persistence;

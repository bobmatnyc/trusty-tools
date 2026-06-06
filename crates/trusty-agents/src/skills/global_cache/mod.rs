//! Global skills cache — scans multiple source directories and maintains a
//! content-addressed cache with a persistent JSON index.
//!
//! Why: Skills live in multiple places (project, user-local, skillset-mcp).
//! Scanning all sources on every startup is slow and defeats caching. This
//! module keeps `~/.trusty-agents/skills/index.json` (metadata) and
//! `~/.trusty-agents/skills/cache/<hash>` (content) so repeated runs avoid
//! redundant disk reads and skills from any source are instantly available.
//! What: `GlobalSkillsCache` holds cache/index paths; `refresh` scans all
//! source directories and updates both. `get_content` serves content from
//! the cache or falls back to a direct disk read. `discovery_paths` defines
//! the canonical source priority order.
//! Test: Construct with a temp home, write a `.md` file in one of the source
//! dirs, call `refresh`, assert the index contains the skill and `get_content`
//! returns its body.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

/// Categorizes where a skill file originated.
///
/// Why: Downstream consumers may want to filter or display skills by origin
/// (e.g., prefer project-local over global), and serde round-trips cleanly
/// through the JSON index without losing that provenance.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// `config/skills/` inside the current project.
    Project,
    /// `~/.trusty-agents/skills/files/`
    UserLocal,
    /// `~/Projects/skillset-mcp`
    SkillsetMcp,
}

/// Indexed metadata record for one skill file.
///
/// Why: Lets callers query the index for name/tags/hash without reading the
/// file content; the hash also drives cache lookups and change detection.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SkillMeta {
    /// Bare skill name (filename stem or frontmatter `name` field).
    pub name: String,
    /// Tags extracted from YAML frontmatter (may be empty).
    pub tags: Vec<String>,
    /// Where the skill was found.
    pub source: SkillSource,
    /// Absolute path to the `.md` file on disk.
    pub path: PathBuf,
    /// SHA-256 hex digest of the raw file content.
    pub content_hash: String,
    /// `mtime` as Unix seconds at index time (for change detection).
    pub last_modified: u64,
}

/// Global skills index + content cache stored under `~/.trusty-agents/skills/`.
///
/// Why: Multiple source directories need to be merged and cached once so that
/// the per-invocation startup cost is a single JSON read rather than a full
/// directory scan. Atomic writes (tmp + rename) keep the index consistent
/// even if the process crashes mid-update.
pub struct GlobalSkillsCache {
    /// `~/.trusty-agents/skills/cache/` — stores `<hash>` files.
    cache_dir: PathBuf,
    /// `~/.trusty-agents/skills/index.json`
    index_path: PathBuf,
}

impl GlobalSkillsCache {
    /// Construct a cache rooted at `~/.trusty-agents/skills/`.
    ///
    /// Why: Centralizes home-dir resolution so callers don't scatter it.
    /// What: Returns an error when `$HOME` is unset.
    /// Test: Call in a test with `HOME` set to a tempdir and assert no error.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let base = home.join(".trusty-agents").join("skills");
        Ok(Self {
            cache_dir: base.join("cache"),
            index_path: base.join("index.json"),
        })
    }

    /// Create cache and index directories if they do not already exist.
    ///
    /// Why: The dirs are created lazily so they don't clutter home on
    /// machines that never use global skills.
    /// What: Creates `~/.trusty-agents/skills/cache/` (and parents) via tokio.
    /// Test: Call twice on the same path and assert no error (idempotent).
    pub async fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir).await?;
        Ok(())
    }

    /// Load the full index from disk, returning an empty map if absent.
    ///
    /// Why: Callers need the index without failing on first-run when no
    /// index file exists yet.
    /// What: Returns `Ok(HashMap::new())` on missing file; propagates other
    /// IO or parse errors.
    /// Test: Assert empty map returned when index_path does not exist.
    pub async fn load_index(&self) -> Result<HashMap<String, SkillMeta>> {
        match fs::read_to_string(&self.index_path).await {
            Ok(text) => {
                let map: HashMap<String, SkillMeta> = serde_json::from_str(&text)?;
                Ok(map)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically persist the index to disk.
    ///
    /// Why: A partial write followed by a crash would corrupt the index; a
    /// tmp-then-rename is atomic on POSIX filesystems.
    /// What: Writes to `<index_path>.tmp` then renames over the target.
    /// Test: Write an index, drop and reload — assert round-trip equality.
    pub async fn save_index(&self, index: &HashMap<String, SkillMeta>) -> Result<()> {
        self.ensure_dirs().await?;
        let tmp = self.index_path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(index)?;
        fs::write(&tmp, &text).await?;
        fs::rename(&tmp, &self.index_path).await?;
        Ok(())
    }

    /// Scan all source directories and rebuild the skill index.
    ///
    /// Why: Called once at startup to detect new or changed skill files
    /// before the workflow engine queries the index.
    /// What: Iterates `discovery_paths`, reads each `*.md` file, computes
    /// its SHA-256 hash, and upserts a `SkillMeta` entry. Saves the updated
    /// index atomically.
    /// Test: Write a `.md` file, call `refresh`, assert the entry appears
    /// in `load_index()`.
    pub async fn refresh(&self, project_dir: &Path) -> Result<()> {
        self.ensure_dirs().await?;
        let mut index = self.load_index().await.unwrap_or_default();

        for (dir, source) in discovery_paths(project_dir) {
            if !dir.exists() {
                continue;
            }
            let mut entries = match fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "global_cache: cannot read dir");
                    continue;
                }
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let content = match fs::read_to_string(&path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "global_cache: skip unreadable");
                        continue;
                    }
                };
                let hash = hex_sha256(&content);
                let mtime = entry
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tags = extract_tags_from_content(&content);

                // Cache the content keyed by hash (no-op if already cached).
                if let Err(e) = self.cache_content(&hash, &content).await {
                    tracing::warn!(hash = %hash, error = %e, "global_cache: failed to cache content");
                }

                index.insert(
                    name.clone(),
                    SkillMeta {
                        name,
                        tags,
                        source: source.clone(),
                        path,
                        content_hash: hash,
                        last_modified: mtime,
                    },
                );
            }
        }

        self.save_index(&index).await?;
        tracing::info!(count = index.len(), "global skills cache refreshed");
        Ok(())
    }

    /// Return skill content: try the hash-keyed cache first, fall back to disk.
    ///
    /// Why: The cache avoids re-reading large skill files on every lookup;
    /// the disk fallback handles skills whose cache entry was evicted or
    /// whose content changed since the last refresh.
    /// What: Reads `cache_dir/<hash>` if it exists; otherwise reads `meta.path`
    /// directly and re-caches it.
    /// Test: Call after `refresh`; assert content matches the file on disk.
    #[cfg(test)]
    pub async fn get_content(&self, meta: &SkillMeta) -> Result<String> {
        let cache_file = self.cache_dir.join(&meta.content_hash);
        if let Ok(cached) = fs::read_to_string(&cache_file).await {
            return Ok(cached);
        }
        // Cache miss — read from source and re-cache.
        let content = fs::read_to_string(&meta.path).await?;
        if let Err(e) = self.cache_content(&meta.content_hash, &content).await {
            tracing::warn!(error = %e, "global_cache: failed to re-cache content");
        }
        Ok(content)
    }

    /// Write `content` to `cache_dir/<hash>` (no-op if file already exists).
    ///
    /// Why: Avoids redundant writes for unchanged skill files.
    /// What: Checks existence before writing; skips if the file is already there.
    /// Test: Write twice with the same hash; assert file is written once.
    async fn cache_content(&self, hash: &str, content: &str) -> Result<()> {
        let dest = self.cache_dir.join(hash);
        if dest.exists() {
            return Ok(());
        }
        let tmp = self.cache_dir.join(format!("{hash}.tmp"));
        fs::write(&tmp, content).await?;
        fs::rename(&tmp, &dest).await?;
        Ok(())
    }
}

/// Build (or refresh) the global skills cache once, swallowing all errors.
///
/// Why (#115): The persistent skill index (`~/.trusty-agents/skills/index.json`) and
/// content cache must be built/loaded once at process boot rather than
/// rebuilt in-memory on every run. Both the workflow path and the
/// PM/interactive boot path need the same fire-and-forget refresh, but cache
/// init or refresh failures (no `$HOME`, unreadable source dir, full disk)
/// must never block startup. Centralizing the construct-refresh-swallow dance
/// here keeps the two call sites honest and identical.
/// What: Constructs a `GlobalSkillsCache`, calls `refresh(project_dir)`, and
/// logs (but swallows) any error at WARN. No-op effect on the in-memory
/// registries — it only primes the on-disk index/cache for this and future
/// runs.
/// Test: `refresh_global_cache_is_noop_on_missing_sources` in `global_cache/tests.rs`.
pub async fn refresh_global_cache(project_dir: &Path) {
    match GlobalSkillsCache::new() {
        Ok(cache) => {
            if let Err(e) = cache.refresh(project_dir).await {
                tracing::warn!(error = %e, "global skills cache refresh failed (continuing)");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "global skills cache init failed (continuing)");
        }
    }
}

/// Return the SHA-256 hex digest of `content`.
fn hex_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extract tags from YAML frontmatter in the skill file content.
///
/// Why: Duplicating the full frontmatter parser here would create coupling;
/// this minimal extractor is sufficient for tag indexing.
/// What: Looks for a line matching `tags: [...]` inside the first `---` fence.
/// Test: Pass content with `tags: [rust, async]` and assert ["rust","async"].
fn extract_tags_from_content(content: &str) -> Vec<String> {
    let rest = match content.strip_prefix("---") {
        Some(r) => r.strip_prefix('\n').or_else(|| r.strip_prefix("\r\n")),
        None => return Vec::new(),
    };
    let rest = match rest {
        Some(r) => r,
        None => return Vec::new(),
    };
    let fm_end = match rest.find("\n---") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let fm = &rest[..fm_end];
    for line in fm.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("tags:") {
            let val = val.trim();
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

/// Return the ordered list of `(directory, source)` pairs to scan.
///
/// Why: The priority order (project > user-local > skillset-mcp) ensures that
/// project-specific overrides shadow global defaults without manual config.
/// What: Returns up to three paths; callers should iterate in order and use
/// the first match for a given skill name.
/// Test: Assert the returned slice contains `project_dir/.trusty-agents/skills` first.
pub fn discovery_paths(project_dir: &Path) -> Vec<(PathBuf, SkillSource)> {
    let home = dirs::home_dir().unwrap_or_default();
    vec![
        (
            project_dir.join(".trusty-agents/skills"),
            SkillSource::Project,
        ),
        (
            home.join(".trusty-agents/skills/files"),
            SkillSource::UserLocal,
        ),
        (home.join("Projects/skillset-mcp"), SkillSource::SkillsetMcp),
    ]
}

#[cfg(test)]
mod tests;

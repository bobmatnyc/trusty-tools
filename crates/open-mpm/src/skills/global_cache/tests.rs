//! Unit tests for the global skills cache (#363 split from `mod.rs`).

use super::*;
use std::collections::HashMap;
use tempfile::TempDir;

/// Build a `GlobalSkillsCache` rooted at a tempdir instead of `$HOME`.
fn cache_in(base: &Path) -> GlobalSkillsCache {
    let skills = base.join(".open-mpm").join("skills");
    GlobalSkillsCache {
        cache_dir: skills.join("cache"),
        index_path: skills.join("index.json"),
    }
}

/// Write a minimal skill `.md` file with optional YAML frontmatter.
async fn write_skill(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).await.unwrap();
    fs::write(dir.join(format!("{name}.md")), body)
        .await
        .unwrap();
}

// INTENT: Verify constructor succeeds when HOME is set.
#[tokio::test]
async fn test_new_succeeds() {
    // `dirs::home_dir()` reads real HOME; just verify no panic.
    let result = GlobalSkillsCache::new();
    assert!(result.is_ok(), "new() should succeed when HOME is set");
}

// INTENT: Verify ensure_dirs is idempotent and creates the cache directory.
#[tokio::test]
async fn test_ensure_dirs_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());

    cache.ensure_dirs().await.unwrap();
    assert!(cache.cache_dir.exists());

    // Second call is a no-op — should not error.
    cache.ensure_dirs().await.unwrap();
    assert!(cache.cache_dir.exists());
}

// INTENT: Verify load_index returns empty map when no index file exists.
#[tokio::test]
async fn test_load_index_missing_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());

    let idx = cache.load_index().await.unwrap();
    assert!(idx.is_empty());
}

// INTENT: Verify save_index + load_index round-trips data correctly.
#[tokio::test]
async fn test_save_load_index_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());

    let mut index = HashMap::new();
    index.insert(
        "test-skill".to_string(),
        SkillMeta {
            name: "test-skill".to_string(),
            tags: vec!["rust".to_string(), "async".to_string()],
            source: SkillSource::Project,
            path: PathBuf::from("/fake/test-skill.md"),
            content_hash: "abc123".to_string(),
            last_modified: 1000,
        },
    );

    cache.save_index(&index).await.unwrap();

    let loaded = cache.load_index().await.unwrap();
    assert_eq!(loaded.len(), 1);
    let meta = loaded.get("test-skill").unwrap();
    assert_eq!(meta.name, "test-skill");
    assert_eq!(meta.tags, vec!["rust", "async"]);
    assert_eq!(meta.source, SkillSource::Project);
    assert_eq!(meta.content_hash, "abc123");
    assert_eq!(meta.last_modified, 1000);
}

// INTENT: Verify save_index uses atomic write (tmp file is cleaned up).
#[tokio::test]
async fn test_save_index_atomic_no_tmp_residue() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());

    let index = HashMap::new();
    cache.save_index(&index).await.unwrap();

    let tmp_path = cache.index_path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "tmp file should be renamed away");
    assert!(cache.index_path.exists(), "index.json should exist");
}

// INTENT: Verify refresh scans a project skills dir and populates the index.
#[tokio::test]
async fn test_refresh_scans_project_skills() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());
    let project = tmp.path().join("project");
    let skills_dir = project.join(".open-mpm").join("skills");

    let body = "# My Skill\nSome content here.";
    write_skill(&skills_dir, "my-skill", body).await;

    cache.refresh(&project).await.unwrap();

    let idx = cache.load_index().await.unwrap();
    assert!(
        idx.contains_key("my-skill"),
        "index should contain my-skill"
    );
    let meta = idx.get("my-skill").unwrap();
    assert_eq!(meta.source, SkillSource::Project);
    assert_eq!(meta.content_hash, hex_sha256(body));
}

// INTENT: Verify get_content returns cached content (cache hit path).
#[tokio::test]
async fn test_get_content_cache_hit() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());
    let project = tmp.path().join("project");
    let skills_dir = project.join(".open-mpm").join("skills");

    let body = "# Cached Skill\nBody text.";
    write_skill(&skills_dir, "cached", body).await;

    cache.refresh(&project).await.unwrap();

    let idx = cache.load_index().await.unwrap();
    let meta = idx.get("cached").unwrap();

    // Cache file should exist after refresh.
    let content = cache.get_content(meta).await.unwrap();
    assert_eq!(content, body);
}

// INTENT: Verify get_content falls back to disk when cache file is missing.
#[tokio::test]
async fn test_get_content_cache_miss_falls_back() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());
    cache.ensure_dirs().await.unwrap();

    let body = "# Fallback Skill\nDisk content.";
    let skill_path = tmp.path().join("standalone.md");
    fs::write(&skill_path, body).await.unwrap();

    let meta = SkillMeta {
        name: "standalone".to_string(),
        tags: vec![],
        source: SkillSource::UserLocal,
        path: skill_path,
        content_hash: hex_sha256(body),
        last_modified: 0,
    };

    // No cache file written — should fall back to meta.path.
    let content = cache.get_content(&meta).await.unwrap();
    assert_eq!(content, body);

    // After fallback, cache file should now exist.
    let cache_file = cache.cache_dir.join(&meta.content_hash);
    assert!(cache_file.exists(), "content should be re-cached on miss");
}

// INTENT: Verify extract_tags_from_content parses YAML frontmatter tags.
#[test]
fn test_extract_tags_basic() {
    let content = "---\ntags: [rust, async]\n---\n# Skill\nBody.";
    let tags = extract_tags_from_content(content);
    assert_eq!(tags, vec!["rust", "async"]);
}

// INTENT: Verify extract_tags handles quoted tag values.
#[test]
fn test_extract_tags_quoted() {
    let content = "---\ntags: [\"rust\", 'tokio']\n---\n# Skill";
    let tags = extract_tags_from_content(content);
    assert_eq!(tags, vec!["rust", "tokio"]);
}

// INTENT: Verify extract_tags returns empty when no frontmatter present.
#[test]
fn test_extract_tags_no_frontmatter() {
    let content = "# Just a heading\nNo frontmatter here.";
    let tags = extract_tags_from_content(content);
    assert!(tags.is_empty());
}

// INTENT: Verify extract_tags returns empty when frontmatter has no tags line.
#[test]
fn test_extract_tags_frontmatter_without_tags() {
    let content = "---\nname: foo\n---\n# Skill";
    let tags = extract_tags_from_content(content);
    assert!(tags.is_empty());
}

// INTENT: Verify discovery_paths returns project dir first (highest priority).
#[test]
fn test_discovery_paths_project_first() {
    let project = PathBuf::from("/tmp/myproject");
    let paths = discovery_paths(&project);

    assert!(!paths.is_empty());
    assert_eq!(paths[0].0, project.join(".open-mpm/skills"));
    assert_eq!(paths[0].1, SkillSource::Project);
}

// INTENT: Verify discovery_paths contains all three source types in order.
#[test]
fn test_discovery_paths_order() {
    let project = PathBuf::from("/tmp/proj");
    let paths = discovery_paths(&project);

    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0].1, SkillSource::Project);
    assert_eq!(paths[1].1, SkillSource::UserLocal);
    assert_eq!(paths[2].1, SkillSource::SkillsetMcp);
}

// INTENT: Verify hex_sha256 produces correct deterministic output.
#[test]
fn test_hex_sha256_deterministic() {
    let a = hex_sha256("hello");
    let b = hex_sha256("hello");
    assert_eq!(a, b);
    assert_eq!(a.len(), 64); // SHA-256 hex = 64 chars

    let c = hex_sha256("world");
    assert_ne!(a, c);
}

// INTENT: Verify refresh handles non-md files gracefully (skips them).
#[tokio::test]
async fn test_refresh_skips_non_md_files() {
    let tmp = TempDir::new().unwrap();
    let cache = cache_in(tmp.path());
    let project = tmp.path().join("project");
    let skills_dir = project.join(".open-mpm").join("skills");

    fs::create_dir_all(&skills_dir).await.unwrap();
    fs::write(skills_dir.join("readme.txt"), "not a skill")
        .await
        .unwrap();
    write_skill(&skills_dir, "real", "# Real skill").await;

    cache.refresh(&project).await.unwrap();

    let idx = cache.load_index().await.unwrap();
    assert_eq!(idx.len(), 1);
    assert!(idx.contains_key("real"));
}

// INTENT: Verify the `refresh_global_cache` startup helper builds the persistent
// index under HOME and is a no-op (never panics / errors) when source dirs are
// absent — this is the wiring called once at PM/interactive boot (#115). The
// helper swallows all errors, so the observable contract is "the index file is
// created and is valid JSON" after a refresh against an empty project.
#[tokio::test]
async fn refresh_global_cache_is_noop_on_missing_sources() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    // SAFETY: tests run single-threaded by default; HOME restored before return.
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    // No `.open-mpm/skills` exists under `project` → refresh must still succeed
    // (fire-and-forget contract) and leave a valid, empty index on disk.
    refresh_global_cache(project.path()).await;

    let index_path = home
        .path()
        .join(".open-mpm")
        .join("skills")
        .join("index.json");
    let exists = index_path.exists();
    let parsed_ok = if exists {
        fs::read_to_string(&index_path)
            .await
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, SkillMeta>>(&t).ok())
            .is_some()
    } else {
        false
    };

    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    assert!(exists, "refresh_global_cache must create the index file");
    assert!(parsed_ok, "the persisted index must be valid JSON");
}

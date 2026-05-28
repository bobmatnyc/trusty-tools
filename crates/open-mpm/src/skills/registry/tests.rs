//! Unit tests for the tag-indexed skill registry (#363 split from `mod.rs`).

use super::*;
use crate::test_env::HOME_LOCK;
use std::fs;
use tempfile::TempDir;

fn write_skill(dir: &Path, name: &str, description: &str, tags: &[&str]) {
    let tags_str = tags
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let content = format!(
        "---\nname: {name}\ndescription: {description}\ntags: [{tags_str}]\n---\n\n# {name}\nbody\n",
    );
    fs::write(dir.join(format!("{name}.md")), content).unwrap();
}

#[test]
fn registry_finds_skills_by_tag() {
    let dir = TempDir::new().unwrap();
    write_skill(
        dir.path(),
        "fastapi",
        "async routes",
        &["python", "fastapi"],
    );
    write_skill(dir.path(), "pytest", "fixtures", &["python", "pytest"]);

    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(reg.len(), 2);

    let hits = reg.find_by_tags(&["python"]);
    assert_eq!(hits.len(), 2);
    let names: Vec<&str> = hits.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"fastapi"));
    assert!(names.contains(&"pytest"));
}

#[test]
fn registry_skips_files_without_frontmatter() {
    let dir = TempDir::new().unwrap();
    // Valid file: indexed.
    write_skill(dir.path(), "ok", "desc", &["tag1"]);
    // No frontmatter: skipped with warn, not a panic.
    fs::write(
        dir.path().join("plain.md"),
        "# Just markdown, no frontmatter\n",
    )
    .unwrap();
    // Frontmatter missing `tags`: skipped with warn.
    fs::write(
        dir.path().join("notag.md"),
        "---\nname: notag\ndescription: missing tags\n---\nbody\n",
    )
    .unwrap();
    // Frontmatter missing `name`: skipped with warn.
    fs::write(
        dir.path().join("noname.md"),
        "---\ndescription: missing name\ntags: [x]\n---\nbody\n",
    )
    .unwrap();

    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(reg.len(), 1, "only 'ok' should be indexed");
    assert!(reg.get("ok").is_some());
}

#[test]
fn registry_higher_priority_source_wins() {
    let high = TempDir::new().unwrap();
    let low = TempDir::new().unwrap();
    // Same name in both dirs; high wins.
    write_skill(high.path(), "shared", "from-high", &["tag-high"]);
    write_skill(low.path(), "shared", "from-low", &["tag-low"]);
    // Unique-to-low skill still appears.
    write_skill(low.path(), "only-low", "low only", &["low-tag"]);

    let reg = SkillRegistry::load(&[high.path().to_path_buf(), low.path().to_path_buf()]);
    assert_eq!(reg.len(), 2);
    let shared = reg.get("shared").expect("shared present");
    assert_eq!(shared.description, "from-high", "high-priority dir wins");
    assert!(reg.get("only-low").is_some());
}

#[test]
fn registry_tag_overlap_ranking() {
    let dir = TempDir::new().unwrap();
    // Triple match for "three"; double for "two"; single for "one".
    write_skill(dir.path(), "three", "d", &["python", "fastapi", "pytest"]);
    write_skill(dir.path(), "two", "d", &["python", "fastapi"]);
    write_skill(dir.path(), "one", "d", &["python"]);

    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let ranked = reg.find_by_tags(&["python", "fastapi", "pytest"]);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].name, "three", "3-tag match should rank first");
    assert_eq!(ranked[1].name, "two", "2-tag match should rank second");
    assert_eq!(ranked[2].name, "one", "1-tag match should rank last");
}

#[test]
fn registry_find_by_tags_case_insensitive() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "rs", "d", &["Rust", "Async"]);
    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let hits = reg.find_by_tags(&["rust"]);
    assert_eq!(hits.len(), 1);
    let hits = reg.find_by_tags(&["ASYNC"]);
    assert_eq!(hits.len(), 1);
}

#[test]
fn registry_recursive_scan_picks_up_nested_dirs() {
    let root = TempDir::new().unwrap();
    let nested = root.path().join("languages");
    fs::create_dir_all(&nested).unwrap();
    write_skill(&nested, "rust", "rust idioms", &["rust"]);
    write_skill(root.path(), "top", "top level", &["top"]);

    let reg = SkillRegistry::load(&[root.path().to_path_buf()]);
    assert_eq!(reg.len(), 2);
    assert!(reg.get("rust").is_some());
    assert!(reg.get("top").is_some());
}

#[test]
fn registry_get_content_returns_file_body() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "d", &["t"]);
    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let content = reg.get_content("x").expect("content present");
    assert!(content.contains("---"));
    assert!(content.contains("# x"));
}

#[test]
fn skill_search_paths_order() {
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    // SAFETY: test-only; we restore HOME at the end.
    unsafe {
        std::env::set_var("HOME", "/tmp/skills-home-test");
    }
    let paths = skill_search_paths(Path::new("/opt/open-mpm/config"));
    assert_eq!(paths[0], PathBuf::from(".open-mpm/skills"));
    assert_eq!(paths[1], PathBuf::from(".claude/skills"));
    // The `../trusty-common/skills` sibling path is conditionally inserted
    // based on whether the directory exists at test time. Skip past it if
    // present so the remaining assertions stay stable across environments.
    let trusty_common = PathBuf::from("../trusty-common/skills");
    let mut idx = 2;
    if paths.get(idx) == Some(&trusty_common) {
        idx += 1;
    }
    assert_eq!(
        paths[idx],
        PathBuf::from("/tmp/skills-home-test/.open-mpm/skills")
    );
    assert_eq!(
        paths[idx + 1],
        PathBuf::from("/tmp/skills-home-test/.claude/skills")
    );
    assert_eq!(paths[idx + 2], PathBuf::from("/opt/open-mpm/config/skills"));

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

// ── Integration-style tests against bundled .open-mpm/skills/ ──────────
//
// Why: Confirms that the bundled `.md` skills under `.open-mpm/skills/`
// still parse correctly and that the tag index surfaces the expected
// entries. Guards against frontmatter drift in the shipped skill library.
// Test: Run `cargo test --lib skill_registry_discovers_bundled_skills`.

fn bundled_skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".open-mpm")
        .join("skills")
}

#[test]
fn skill_registry_discovers_bundled_skills() {
    let paths = vec![bundled_skills_dir()];
    let registry = SkillRegistry::load(&paths);
    assert!(!registry.is_empty(), "expected at least one bundled skill");

    let python_hits = registry.find_by_tags(&["python"]);
    assert!(
        !python_hits.is_empty(),
        "expected at least one python-tagged skill"
    );

    let fastapi_hits = registry.find_by_tags(&["fastapi"]);
    assert!(
        fastapi_hits.iter().any(|s| s.name == "fastapi"),
        "expected fastapi skill discoverable by tag"
    );
}

#[test]
fn skill_registry_ranks_by_tag_overlap() {
    let paths = vec![bundled_skills_dir()];
    let registry = SkillRegistry::load(&paths);
    let results = registry.find_by_tags(&["python", "fastapi", "pytest"]);
    // The registry is non-empty and results don't panic.
    assert!(!results.is_empty(), "expected non-empty results");
    // First result carries the highest tag overlap; verify it carries at
    // least one of the queried tags (sanity of ranking stability).
    let first = results[0];
    let queried: Vec<String> = ["python", "fastapi", "pytest"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let overlap = first
        .tags
        .iter()
        .filter(|t| queried.iter().any(|q| q.eq_ignore_ascii_case(t)))
        .count();
    assert!(
        overlap >= 1,
        "top-ranked skill should overlap at least one queried tag (got {overlap})"
    );
}

#[test]
fn registry_tag_overlap_score_counts_matches() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "s", "d", &["a", "b", "c"]);
    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(reg.tag_overlap_score("s", &["a", "b"]), 2);
    assert_eq!(reg.tag_overlap_score("s", &["a", "z"]), 1);
    assert_eq!(reg.tag_overlap_score("s", &["z"]), 0);
    assert_eq!(reg.tag_overlap_score("missing", &["a"]), 0);
}

//! Effectiveness scoring, persistence, and #184 loading-hang tests for the
//! skill registry (#363 split from `mod.rs`).

use super::meta::LARGE_DIR_MD_THRESHOLD;
use super::scan::looks_like_external_skill_dir;
use super::*;
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

// ── #171: effectiveness scoring + persistence ──────────────────────────

/// Why: Verifies that the effectiveness multiplier can flip ranking when
/// raw tag overlap alone would have ordered the skills differently — a
/// 1-tag, high-effectiveness skill must rank above a 2-tag, low-effectiveness
/// one (`1*0.9 = 0.9 > 2*0.3 = 0.6`).
/// What: Builds two skills, mutates effectiveness, then queries by tags.
/// Test: `effectiveness_score_influences_ranking`.
#[test]
fn effectiveness_score_influences_ranking() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "stale", "d", &["python", "fastapi"]);
    write_skill(dir.path(), "fresh", "d", &["python"]);
    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);

    // Sanity: with default effectiveness (0.5 each), 2-tag wins.
    let baseline = reg.find_by_tags(&["python", "fastapi"]);
    assert_eq!(baseline[0].name, "stale");

    // Tilt the scores hard enough to flip the ranking.
    reg.update_effectiveness("stale", 0.0); // 0.3*0 + 0.7*0.5 = 0.35
    for _ in 0..10 {
        reg.update_effectiveness("stale", 0.0);
    }
    for _ in 0..10 {
        reg.update_effectiveness("fresh", 1.0);
    }

    let ranked = reg.find_by_tags(&["python", "fastapi"]);
    assert_eq!(
        ranked[0].name, "fresh",
        "high-effectiveness 1-tag skill should rank above low-effectiveness 2-tag skill"
    );
}

/// Why: Locks the EMA formula so future refactors can't silently change
/// the weighting and skew rankings on long-lived installs.
/// What: Starts at default (0.5), pushes a 1.0 observation, asserts the
/// expected 0.65 result.
/// Test: `update_effectiveness_ema`.
#[test]
fn update_effectiveness_ema() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "d", &["t"]);
    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);

    // 0.3 * 1.0 + 0.7 * 0.5 = 0.65
    reg.update_effectiveness("x", 1.0);
    let meta = reg.get("x").unwrap();
    assert!((meta.effectiveness_score - 0.65).abs() < 1e-6);

    // 0.3 * 0.0 + 0.7 * 0.65 = 0.455
    reg.update_effectiveness("x", 0.0);
    let meta = reg.get("x").unwrap();
    assert!((meta.effectiveness_score - 0.455).abs() < 1e-6);

    // Out-of-range scores are clamped before applying.
    reg.update_effectiveness("x", 5.0);
    let meta = reg.get("x").unwrap();
    // 0.3 * 1.0 + 0.7 * 0.455 = 0.6185
    assert!((meta.effectiveness_score - 0.6185).abs() < 1e-6);
}

/// Why: Existing JSON indexes (or hand-edited fixtures) must not need an
/// effectiveness field to deserialize — defaults keep migrations painless.
/// What: Deserializes a `SkillMeta` from a minimal JSON document and
/// asserts the new fields took their defaults.
/// Test: `skill_meta_deserializes_with_defaults`.
#[test]
fn skill_meta_deserializes_with_defaults() {
    let raw = r#"{
        "name": "x",
        "description": "d",
        "tags": ["t"],
        "source_path": "/tmp/x.md"
    }"#;
    let meta: SkillMeta = serde_json::from_str(raw).expect("deserialize without new fields");
    assert!((meta.effectiveness_score - 0.5).abs() < 1e-6);
    assert_eq!(meta.use_count, 0);
    assert!(meta.last_used.is_none());
}

/// Why: Round-trip protection — a save followed by a load must restore
/// every persisted field so effectiveness learning isn't lost on restart.
/// What: Writes the index, reloads from disk, merges into a fresh
/// registry, asserts the persisted values overwrote the defaults.
/// Test: `merge_index_restores_effectiveness_after_reload`.
#[test]
fn merge_index_restores_effectiveness_after_reload() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "d", &["t"]);
    let index_path = dir.path().join("index.json");

    // Run 1: train and persist.
    {
        let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
        reg.update_effectiveness("x", 1.0); // -> 0.65
        reg.record_use("x", "2026-04-24T00:00:00Z");
        reg.save_index(&index_path).expect("save_index");
    }

    // Run 2: fresh scan + merge.
    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let meta = reg.get("x").unwrap();
    assert!(
        (meta.effectiveness_score - 0.5).abs() < 1e-6,
        "fresh scan resets to default"
    );

    reg.merge_index(&index_path).expect("merge_index");
    let meta = reg.get("x").unwrap();
    assert!((meta.effectiveness_score - 0.65).abs() < 1e-6);
    assert_eq!(meta.use_count, 1);
    assert_eq!(meta.last_used.as_deref(), Some("2026-04-24T00:00:00Z"));
}

// ── #184: Skill loading hang fixes ────────────────────────────────────

/// Why: Verifies the per-source cap stops scanning once
/// `MAX_SKILLS_PER_SOURCE` files have been indexed, preventing a 700-file
/// directory from hanging startup.
/// What: Generates `MAX_SKILLS_PER_SOURCE * 3` valid skill files in one
/// directory, loads it, and asserts only the cap's worth get indexed.
/// Test: `load_caps_skills_per_source`.
#[test]
fn load_caps_skills_per_source() {
    let dir = TempDir::new().unwrap();
    let n = MAX_SKILLS_PER_SOURCE * 3;
    for i in 0..n {
        let name = format!("s{i:04}");
        write_skill(dir.path(), &name, "d", &["t"]);
    }
    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    assert!(
        reg.len() <= MAX_SKILLS_PER_SOURCE,
        "expected <= {} skills loaded, got {} (cap not enforced)",
        MAX_SKILLS_PER_SOURCE,
        reg.len()
    );
    assert!(
        reg.len() >= MAX_SKILLS_PER_SOURCE,
        "expected at least {} skills loaded, got {} (cap aborted too early)",
        MAX_SKILLS_PER_SOURCE,
        reg.len()
    );
}

/// Why: Confirms that a directory exceeding `LARGE_DIR_MD_THRESHOLD` with
/// no top-level TOML manifests (claude-mpm's `~/.claude/skills/` shape)
/// is detected as external and skipped wholesale by `load`, replacing the
/// 30+ minute hang with a fast WARN.
/// What: Creates `LARGE_DIR_MD_THRESHOLD + 5` `.md` files in a flat dir
/// (no `.toml`), then asserts `load` returns an empty registry. Detection
/// is also asserted directly via `looks_like_external_skill_dir`.
/// Test: `load_skips_external_skill_dir`.
#[test]
fn load_skips_external_skill_dir() {
    let dir = TempDir::new().unwrap();
    // Many .md files, no .toml manifests.
    for i in 0..(LARGE_DIR_MD_THRESHOLD + 5) {
        let name = format!("ext{i:04}");
        write_skill(dir.path(), &name, "d", &["t"]);
    }
    assert!(
        looks_like_external_skill_dir(dir.path()),
        "expected directory with {}+ .md files and no .toml to be flagged external",
        LARGE_DIR_MD_THRESHOLD
    );

    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    assert!(
        reg.is_empty(),
        "expected external dir to be skipped, got {} skills",
        reg.len()
    );
}

/// Why: Guards against false positives — a normal trusty-agents skills dir
/// with a TOML manifest must NOT be flagged as external even if it has
/// many files.
/// What: Writes a `skill-sources.toml` plus a few `.md` files; asserts
/// `looks_like_external_skill_dir` returns false.
/// Test: `looks_like_external_skill_dir_passes_trusty_agents_layout`.
#[test]
fn looks_like_external_skill_dir_passes_trusty_agents_layout() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("skill-sources.toml"), "# manifest").unwrap();
    for i in 0..10 {
        let name = format!("ok{i}");
        write_skill(dir.path(), &name, "d", &["t"]);
    }
    assert!(
        !looks_like_external_skill_dir(dir.path()),
        "directory with TOML manifest must not be flagged external"
    );
}

/// Why: Locks the env-var contract that CTRL relies on to keep its
/// startup fast — when `TAGENT_SKILLS_PROJECT_LOCAL_ONLY=1` is set,
/// `skill_search_paths` must NOT include `~/.claude/skills/` or
/// `~/.trusty-agents/skills/`.
/// What: Sets the env var, calls `skill_search_paths`, asserts the
/// returned list contains only the project-local + bundled paths.
/// Test: `skill_search_paths_respects_project_local_only_env`.
#[test]
fn skill_search_paths_respects_project_local_only_env() {
    // SAFETY: tests run single-threaded by default; we restore env on exit.
    let prev = crate::env_compat::env_var_os(
        "TAGENT_SKILLS_PROJECT_LOCAL_ONLY",
        "OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY",
    );
    unsafe {
        std::env::set_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY", "1");
    }
    let paths = skill_search_paths(Path::new("/opt/trusty-agents/config"));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY", v),
            None => std::env::remove_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY"),
        }
    }
    assert_eq!(
        paths.len(),
        2,
        "expected only project-local + bundled paths"
    );
    assert_eq!(paths[0], PathBuf::from(".trusty-agents/skills"));
    assert_eq!(paths[1], PathBuf::from("/opt/trusty-agents/config/skills"));
    assert!(
        !paths
            .iter()
            .any(|p| p.to_string_lossy().contains(".claude")),
        "project-local-only mode must not include .claude paths"
    );
}

/// Why: Verifies #197 — a stale index with an unrecognizable schema must
/// be silently deleted and the registry must continue with fresh defaults
/// rather than propagating an error that would crash startup.
/// What: Writes malformed JSON to the index path, calls `merge_index`,
/// asserts no error is returned, the file is gone, and the registry
/// retains its scanned defaults.
/// Test: `merge_index_stale_file_is_deleted_and_noop`.
#[test]
fn merge_index_stale_file_is_deleted_and_noop() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "d", &["t"]);
    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);

    // Write a stale index that cannot deserialize as SkillMeta — the
    // `source_path` field is missing, which is required.
    let index_path = dir.path().join("index.json");
    std::fs::write(
        &index_path,
        r#"{"x": {"name": "x", "missing_required": true}}"#,
    )
    .unwrap();

    // merge_index must succeed (no error), delete the stale file, and
    // leave effectiveness at the fresh-scan default.
    reg.merge_index(&index_path)
        .expect("stale index must not error");
    assert!(
        !index_path.exists(),
        "stale index file must be deleted after failed deserialization"
    );
    let meta = reg.get("x").unwrap();
    assert!(
        (meta.effectiveness_score - 0.5).abs() < 1e-6,
        "effectiveness must remain at default after stale-index discard"
    );
}

/// Why: Regression test for #216 — an index.json written by a pre-#216
/// harness omits the `description` field from `SkillMeta` entries. The
/// prior code treated a missing `description` as a fatal deserialization
/// error, deleted the index, and emitted a WARN on every run. Now that
/// `description` carries `#[serde(default)]`, the stale file deserializes
/// cleanly and effectiveness scores are restored without any WARN.
/// What: Writes a versioned index whose entries lack `description`, merges
/// it, and asserts the persisted effectiveness is restored (not reset to
/// the 0.5 default) — confirming the file was read rather than deleted.
/// Test: `merge_index_missing_description_restores_effectiveness`.
#[test]
fn merge_index_missing_description_restores_effectiveness() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "some description", &["t"]);
    let index_path = dir.path().join("index.json");

    // Simulate a pre-#216 index.json: SkillMeta entries have no
    // `description` field, but the versioned wrapper and all other fields
    // are present and valid.
    std::fs::write(
        &index_path,
        r#"{
  "schema_version": 1,
  "skills": {
"x": {
  "name": "x",
  "tags": ["t"],
  "source_path": "/tmp/x.md",
  "effectiveness_score": 0.88,
  "use_count": 3,
  "last_used": "2026-04-01T00:00:00Z"
}
  }
}"#,
    )
    .unwrap();

    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    // Ensure fresh-scan default before merge.
    let meta = reg.get("x").unwrap();
    assert!((meta.effectiveness_score - 0.5).abs() < 1e-6);

    // Merge must succeed and restore persisted fields — no WARN, no delete.
    reg.merge_index(&index_path)
        .expect("index without description must merge cleanly");
    assert!(
        index_path.exists(),
        "index must NOT be deleted when description is merely absent (has serde default)"
    );
    let meta = reg.get("x").unwrap();
    assert!(
        (meta.effectiveness_score - 0.88).abs() < 1e-4,
        "persisted effectiveness_score must be restored; got {}",
        meta.effectiveness_score
    );
    assert_eq!(meta.use_count, 3);
    assert_eq!(meta.last_used.as_deref(), Some("2026-04-01T00:00:00Z"));
}

/// Why: #483 — the registry must expose a free-text `search` that ranks
/// skills via the BM25 index built during `load`. Verifies the delegation
/// is wired and that an `empty()` registry (no index) returns nothing.
/// What: Builds a registry over a dir with two skills, searches for a term
/// unique to one, and asserts it ranks first; then checks `empty()`.
/// Test: `registry_search_delegates_to_bm25_index`.
#[test]
fn registry_search_delegates_to_bm25_index() {
    let dir = TempDir::new().unwrap();
    write_skill(
        dir.path(),
        "web-search",
        "search the web with brave",
        &["web"],
    );
    write_skill(
        dir.path(),
        "rust-async",
        "tokio runtime patterns",
        &["rust"],
    );

    let reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let hits = reg.search("how do I search the web", 3);
    assert!(!hits.is_empty(), "expected a BM25 hit");
    assert_eq!(hits[0], "web-search", "got {hits:?}");

    // An empty registry has no BM25 index → search returns empty.
    assert!(SkillRegistry::empty().search("anything", 3).is_empty());
}

/// Why: A missing index file is the first-run baseline and must be a
/// no-op rather than an error so startup never breaks on a clean install.
#[test]
fn merge_index_missing_file_is_noop() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "x", "d", &["t"]);
    let mut reg = SkillRegistry::load(&[dir.path().to_path_buf()]);
    let nonexistent = dir.path().join("does-not-exist.json");
    reg.merge_index(&nonexistent).expect("missing file ok");
}

/// Why: `load_with_index` is the single startup entry point that every boot
/// path (PM `build_registries`, the workflow `load_tag_skill_registry`, the
/// in-process runner, the post-run usage updater) now funnels through. The
/// wiring is only useful if that one call actually CONSULTS the persisted
/// `~/.trusty-agents/skills/index.json` rather than returning fresh defaults — this
/// test proves the persisted effectiveness/usage fields survive a simulated
/// process restart through `load_with_index`.
/// What: Points `$HOME` at a tempdir (so `skill_index_path()` resolves under
/// it) and restricts discovery to the project-local + bundled paths via
/// `TAGENT_SKILLS_PROJECT_LOCAL_ONLY=1`, writes a skill into the bundled
/// `<config_dir>/skills`, persists a trained index at the canonical path, then
/// calls `load_with_index(config_dir)` and asserts the trained values were
/// merged back. Restores env on exit.
/// Test: `load_with_index_merges_persisted_effectiveness`.
#[test]
fn load_with_index_merges_persisted_effectiveness() {
    let home = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();
    let bundled_skills = config.path().join("skills");
    fs::create_dir_all(&bundled_skills).unwrap();
    write_skill(&bundled_skills, "x", "d", &["t"]);

    // SAFETY: tests run single-threaded by default; env restored before return.
    let prev_home = std::env::var_os("HOME");
    let prev_local = crate::env_compat::env_var_os(
        "TAGENT_SKILLS_PROJECT_LOCAL_ONLY",
        "OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY",
    );
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY", "1");
    }

    // Run 1: scan via load_with_index (index absent → defaults), train, persist
    // to the canonical `skill_index_path()` under the temp HOME.
    {
        let mut reg = SkillRegistry::load_with_index(config.path());
        assert!(reg.get("x").is_some(), "bundled skill must be discovered");
        assert!(
            (reg.get("x").unwrap().effectiveness_score - 0.5).abs() < 1e-6,
            "first load (no index) must use the default score"
        );
        reg.update_effectiveness("x", 1.0); // -> 0.65
        reg.record_use("x", "2026-05-29T00:00:00Z");
        reg.save_index(&skill_index_path()).expect("save_index");
    }

    // Run 2: a fresh load_with_index must consult the persisted index and
    // restore the trained values rather than reverting to defaults.
    let reg = SkillRegistry::load_with_index(config.path());
    let restored = reg.get("x").cloned();

    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_local {
            Some(v) => std::env::set_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY", v),
            None => std::env::remove_var("TAGENT_SKILLS_PROJECT_LOCAL_ONLY"),
        }
    }

    let restored = restored.expect("skill present after reload");
    assert!(
        (restored.effectiveness_score - 0.65).abs() < 1e-6,
        "load_with_index must merge the persisted effectiveness score (got {})",
        restored.effectiveness_score
    );
    assert_eq!(
        restored.use_count, 1,
        "persisted use_count must be restored"
    );
    assert_eq!(
        restored.last_used.as_deref(),
        Some("2026-05-29T00:00:00Z"),
        "persisted last_used must be restored"
    );
}

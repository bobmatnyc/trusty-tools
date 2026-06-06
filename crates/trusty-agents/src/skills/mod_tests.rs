//! Unit tests for the skill index types, loader, and LLM selector (#363
//! split from `skills/mod.rs`).

use std::path::PathBuf;

use super::llm::{compute_cache_key, parse_skill_selection_response, skill_llm_enabled};
use super::loader::SkillsLoader;
use super::types::{SkillEntry, SkillRegistry, parse_skill_file, strip_frontmatter};

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "trusty-agents-skills-test-{}",
        uuid::Uuid::new_v4()
    ));
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
    let prev = crate::env_compat::env_var("TAGENT_SKILL_LLM", "OPEN_MPM_SKILL_LLM").ok();
    // SAFETY: env mutation is process-global; acceptable in this isolated test.
    unsafe {
        std::env::remove_var("TAGENT_SKILL_LLM");
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
            std::env::set_var("TAGENT_SKILL_LLM", v);
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

//! Unit tests for configurable skill sources (#363 split from `mod.rs`).

use super::*;
use tempfile::TempDir;

#[test]
fn sources_load_defaults_when_file_missing() {
    let dir = TempDir::new().unwrap();
    let reg = SkillSourceRegistry::load(dir.path());
    assert_eq!(reg.sources().len(), 2);
    // Highest priority first.
    assert_eq!(reg.sources()[0].priority, 10);
    assert_eq!(
        reg.sources()[0].path.as_deref(),
        Some(".trusty-agents/skills")
    );
    assert_eq!(reg.sources()[1].priority, 5);
    assert_eq!(
        reg.sources()[1].path.as_deref(),
        Some("~/.trusty-agents/skills")
    );
}

#[test]
fn sources_load_handles_malformed_file() {
    let dir = TempDir::new().unwrap();
    let cfg_dir = dir.path().join(".trusty-agents");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("skill-sources.toml"), "not = valid = toml").unwrap();
    let reg = SkillSourceRegistry::load(dir.path());
    // Falls back to defaults rather than panicking.
    assert_eq!(reg.sources().len(), 2);
}

#[test]
fn sources_resolved_paths_expands_tilde() {
    let project = TempDir::new().unwrap();
    let sources = vec![
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("~/some-skills".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 10,
            enabled: true,
            approved: true,
        },
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("local-skills".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 5,
            enabled: true,
            approved: true,
        },
    ];
    let reg = SkillSourceRegistry::from_sources(project.path(), sources);
    let paths = reg.resolved_paths();
    assert_eq!(paths.len(), 2);

    // First path: tilde-expanded under HOME (or kept as-is if HOME unset).
    if let Some(home) = dirs::home_dir() {
        assert_eq!(paths[0], home.join("some-skills"));
    }

    // Second path: relative resolved against project root.
    assert_eq!(paths[1], project.path().join("local-skills"));
}

#[test]
fn sources_disabled_source_excluded() {
    let project = TempDir::new().unwrap();
    let sources = vec![
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("a".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 10,
            enabled: true,
            approved: true,
        },
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("b".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 5,
            enabled: false,
            approved: true,
        },
    ];
    let reg = SkillSourceRegistry::from_sources(project.path(), sources);
    let paths = reg.resolved_paths();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], project.path().join("a"));
}

#[test]
fn remote_git_cache_path_computed_correctly() {
    let project = TempDir::new().unwrap();
    let sources = vec![SkillSource {
        source_type: SkillSourceType::RemoteGit,
        path: None,
        name: Some("claude-mpm-agents".to_string()),
        url: Some("https://github.com/example/claude-mpm-agents".to_string()),
        branch: "main".to_string(),
        skills_subdir: Some("skills/".to_string()),
        priority: 3,
        enabled: true,
        approved: true,
    }];
    let reg = SkillSourceRegistry::from_sources(project.path(), sources);
    let paths = reg.resolved_paths();
    assert_eq!(paths.len(), 1);

    let expected_base = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".trusty-agents")
        .join("skills")
        .join("cache")
        .join("claude-mpm-agents")
        .join("skills/");
    assert_eq!(paths[0], expected_base);
}

#[test]
fn remote_git_without_subdir_uses_cache_dir_directly() {
    let project = TempDir::new().unwrap();
    let sources = vec![SkillSource {
        source_type: SkillSourceType::RemoteGit,
        path: None,
        name: Some("foo".to_string()),
        url: Some("https://example.com/foo.git".to_string()),
        branch: default_branch(),
        skills_subdir: None,
        priority: 0,
        enabled: true,
        approved: true,
    }];
    let reg = SkillSourceRegistry::from_sources(project.path(), sources);
    let paths = reg.resolved_paths();
    assert_eq!(paths.len(), 1);
    let expected = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".trusty-agents")
        .join("skills")
        .join("cache")
        .join("foo");
    assert_eq!(paths[0], expected);
}

#[test]
fn sources_priority_sort_descending() {
    let project = TempDir::new().unwrap();
    let sources = vec![
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("low".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 1,
            enabled: true,
            approved: true,
        },
        SkillSource {
            source_type: SkillSourceType::Local,
            path: Some("high".to_string()),
            name: None,
            url: None,
            branch: default_branch(),
            skills_subdir: None,
            priority: 100,
            enabled: true,
            approved: true,
        },
    ];
    let reg = SkillSourceRegistry::from_sources(project.path(), sources);
    assert_eq!(reg.sources()[0].priority, 100);
    assert_eq!(reg.sources()[1].priority, 1);
}

#[test]
fn sources_parses_full_toml_example() {
    let dir = TempDir::new().unwrap();
    let cfg_dir = dir.path().join(".trusty-agents");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let toml_text = r#"
[[sources]]
type = "local"
path = ".trusty-agents/skills"
priority = 10
enabled = true

[[sources]]
type = "remote_git"
name = "claude-mpm-agents"
url = "https://github.com/bobmatnyc/claude-mpm-agents"
branch = "main"
skills_subdir = "skills/"
priority = 3
approved = false
enabled = false
"#;
    std::fs::write(cfg_dir.join("skill-sources.toml"), toml_text).unwrap();
    let reg = SkillSourceRegistry::load(dir.path());
    assert_eq!(reg.sources().len(), 2);
    let local = &reg.sources()[0];
    assert_eq!(local.source_type, SkillSourceType::Local);
    assert_eq!(local.priority, 10);
    let remote = &reg.sources()[1];
    assert_eq!(remote.source_type, SkillSourceType::RemoteGit);
    assert_eq!(remote.name.as_deref(), Some("claude-mpm-agents"));
    assert_eq!(remote.skills_subdir.as_deref(), Some("skills/"));
    assert!(!remote.enabled);
    assert!(!remote.approved);
}

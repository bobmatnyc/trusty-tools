//! Unit tests for tm project-config parsing + serial-id allocation.
//!
//! Why: The config round-trip and ticket-id allocation are persistence
//! contracts the tm workflow relies on.
//! What: parse/serialize + id-allocation tests.
//! Test: This module is itself the test coverage.

use super::*;

use tempfile::TempDir;

fn sample_config() -> ProjectConfig {
    ProjectConfig {
        project: ProjectMeta {
            name: "open-mpm".to_string(),
            path: PathBuf::from("/Users/masa/Projects/open-mpm"),
            default_harness: Some("repl".to_string()),
        },
        harnesses: vec![
            HarnessConfig {
                name: "repl".to_string(),
                startup_command: "om".to_string(),
                adapter: "claude-mpm".to_string(),
            },
            HarnessConfig {
                name: "bash".to_string(),
                startup_command: "bash".to_string(),
                adapter: "shell".to_string(),
            },
        ],
    }
}

#[test]
fn test_new_defaults_to_basename() {
    let cfg = ProjectConfig::new(PathBuf::from("/tmp/foo-bar"));
    assert_eq!(cfg.project.name, "foo-bar");
    assert!(cfg.project.default_harness.is_none());
    assert!(cfg.harnesses.is_empty());
}

#[test]
fn test_roundtrip_with_harnesses() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    let cfg = sample_config();

    store.save(&cfg).unwrap();
    let loaded = store.load("open-mpm").unwrap().unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn test_list_skips_non_toml_and_invalid() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    store.save(&sample_config()).unwrap();

    // A non-toml file must be ignored.
    std::fs::write(dir.path().join("readme.txt"), "hi").unwrap();
    // A malformed toml must be skipped, not panic.
    std::fs::write(dir.path().join("broken.toml"), "this is not = toml [[[").unwrap();

    let configs = store.list().unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].project.name, "open-mpm");
}

#[test]
fn test_load_missing_returns_none() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    assert!(store.load("ghost").unwrap().is_none());
}

#[test]
fn test_delete_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    store.save(&sample_config()).unwrap();
    store.delete("open-mpm").unwrap();
    store.delete("open-mpm").unwrap(); // second call is a no-op
    assert!(store.load("open-mpm").unwrap().is_none());
}

#[test]
fn test_resolve_harness_default_and_explicit() {
    let cfg = sample_config();

    // Default → 'repl'.
    let h = cfg.resolve_harness(None).unwrap();
    assert_eq!(h.name, "repl");

    // Explicit → 'bash'.
    let h = cfg.resolve_harness(Some("bash")).unwrap();
    assert_eq!(h.adapter, "shell");

    // Unknown explicit → error.
    let err = cfg.resolve_harness(Some("nope")).unwrap_err();
    assert!(err.to_string().contains("not declared"));

    // Missing default → error.
    let mut cfg2 = cfg.clone();
    cfg2.project.default_harness = None;
    let err = cfg2.resolve_harness(None).unwrap_err();
    assert!(err.to_string().contains("no default_harness"));

    // Default points to unknown harness → error.
    let mut cfg3 = cfg.clone();
    cfg3.project.default_harness = Some("ghost".to_string());
    let err = cfg3.resolve_harness(None).unwrap_err();
    assert!(err.to_string().contains("not declared"));
}

#[test]
fn test_save_rejects_empty_name() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    let mut cfg = sample_config();
    cfg.project.name = String::new();
    let err = store.save(&cfg).unwrap_err();
    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn test_find_by_path_returns_existing() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    store.save(&sample_config()).unwrap();

    let found = store
        .find_by_path(Path::new("/Users/masa/Projects/open-mpm"))
        .unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().project.name, "open-mpm");
}

#[test]
fn test_find_by_path_returns_none_when_missing() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    store.save(&sample_config()).unwrap();

    let found = store.find_by_path(Path::new("/nowhere")).unwrap();
    assert!(found.is_none());
}

#[test]
fn test_find_or_create_creates_when_missing() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    let target = PathBuf::from("/tmp/auto-created");

    let cfg = store.find_or_create(&target, "claude-code", None).unwrap();

    // Name defaults to basename.
    assert_eq!(cfg.project.name, "auto-created");
    assert_eq!(cfg.project.path, target);
    assert_eq!(cfg.project.default_harness.as_deref(), Some("claude-code"));
    assert_eq!(cfg.harnesses.len(), 1);
    assert_eq!(cfg.harnesses[0].name, "claude-code");
    assert_eq!(cfg.harnesses[0].adapter, "claude-code");
    assert_eq!(cfg.harnesses[0].startup_command, "claude");

    // File was persisted and is loadable.
    let on_disk = store.load("auto-created").unwrap().unwrap();
    assert_eq!(on_disk, cfg);
}

#[test]
fn test_find_or_create_returns_existing_unchanged() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    store.save(&sample_config()).unwrap();

    let cfg = store
        .find_or_create(
            Path::new("/Users/masa/Projects/open-mpm"),
            // Different adapter id — must NOT mutate the existing config.
            "shell",
            Some("ignored-name-override"),
        )
        .unwrap();
    assert_eq!(cfg.project.name, "open-mpm");
    assert_eq!(cfg.project.default_harness.as_deref(), Some("repl"));
    assert_eq!(cfg.harnesses.len(), 2);
}

#[test]
fn test_find_or_create_honors_name_override() {
    let dir = TempDir::new().unwrap();
    let store = ProjectConfigStore::open(dir.path()).unwrap();
    let target = PathBuf::from("/tmp/some-dir");

    let cfg = store
        .find_or_create(&target, "shell", Some("custom-label"))
        .unwrap();
    assert_eq!(cfg.project.name, "custom-label");
    // File is written under the override name.
    assert!(dir.path().join("custom-label.toml").exists());
}

#[test]
fn test_default_startup_command_for_known_adapters() {
    assert_eq!(default_startup_command_for("claude-mpm"), "claude-mpm");
    assert_eq!(default_startup_command_for("claude-code"), "claude");
    assert_eq!(default_startup_command_for("codex"), "codex");
    assert_eq!(default_startup_command_for("shell"), "bash");
    assert_eq!(default_startup_command_for("open-mpm"), "om");
}

#[test]
fn test_default_startup_command_falls_back_to_bash() {
    assert_eq!(default_startup_command_for("nope"), "bash");
    assert_eq!(default_startup_command_for(""), "bash");
}

#[test]
fn test_next_session_name() {
    // No existing → -1.
    assert_eq!(
        next_session_name("open-mpm", "repl", &[]),
        "open-mpm-repl-1"
    );

    // Mix of unrelated names is ignored.
    let existing = vec![
        "open-mpm-repl-1".to_string(),
        "open-mpm-repl-2".to_string(),
        "open-mpm-bash-1".to_string(), // different harness
        "other-repl-9".to_string(),    // different project
        "open-mpm-repl-broken".to_string(),
    ];
    assert_eq!(
        next_session_name("open-mpm", "repl", &existing),
        "open-mpm-repl-3"
    );

    // Gaps still pick max+1 (we don't reuse holes — predictability wins).
    let with_gap = vec!["open-mpm-repl-1".to_string(), "open-mpm-repl-5".to_string()];
    assert_eq!(
        next_session_name("open-mpm", "repl", &with_gap),
        "open-mpm-repl-6"
    );
}

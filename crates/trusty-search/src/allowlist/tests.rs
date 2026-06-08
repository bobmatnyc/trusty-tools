//! Unit tests for the opt-in index allowlist.
//!
//! Why: keeping the test suite in a sibling file prevents `allowlist/mod.rs`
//! from exceeding the 500-line file cap while keeping every assertion co-located
//! with the module it validates.
//! What: covers denylist, AllowlistConfig CRUD, check_path semantics, and the
//! add/remove convenience helpers.
//! Test: all tests in this file are collected by `cargo test -p trusty-search`.

use super::{
    add_to_allowlist, check_path, remove_from_allowlist, AllowlistCheck, AllowlistConfig,
    AllowlistEntry,
};
use std::fs;
use std::path::{Path, PathBuf};

// ── Test helpers ──────────────────────────────────────────────────────────────

fn tmp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("ts-allowlist-{label}-{pid}-{nanos}"));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn allowlist_file(dir: &Path) -> PathBuf {
    dir.join("allowlist.toml")
}

fn entry(path: &Path) -> AllowlistEntry {
    AllowlistEntry {
        path: path.to_path_buf(),
        name: None,
        exclude: vec![],
        extensions: vec![],
        skip_kg: false,
    }
}

// ── Denylist tests ────────────────────────────────────────────────────────────

#[test]
fn denylist_blocks_ssh_dir() {
    // Why: ~/.ssh is a canonical secrets directory; indexing it must always
    // be refused, even if the user somehow adds it to the allowlist.
    let path = PathBuf::from(format!("{}/.ssh", dirs::home_dir().unwrap().display()));
    assert!(
        super::is_denied(&path).is_some(),
        "expected denial for {path:?}"
    );
}

#[test]
fn denylist_blocks_aws_dir() {
    let path = PathBuf::from(format!("{}/.aws", dirs::home_dir().unwrap().display()));
    assert!(super::is_denied(&path).is_some());
}

#[test]
fn denylist_blocks_tmp() {
    // Why: /tmp directories are ephemeral and should never be indexed.
    let path = PathBuf::from("/tmp/my-project");
    assert!(super::is_denied(&path).is_some());
}

#[test]
fn denylist_blocks_env_file_in_path() {
    // Why: a path containing /.env indicates a directory that holds env files.
    let path = PathBuf::from("/projects/.env/configs");
    assert!(super::is_denied(&path).is_some());
}

#[test]
fn denylist_blocks_home_toplevel() {
    // Why: indexing $HOME itself or ~/Desktop would capture enormous amounts
    // of personal data.
    let home = dirs::home_dir().unwrap();
    assert!(
        super::is_denied(&home).is_some(),
        "HOME itself must be denied: {home:?}"
    );
    let desktop = home.join("Desktop");
    assert!(
        super::is_denied(&desktop).is_some(),
        "~/Desktop must be denied: {desktop:?}"
    );
    let downloads = home.join("Downloads");
    assert!(super::is_denied(&downloads).is_some());
}

#[test]
fn denylist_allows_safe_path() {
    // Why: legitimate project paths must not be blocked.
    // Use a path that cannot match any pattern.
    let path = PathBuf::from("/srv/my-safe-project");
    assert!(
        super::is_denied(&path).is_none(),
        "expected safe path to pass: {path:?}"
    );
}

#[test]
fn denylist_allows_projects_under_home() {
    // Why: ~/Projects/my-repo is the typical developer setup and must be
    // allowed to pass the denylist (allowlist check is separate).
    let home = dirs::home_dir().unwrap();
    let projects = home.join("Projects").join("my-repo");
    assert!(
        super::is_denied(&projects).is_none(),
        "~/Projects/my-repo must pass the denylist: {projects:?}"
    );
}

// ── AllowlistConfig tests ─────────────────────────────────────────────────────

#[test]
fn load_returns_empty_when_missing() {
    // Why: a fresh daemon has no allowlist; missing file = default-deny.
    let dir = tmp_dir("missing");
    let path = allowlist_file(&dir);
    let cfg = AllowlistConfig::load_from(&path).unwrap();
    assert!(cfg.entries.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_errors_on_malformed_toml() {
    // Why: a corrupt allowlist must be a hard error, not a silent empty list.
    let dir = tmp_dir("malformed");
    let path = allowlist_file(&dir);
    fs::write(&path, "not: : : valid ===").unwrap();
    assert!(AllowlistConfig::load_from(&path).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn roundtrip_preserves_all_fields() {
    // Why: serialise → deserialise must be lossless so hand-edits survive a
    // daemon restart.
    let dir = tmp_dir("roundtrip");
    let path = allowlist_file(&dir);
    let cfg = AllowlistConfig {
        entries: vec![AllowlistEntry {
            path: PathBuf::from("/srv/my-project"),
            name: Some("my-proj".into()),
            exclude: vec!["target/".into()],
            extensions: vec!["rs".into(), "toml".into()],
            skip_kg: true,
        }],
    };
    cfg.save_to(&path).unwrap();
    let loaded = AllowlistConfig::load_from(&path).unwrap();
    assert_eq!(cfg, loaded);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_creates_parent_dirs() {
    // Why: first-run setup must create `~/.config/trusty-search/` if absent.
    let dir = tmp_dir("parent");
    let path = dir.join("nested").join("deep").join("indexes.toml");
    AllowlistConfig::default().save_to(&path).unwrap();
    assert!(path.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn upsert_replaces_existing_by_path() {
    // Why: re-adding the same path must update settings, not duplicate.
    let dir = tmp_dir("upsert-replace");
    fs::create_dir_all(dir.join("proj")).unwrap();
    let project = dir.join("proj");
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(AllowlistEntry {
        path: project.clone(),
        name: Some("old".into()),
        exclude: vec![],
        extensions: vec![],
        skip_kg: false,
    });
    cfg.upsert(AllowlistEntry {
        path: project.clone(),
        name: Some("new".into()),
        exclude: vec!["*.log".into()],
        extensions: vec!["rs".into()],
        skip_kg: true,
    });
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].name, Some("new".into()));
    assert!(cfg.entries[0].skip_kg);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn upsert_appends_new_path() {
    // Why: two different paths must produce two entries.
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(Path::new("/srv/a")));
    cfg.upsert(entry(Path::new("/srv/b")));
    assert_eq!(cfg.entries.len(), 2);
}

#[test]
fn remove_by_path() {
    // Why: `index remove` must evict the allowlist entry.
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(Path::new("/srv/a")));
    cfg.upsert(entry(Path::new("/srv/b")));
    let removed = cfg.remove(Path::new("/srv/a"));
    assert!(removed.is_some());
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, PathBuf::from("/srv/b"));
}

#[test]
fn remove_returns_none_for_unknown() {
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(Path::new("/srv/a")));
    assert!(cfg.remove(Path::new("/srv/unknown")).is_none());
    assert_eq!(cfg.entries.len(), 1);
}

#[test]
fn allowlist_contains_known_path() {
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(Path::new("/srv/my-project")));
    assert!(cfg.contains(Path::new("/srv/my-project")));
}

#[test]
fn allowlist_misses_unknown_path() {
    let cfg = AllowlistConfig::default();
    assert!(!cfg.contains(Path::new("/srv/unknown")));
}

// ── check_path tests ──────────────────────────────────────────────────────────

#[test]
fn check_path_denied_by_denylist() {
    // Why: denylist check must run even when the path is in the allowlist.
    let dir = tmp_dir("cp-denied");
    let allowlist = allowlist_file(&dir);

    // Put the sensitive path in the allowlist (shouldn't matter).
    let ssh = PathBuf::from(format!("{}/.ssh", dirs::home_dir().unwrap().display()));
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(&ssh));
    cfg.save_to(&allowlist).unwrap();

    let result = check_path(&ssh, Some(&allowlist)).unwrap();
    assert!(
        matches!(result, AllowlistCheck::Denied { .. }),
        "expected Denied, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_path_not_allowlisted() {
    // Why: default-deny — a safe path with no allowlist entry must be rejected.
    let dir = tmp_dir("cp-not-allowlisted");
    let allowlist = allowlist_file(&dir);
    // Create an empty allowlist file.
    AllowlistConfig::default().save_to(&allowlist).unwrap();

    let safe_path = PathBuf::from("/srv/my-safe-project");
    let result = check_path(&safe_path, Some(&allowlist)).unwrap();
    assert_eq!(result, AllowlistCheck::NotAllowlisted);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_path_not_allowlisted_when_file_missing() {
    // Why: missing allowlist file = empty allowlist = nothing allowed.
    let dir = tmp_dir("cp-no-file");
    let allowlist = allowlist_file(&dir);
    // Do NOT create the file.
    let safe_path = PathBuf::from("/srv/my-safe-project");
    let result = check_path(&safe_path, Some(&allowlist)).unwrap();
    assert_eq!(result, AllowlistCheck::NotAllowlisted);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_path_allowed() {
    // Why: a path that is in the allowlist and passes the denylist must be
    // allowed.
    let dir = tmp_dir("cp-allowed");
    let allowlist = allowlist_file(&dir);

    let safe_path = PathBuf::from("/srv/my-safe-project");
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(&safe_path));
    cfg.save_to(&allowlist).unwrap();

    let result = check_path(&safe_path, Some(&allowlist)).unwrap();
    assert_eq!(result, AllowlistCheck::Allowed);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn denylist_takes_priority_over_allowlist() {
    // Why: the hard denylist must override the allowlist. Even if a user
    // manually edits indexes.toml to add ~/.ssh, it must be refused.
    let dir = tmp_dir("deny-over-allowlist");
    let allowlist = allowlist_file(&dir);

    let sensitive = PathBuf::from(format!("{}/.aws", dirs::home_dir().unwrap().display()));
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(&sensitive));
    cfg.save_to(&allowlist).unwrap();

    let result = check_path(&sensitive, Some(&allowlist)).unwrap();
    assert!(
        matches!(result, AllowlistCheck::Denied { .. }),
        "denylist must override allowlist"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ── add_to_allowlist / remove_from_allowlist ──────────────────────────────────

#[test]
fn add_to_allowlist_persists_entry() {
    // Why: after `add_to_allowlist`, the file must contain the path and
    // `check_path` must return Allowed.
    let dir = tmp_dir("add-persists");
    let allowlist = allowlist_file(&dir);

    let safe = PathBuf::from("/srv/my-project");
    add_to_allowlist(entry(&safe), Some(&allowlist)).unwrap();

    let result = check_path(&safe, Some(&allowlist)).unwrap();
    assert_eq!(result, AllowlistCheck::Allowed);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn add_to_allowlist_blocked_by_denylist() {
    // Why: `add_to_allowlist` must refuse to persist a sensitive path.
    let dir = tmp_dir("add-blocked");
    let allowlist = allowlist_file(&dir);
    let ssh = PathBuf::from(format!("{}/.ssh", dirs::home_dir().unwrap().display()));
    let err = add_to_allowlist(entry(&ssh), Some(&allowlist));
    assert!(err.is_err());
    // The file must not have been created/modified.
    let cfg = AllowlistConfig::load_from(&allowlist).unwrap();
    assert!(cfg.entries.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn remove_from_allowlist_removes_entry() {
    // Why: after remove, `check_path` must return NotAllowlisted.
    let dir = tmp_dir("remove-entry");
    let allowlist = allowlist_file(&dir);

    let safe = PathBuf::from("/srv/my-project");
    add_to_allowlist(entry(&safe), Some(&allowlist)).unwrap();
    assert_eq!(
        check_path(&safe, Some(&allowlist)).unwrap(),
        AllowlistCheck::Allowed
    );

    remove_from_allowlist(&safe, Some(&allowlist)).unwrap();
    assert_eq!(
        check_path(&safe, Some(&allowlist)).unwrap(),
        AllowlistCheck::NotAllowlisted
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn remove_from_allowlist_noop_when_absent() {
    // Why: removing a path not in the allowlist must succeed (idempotent).
    let dir = tmp_dir("remove-noop");
    let allowlist = allowlist_file(&dir);
    AllowlistConfig::default().save_to(&allowlist).unwrap();

    // Should not error.
    remove_from_allowlist(Path::new("/srv/nonexistent"), Some(&allowlist)).unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn allowlist_path_ends_with_expected_suffix() {
    // Why: must be allowlist.toml (not indexes.toml) to avoid macOS collision.
    let p = AllowlistConfig::default_path();
    let s = p.to_string_lossy();
    assert!(
        s.ends_with("trusty-search/allowlist.toml") || s.ends_with("trusty-search-allowlist.toml"),
        "must be allowlist.toml, not indexes.toml: {s}"
    );
}

// ── Component-boundary anchoring (fix for #795 false-denial regression) ────────

#[test]
fn denylist_allows_secrets_manager_project() {
    // Why: the former substring match wrongly denied a project whose *name*
    // contained a denylist word ("secrets"). Component anchoring must allow it.
    // What: /home/user/Projects/secrets-manager is NOT a "secrets" component —
    // the component is "secrets-manager", which is a different string.
    // Test: assert is_denied returns None for this path.
    let path = PathBuf::from("/home/user/Projects/secrets-manager");
    assert!(
        super::is_denied(&path).is_none(),
        "secrets-manager project must not be denied by denylist: {path:?}"
    );
}

#[test]
fn denylist_allows_credentials_validator_project() {
    // Why: /srv/app/credentials-validator is a legitimate project name;
    // "credentials-validator" != "credentials" so it must pass.
    let path = PathBuf::from("/srv/app/credentials-validator");
    assert!(
        super::is_denied(&path).is_none(),
        "credentials-validator project must not be denied: {path:?}"
    );
}

#[test]
fn denylist_allows_config_service_project() {
    // Why: /data/projects/config-service is a legitimate project; "config-service"
    // is not the same component as ".config", so it must pass.
    let path = PathBuf::from("/data/projects/config-service");
    assert!(
        super::is_denied(&path).is_none(),
        "config-service project must not be denied: {path:?}"
    );
}

#[test]
fn denylist_blocks_exact_secrets_component() {
    // Why: /etc/secrets is the actual sensitive directory; "secrets" is an
    // exact component, so it must be denied.
    let path = PathBuf::from("/etc/secrets/x");
    assert!(
        super::is_denied(&path).is_some(),
        "/etc/secrets must be denied: {path:?}"
    );
}

#[test]
fn denylist_blocks_dot_config_component() {
    // Why: ~/.config contains application secrets; ".config" as an exact
    // component must be denied.
    let home = dirs::home_dir().unwrap();
    let path = home.join(".config").join("trusty");
    assert!(
        super::is_denied(&path).is_some(),
        "~/.config/trusty must be denied: {path:?}"
    );
}

#[test]
fn denylist_blocks_env_file() {
    // Why: a .env file at the project root is a secrets file; the final
    // component ".env" must be caught by SENSITIVE_FILE_NAMES.
    let path = PathBuf::from("/home/user/myproject/.env");
    assert!(
        super::is_denied(&path).is_some(),
        ".env file must be denied: {path:?}"
    );
}

#[test]
fn denylist_denied_path_still_rejected_when_allowlisted() {
    // Why: even when TRUSTY_ALLOW_UNLISTED logic in server.rs is bypassed,
    // `check_path` itself must still deny a path that is in the allowlist
    // but also hits the hard denylist.
    // What: put /etc/secrets in the allowlist; check_path must return Denied.
    let dir = tmp_dir("denylist-priority");
    let allowlist = allowlist_file(&dir);

    let sensitive = PathBuf::from("/etc/secrets");
    let mut cfg = AllowlistConfig::default();
    cfg.upsert(entry(&sensitive));
    cfg.save_to(&allowlist).unwrap();

    let result = check_path(&sensitive, Some(&allowlist)).unwrap();
    assert!(
        matches!(result, AllowlistCheck::Denied { .. }),
        "denylist must block an allowlisted sensitive path; got {result:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

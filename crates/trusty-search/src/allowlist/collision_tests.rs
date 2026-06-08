//! Regression tests for the macOS `indexes.toml` path collision (missing field `path`).
//!
//! Why: on macOS `dirs::config_dir()` == `dirs::data_local_dir()` ==
//! `~/Library/Application Support`, so the allowlist and the daemon registry
//! historically resolved to the same `indexes.toml` file. Loading it as an
//! `AllowlistConfig` failed with "missing field `path`" because daemon entries
//! use `root_path`, not `path`. The fix: rename the allowlist file to
//! `allowlist.toml` and add a `root_path` serde alias as defence-in-depth.
//! What: four tests covering file-name separation, serde alias, and the
//! full write path when both files coexist in the same directory.
//! Test: collected by `cargo test -p trusty-search`.

use super::{add_to_allowlist, AllowlistConfig, AllowlistEntry};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("ts-coll-{label}-{pid}-{nanos}"));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
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

/// Daemon-registry TOML that uses `root_path` (no `path` field).
const DAEMON_TOML: &str = "[[index]]\nid = \"p\"\nroot_path = \"/srv/project\"\n";

#[test]
fn allowlist_path_does_not_collide_with_daemon_registry() {
    // Why: macOS config_dir==data_local_dir; allowlist must be allowlist.toml,
    // not indexes.toml, to avoid loading daemon entries as AllowlistEntry.
    let filename = AllowlistConfig::default_path()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    assert_eq!(filename, "allowlist.toml", "got: {filename}");
    assert_ne!(
        filename, "indexes.toml",
        "allowlist must not use the daemon registry filename"
    );
}

#[test]
fn root_path_alias_deserializes() {
    // Why: defence-in-depth — daemon entries use `root_path`; the alias must
    // let AllowlistConfig::load_from succeed without "missing field `path`".
    let cfg: AllowlistConfig = toml::from_str(DAEMON_TOML).expect("root_path alias must parse OK");
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, PathBuf::from("/srv/project"));
}

#[test]
fn allowlist_load_from_daemon_registry_succeeds_via_alias() {
    // Why: even when load_from is accidentally pointed at indexes.toml, the
    // root_path alias must prevent a hard parse error.
    let dir = tmp_dir("coll-load");
    let daemon_registry = dir.join("indexes.toml");
    fs::write(&daemon_registry, DAEMON_TOML).unwrap();

    let cfg = AllowlistConfig::load_from(&daemon_registry).unwrap();
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, PathBuf::from("/srv/project"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn add_to_allowlist_succeeds_when_daemon_registry_colocated() {
    // Why: the real macOS failure — both files land in the same
    // `~/Library/Application Support/trusty-search/` directory.
    // After the rename fix they are separate files; add_to_allowlist must
    // write allowlist.toml without touching or mis-parsing indexes.toml.
    let dir = tmp_dir("coll-add");
    let daemon_registry = dir.join("indexes.toml");
    fs::write(&daemon_registry, DAEMON_TOML).unwrap();

    let allowlist = dir.join("allowlist.toml");
    let safe = PathBuf::from("/srv/my-project");
    add_to_allowlist(entry(&safe), Some(&allowlist)).unwrap(); // must not Err

    let cfg = AllowlistConfig::load_from(&allowlist).unwrap();
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, safe);
    assert!(
        fs::read_to_string(&daemon_registry)
            .unwrap()
            .contains("/srv/project"),
        "daemon registry must be untouched"
    );
    let _ = fs::remove_dir_all(&dir);
}

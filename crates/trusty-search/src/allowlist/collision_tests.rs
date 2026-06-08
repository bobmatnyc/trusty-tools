//! Regression tests for the macOS `indexes.toml` path collision and the
//! one-time legacy migration.
//!
//! Why: on macOS `dirs::config_dir()` == `dirs::data_local_dir()` ==
//! `~/Library/Application Support`, so the allowlist and the daemon registry
//! historically resolved to the same `indexes.toml` file. Loading it as an
//! `AllowlistConfig` failed with "missing field `path`" because daemon entries
//! use `root_path`, not `path`. Fix 1: rename the allowlist file to
//! `allowlist.toml`. Fix 2: remove the `root_path` serde alias so
//! daemon-registry entries cannot silently become allowlist approvals.
//!
//! Migration tests verify that a real allowlist at the old path is promoted to
//! `allowlist.toml` on first load, but that a daemon-registry file at the old
//! path is NOT migrated (no error, no entry).
//!
//! What: covers file-name separation, alias removal, co-location write path,
//! and the full migration scenarios.
//! Test: collected by `cargo test -p trusty-search`.

use super::{add_to_allowlist, try_migrate_legacy, AllowlistConfig, AllowlistEntry};
use std::path::PathBuf;
use tempfile::TempDir;

fn entry(path: &std::path::Path) -> AllowlistEntry {
    AllowlistEntry {
        path: path.to_path_buf(),
        name: None,
        exclude: vec![],
        extensions: vec![],
        skip_kg: false,
    }
}

/// Daemon-registry TOML: uses `id` + `root_path` per entry (no `path` field).
/// After removing the `root_path` alias this file must NOT parse as
/// `AllowlistConfig` with any entries.
const DAEMON_TOML: &str = "[[index]]\nid = \"p\"\nroot_path = \"/srv/project\"\n";

/// A genuine allowlist TOML (uses the `path` field, no `id`).
const REAL_ALLOWLIST_TOML: &str = "[[index]]\npath = \"/srv/my-project\"\n";

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
fn daemon_registry_does_not_parse_as_allowlist() {
    // Why: the `root_path` serde alias was removed. A daemon-registry file
    // (entries use `root_path` + `id`, no `path` key) must NOT yield any
    // populated allowlist entries. This prevents every registered daemon index
    // from becoming an implicit allowlist approval.
    // What: DAEMON_TOML fails to deserialize the `path` field → either a
    // parse error or an empty entries vec. Both are acceptable (serde may
    // surface this as an error or, with `#[serde(default)]` on the entries
    // vec, as zero entries). Assert that no entry with `path = /srv/project`
    // leaks through.
    let parsed = toml::from_str::<AllowlistConfig>(DAEMON_TOML);
    match parsed {
        Ok(cfg) => {
            // If it parsed (e.g. because serde skipped the missing `path`),
            // there must be zero entries — daemon entries must not be treated
            // as approvals.
            assert!(
                cfg.entries.is_empty(),
                "daemon-registry entries must not become allowlist approvals; \
                 got {} entries",
                cfg.entries.len()
            );
        }
        Err(_) => {
            // A hard parse error is the expected outcome and is also acceptable.
        }
    }
}

#[test]
fn allowlist_load_from_daemon_registry_yields_no_entries() {
    // Why: even when load_from is accidentally pointed at indexes.toml, the
    // result must be empty (no daemon entries imported as approvals).
    let dir = TempDir::new().unwrap();
    let daemon_registry = dir.path().join("indexes.toml");
    std::fs::write(&daemon_registry, DAEMON_TOML).unwrap();

    let cfg = AllowlistConfig::load_from(&daemon_registry);
    match cfg {
        Ok(c) => {
            assert!(
                c.entries.is_empty(),
                "daemon-registry entries must not become allowlist approvals; \
                 got {} entries",
                c.entries.len()
            );
        }
        Err(_) => {
            // Parse error is also acceptable — daemon-registry TOML is not
            // a valid allowlist file after the alias removal.
        }
    }
}

#[test]
fn add_to_allowlist_succeeds_when_daemon_registry_colocated() {
    // Why: the real macOS failure — both files land in the same
    // `~/Library/Application Support/trusty-search/` directory.
    // After the rename fix they are separate files; add_to_allowlist must
    // write allowlist.toml without touching or mis-parsing indexes.toml.
    let dir = TempDir::new().unwrap();
    let daemon_registry = dir.path().join("indexes.toml");
    std::fs::write(&daemon_registry, DAEMON_TOML).unwrap();

    let allowlist = dir.path().join("allowlist.toml");
    let safe = PathBuf::from("/srv/my-project");
    add_to_allowlist(entry(&safe), Some(&allowlist)).unwrap(); // must not Err

    let cfg = AllowlistConfig::load_from(&allowlist).unwrap();
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, safe);
    assert!(
        std::fs::read_to_string(&daemon_registry)
            .unwrap()
            .contains("/srv/project"),
        "daemon registry must be untouched"
    );
}

// ── Migration tests ───────────────────────────────────────────────────────────

#[test]
fn migration_real_allowlist_is_migrated() {
    // Why: a Linux user whose old indexes.toml was a genuine allowlist (entries
    // used the `path` field) must have their entries automatically promoted to
    // the new allowlist.toml on first load.
    // What: create a real allowlist at the legacy path; call try_migrate_legacy
    // with the new (absent) path; assert allowlist.toml now exists with the
    // same entries.
    let dir = TempDir::new().unwrap();
    let new_path = dir.path().join("allowlist.toml");
    let legacy_path = dir.path().join("indexes.toml");

    std::fs::write(&legacy_path, REAL_ALLOWLIST_TOML).unwrap();

    try_migrate_legacy(&new_path, &legacy_path);

    assert!(
        new_path.exists(),
        "allowlist.toml must be created by migration"
    );
    let cfg = AllowlistConfig::load_from(&new_path).unwrap();
    assert_eq!(
        cfg.entries.len(),
        1,
        "migrated allowlist must contain the original entry"
    );
    assert_eq!(cfg.entries[0].path, PathBuf::from("/srv/my-project"));
}

#[test]
fn migration_daemon_registry_is_not_migrated() {
    // Why: on macOS the old indexes.toml at config_dir() is the daemon registry
    // (entries have `id` + `root_path`, no `path` field). Migrating it would
    // import every registered daemon index as an allowlist approval, defeating
    // the opt-in security gate.
    // What: create a daemon-registry-shaped file at the legacy path; call
    // try_migrate_legacy; assert allowlist.toml is NOT created.
    let dir = TempDir::new().unwrap();
    let new_path = dir.path().join("allowlist.toml");
    let legacy_path = dir.path().join("indexes.toml");

    std::fs::write(&legacy_path, DAEMON_TOML).unwrap();

    try_migrate_legacy(&new_path, &legacy_path);

    assert!(
        !new_path.exists(),
        "allowlist.toml must NOT be created when legacy file is a daemon registry"
    );
}

#[test]
fn migration_skipped_when_new_path_exists() {
    // Why: migration must be idempotent — if allowlist.toml already exists,
    // try_migrate_legacy must not overwrite it.
    let dir = TempDir::new().unwrap();
    let new_path = dir.path().join("allowlist.toml");
    let legacy_path = dir.path().join("indexes.toml");

    // Write a different allowlist to the new path first.
    let existing = AllowlistConfig {
        entries: vec![AllowlistEntry {
            path: PathBuf::from("/srv/existing"),
            name: None,
            exclude: vec![],
            extensions: vec![],
            skip_kg: false,
        }],
    };
    existing.save_to(&new_path).unwrap();

    // Also write a real allowlist to the legacy path.
    std::fs::write(&legacy_path, REAL_ALLOWLIST_TOML).unwrap();

    try_migrate_legacy(&new_path, &legacy_path);

    // The new path must be unchanged.
    let cfg = AllowlistConfig::load_from(&new_path).unwrap();
    assert_eq!(cfg.entries.len(), 1);
    assert_eq!(cfg.entries[0].path, PathBuf::from("/srv/existing"));
}

#[test]
fn migration_skipped_when_legacy_absent() {
    // Why: no legacy file = nothing to migrate; no error, no file created.
    let dir = TempDir::new().unwrap();
    let new_path = dir.path().join("allowlist.toml");
    let legacy_path = dir.path().join("indexes.toml"); // does not exist

    try_migrate_legacy(&new_path, &legacy_path);

    assert!(
        !new_path.exists(),
        "no allowlist.toml should be created when legacy file is absent"
    );
}

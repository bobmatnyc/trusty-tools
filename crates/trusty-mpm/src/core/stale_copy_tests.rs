//! Tests for the stale-copy config and store (#10574).

use std::path::Path;

use crate::core::stale_copy::{
    Freshness, ObservedStore, RawStaleCopy, StaleCopyConfig, StaleCopyLists, StaleCopyState,
    sha256_hex, stale_copy_for,
};

fn raw(toml_src: &str) -> RawStaleCopy {
    let value: toml::Value = toml::from_str(toml_src).expect("toml");
    RawStaleCopy(value)
}

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), config).expect("write config");
    dir
}

#[test]
fn validate_rejects_unknown_keys_and_escaping_patterns() {
    let ok = raw("paths = [\"docs/**/*.md\"]\nlocked = [\"docs/a.md\"]")
        .validate()
        .expect("valid");
    assert_eq!(ok.paths, vec!["docs/**/*.md"]);
    assert_eq!(ok.locked, vec!["docs/a.md"]);
    for bad in [
        "pathz = [\"a\"]",
        "paths = \"docs\"",
        "paths = [1]",
        "paths = [\"/etc/passwd\"]",
        "paths = [\"../x.md\"]",
        "paths = [\"\"]",
        "paths = [\"docs/[\"]",
    ] {
        assert!(raw(bad).validate().is_err(), "{bad} must be rejected");
    }
}

#[test]
fn classify_matches_registered_and_locked_paths() {
    let root = Path::new("/p");
    let lists = StaleCopyLists {
        paths: vec!["docs/*.md".into()],
        locked: vec!["docs/rules.md".into()],
    };
    let cfg = StaleCopyConfig::new(root, &lists).expect("compile");
    let a = cfg.classify("docs/x.md", root).expect("registered");
    assert_eq!(a.rel, "docs/x.md");
    assert!(!a.locked);
    assert!(
        cfg.classify("/p/docs/rules.md", root)
            .expect("locked")
            .locked
    );
    assert!(
        cfg.classify("x.md", &root.join("docs")).is_some(),
        "relative to cwd"
    );
    // `*` does not cross `/`.
    assert!(cfg.classify("docs/sub/x.md", root).is_none());
    assert!(cfg.classify("src/lib.rs", root).is_none());
}

#[test]
fn classify_ignores_paths_outside_the_project() {
    let root = Path::new("/p");
    let lists = StaleCopyLists {
        paths: vec!["**".into()],
        locked: vec![],
    };
    let cfg = StaleCopyConfig::new(root, &lists).expect("compile");
    assert!(cfg.classify("/q/docs/x.md", root).is_none());
    assert!(cfg.classify("../q/x.md", root).is_none());
}

#[test]
fn state_is_inert_without_a_table_or_with_empty_lists() {
    let none = tempfile::tempdir().expect("tempdir");
    assert!(matches!(stale_copy_for(none.path()), StaleCopyState::Inert));
    let no_table = project("worktree = false\n");
    assert!(matches!(
        stale_copy_for(no_table.path()),
        StaleCopyState::Inert
    ));
    let empty = project("[stale_copy]\npaths = []\n");
    assert!(matches!(
        stale_copy_for(empty.path()),
        StaleCopyState::Inert
    ));
}

#[test]
fn state_is_rejected_for_a_malformed_config() {
    let bad_file = project("this is not toml = = \n");
    assert!(matches!(
        stale_copy_for(bad_file.path()),
        StaleCopyState::Rejected(_)
    ));
    let bad_table = project("[stale_copy]\npaths = [\"/abs\"]\n");
    assert!(matches!(
        stale_copy_for(bad_table.path()),
        StaleCopyState::Rejected(_)
    ));
}

#[test]
fn state_is_active_from_a_subdirectory() {
    let dir = project("worktree = false\n[stale_copy]\npaths = [\"docs/*.md\"]\n");
    let sub = dir.path().join("docs");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let StaleCopyState::Active(cfg) = stale_copy_for(&sub) else {
        panic!("expected an active guard");
    };
    assert!(cfg.classify("x.md", &sub).is_some());
}

#[test]
fn record_then_lookup_round_trips_with_a_saved_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ObservedStore::at(dir.path().join("store"));
    let abs = dir.path().join("x.md");
    let rec = store.record(&abs, b"v1").expect("record");
    assert_eq!(rec.sha256, sha256_hex(b"v1"));
    assert!(rec.copy_saved);
    assert_eq!(store.lookup(&abs).expect("lookup"), Some(rec));
    assert_eq!(std::fs::read(store.copy_path(&abs)).expect("copy"), b"v1");
    store.forget(&abs).expect("forget");
    assert_eq!(store.lookup(&abs).expect("lookup"), None);
}

#[test]
fn freshness_covers_every_arm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ObservedStore::at(dir.path().join("store"));
    let abs = dir.path().join("x.md");
    assert_eq!(store.freshness(&abs), Freshness::Absent);
    std::fs::write(&abs, "v1").expect("write");
    assert_eq!(store.freshness(&abs), Freshness::NeverRead);
    store.record(&abs, b"v1").expect("record");
    assert_eq!(store.freshness(&abs), Freshness::Fresh);
    std::fs::write(&abs, "v1 + owner edit").expect("write");
    assert!(matches!(store.freshness(&abs), Freshness::Changed { .. }));
}

#[test]
fn an_unreadable_record_is_a_store_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_dir = dir.path().join("store");
    let store = ObservedStore::at(store_dir.clone());
    let abs = dir.path().join("x.md");
    std::fs::write(&abs, "v1").expect("write");
    store.record(&abs, b"v1").expect("record");
    // Corrupt every record: the store answers an error, never "fresh".
    for entry in std::fs::read_dir(&store_dir).expect("ls").flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            std::fs::write(entry.path(), "{not json").expect("corrupt");
        }
    }
    assert!(store.lookup(&abs).is_err());
    assert!(matches!(store.freshness(&abs), Freshness::Unreadable(_)));
}

#[test]
fn store_evicts_the_oldest_records_past_the_count_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_dir = dir.path().join("store");
    let store = ObservedStore::at(store_dir.clone());
    let total = crate::core::stale_copy::MAX_RECORDS + 3;
    for i in 0..total {
        store
            .record(&dir.path().join(format!("f{i}.md")), b"x")
            .expect("record");
    }
    let records = std::fs::read_dir(&store_dir)
        .expect("ls")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .count();
    assert_eq!(records, crate::core::stale_copy::MAX_RECORDS);
    let last = dir.path().join(format!("f{}.md", total - 1));
    assert!(
        store.lookup(&last).expect("lookup").is_some(),
        "newest kept"
    );
}

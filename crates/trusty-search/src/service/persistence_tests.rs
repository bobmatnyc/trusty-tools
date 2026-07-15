//! Unit tests for `service::persistence` (extracted from `persistence.rs` to
//! keep the production file under the 500-SLOC cap — issue #1372).
//!
//! Why: `persistence.rs` carries the registry-file (de)serialisation, per-index
//! path helpers, and the issue #1372 hygiene-field defaults; its inline test
//! module pushed the file over the production SLOC cap. Splitting the tests into
//! this sibling `#[path]`-included module restores compliance without changing
//! coverage.
//! What: all the round-trip / default / sanitisation tests for `PersistedIndex`
//! and the data-dir helpers.
//! Test: this module IS the tests.

use super::*;

#[test]
fn sanitize_strips_unsafe_chars() {
    assert_eq!(sanitize_id("good-name_1.0"), "good-name_1.0");
    // `.` is in the allow-set; `/` becomes `_`. So `../escape` becomes
    // `.._escape`. The important invariant is that no path separator
    // survives, not that dots are stripped.
    assert_eq!(sanitize_id("../escape"), ".._escape");
    assert_eq!(sanitize_id("with spaces/slash"), "with_spaces_slash");
}

#[test]
fn registry_file_serde_roundtrip() {
    // Just exercise the (de)serializer without touching the filesystem.
    let file = IndexRegistryFile {
        indexes: vec![
            PersistedIndex {
                id: "a".into(),
                root_path: PathBuf::from("/tmp/a"),
                ..Default::default()
            },
            PersistedIndex {
                id: "b".into(),
                root_path: PathBuf::from("/tmp/b"),
                ..Default::default()
            },
        ],
    };
    let s = toml::to_string_pretty(&file).unwrap();
    let parsed: IndexRegistryFile = toml::from_str(&s).unwrap();
    assert_eq!(parsed.indexes, file.indexes);
}

/// Regression test for issue #118: `DELETE /indexes/:id` must rewrite
/// `indexes.toml` so the removal survives a daemon restart.
///
/// Why: production accumulated 60+ "deleted" indexes because the DELETE
/// path only mutated the in-memory `DashMap`. Each empty entry replayed
/// from disk pre-allocates an HNSW arena (80–150 MB). The fix wires
/// `delete_index_handler` to `remove_index_registry_entry`; this test
/// pins that behaviour at the persistence boundary by driving the
/// load/save/remove pipeline against a tempfile and asserting the
/// deleted id is absent from the rehydrated registry.
#[test]
fn remove_index_persists_to_toml() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "keep".into(),
            root_path: PathBuf::from("/tmp/keep"),
            ..Default::default()
        },
    )
    .unwrap();
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "drop".into(),
            root_path: PathBuf::from("/tmp/drop"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(load_index_registry_at(&path).unwrap().len(), 2);

    // Delete the second entry — this is the persistence call that
    // `delete_index_handler` makes on the DELETE handler path.
    remove_index_registry_entry_at(&path, "drop").unwrap();

    // Rehydrate from disk (simulating a daemon restart) and confirm only
    // the survivor comes back. This is the assertion that would have
    // failed before the fix.
    let restored = load_index_registry_at(&path).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, "keep");
    assert!(restored.iter().all(|e| e.id != "drop"));

    // Idempotent delete: removing again is a silent no-op.
    remove_index_registry_entry_at(&path, "drop").unwrap();
    assert_eq!(load_index_registry_at(&path).unwrap().len(), 1);
}

/// Regression test for the add-side of issue #118: re-registering the
/// same `id` must upsert (not append) in the on-disk file.
///
/// Why: if `POST /indexes` appended a duplicate `[[index]]` block on
/// every call, a flapping daemon would build up the same accumulation
/// pathology the DELETE bug caused — every duplicate replays as a
/// separate HNSW arena at startup.
#[test]
fn upsert_index_dedupes_on_id() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "proj".into(),
            root_path: PathBuf::from("/old"),
            ..Default::default()
        },
    )
    .unwrap();
    // Re-register with the same id but a different root_path.
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "proj".into(),
            root_path: PathBuf::from("/new"),
            ..Default::default()
        },
    )
    .unwrap();

    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1, "duplicate [[index]] block written");
    assert_eq!(entries[0].root_path, PathBuf::from("/new"));
}

/// Issue #100: `respect_gitignore` defaults to `true` on every code path —
/// constructor, missing-field deserialisation, and after a save/load
/// round-trip. This pins the back-compat contract: an `indexes.toml`
/// written by a previous trusty-search version must pick up the
/// gitignore-honouring fix automatically on warm boot.
#[test]
fn respect_gitignore_defaults_true_and_round_trips() {
    // Default constructor returns true.
    assert!(PersistedIndex::default().respect_gitignore);

    // Loading legacy TOML without the field gives true.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].respect_gitignore,
        "missing field must default to true (issue #100 back-compat)"
    );

    // Explicit false survives save/load cycle.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "vendored".into(),
            root_path: PathBuf::from("/tmp/v"),
            respect_gitignore: false,
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].respect_gitignore);
}

/// `follow_links` back-compat contract. Two DIFFERENT defaults meet here on
/// purpose, so the test pins both. First: a legacy `indexes.toml` entry that
/// predates the field deserialises as `true` — those indexes were built while
/// the walker followed symlinks unconditionally, so keeping them following is
/// the least-surprising behaviour on upgrade (no silent index reshuffle).
/// Second: a NEW index registered via `POST /indexes` gets `false` from the
/// create handler (`req.follow_links.unwrap_or(false)`), which is tested at the
/// server layer rather than by this constructor default. Finally, the explicit
/// `false` opt-out must survive a save/load round-trip.
#[test]
fn follow_links_missing_field_defaults_true_and_explicit_false_round_trips() {
    // Constructor (Default) mirrors the legacy serde default.
    assert!(
        PersistedIndex::default().follow_links,
        "Default::default() must carry the legacy follow_links=true"
    );

    // Loading legacy TOML without the field gives true (back-compat).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].follow_links,
        "missing field must default to true so pre-existing indexes keep following symlinks"
    );

    // A new-index opt-out (follow_links = false) survives save/load.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "no-follow".into(),
            root_path: PathBuf::from("/tmp/nf"),
            follow_links: false,
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].follow_links,
        "explicit follow_links=false must persist across a save/load cycle"
    );
}

/// Issue #118: `include_docs` defaults to `true` on every code path —
/// constructor, missing-field deserialisation, and after a save/load
/// round-trip. This pins the back-compat migration story: an
/// `indexes.toml` written by v0.8.2 (where `include_docs = false` was
/// the default and would be omitted from the file by
/// `skip_serializing_if = "std::ops::Not::not"`) now reads back as
/// `true` under v0.8.3 — `mode=text` searches start returning results
/// on the next daemon restart without any explicit migration step.
/// Indexes that PERSISTED an explicit `include_docs = false` keep
/// their opt-out via the explicit-false round-trip case below.
#[test]
fn include_docs_defaults_true_and_round_trips() {
    // Default constructor returns true.
    assert!(PersistedIndex::default().include_docs);

    // Loading legacy TOML without the field gives true — this is the
    // v0.8.2 → v0.8.3 silent migration: missing field becomes the new
    // default.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].include_docs,
        "missing field must default to true (issue #118 migration)"
    );

    // Explicit false survives save/load cycle — opt-out users keep their
    // setting through the upgrade.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "docs_off".into(),
            root_path: PathBuf::from("/tmp/v"),
            include_docs: false,
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].include_docs);
}

/// Issue #109 Phase 1: `lexical_only` defaults to `false` and is
/// omitted from the TOML when unset, so existing `indexes.toml` files
/// keep their compact shape. An explicit `true` survives a save/load
/// cycle.
#[test]
fn lexical_only_round_trips() {
    // Default constructor returns false.
    assert!(!PersistedIndex::default().lexical_only);

    // Loading legacy TOML without the field gives false (full pipeline).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].lexical_only,
        "missing field must default to false (issue #109 back-compat)"
    );

    // Explicit true survives round-trip and is written to disk.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "lex_only".into(),
            root_path: PathBuf::from("/tmp/v"),
            lexical_only: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("lexical_only"),
        "explicit true must be serialised — TOML was: {s}"
    );
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].lexical_only);
}

/// Issue #313: `skip_kg` defaults to `false` and is omitted from the TOML
/// when unset, preserving the compact shape of existing `indexes.toml`
/// files. An explicit `true` survives a save/load round-trip, and an
/// `indexes.toml` written by an older daemon (without the field) loads as
/// `false` (full KG pipeline unchanged).
///
/// Why: pins the backward-compat contract so a daemon upgrade never
/// silently drops the KG for existing indexes.
/// What: default constructor, missing-field deserialization, and
/// explicit-true round-trip — the same three shapes as `lexical_only`.
/// Test: this test.
#[test]
fn skip_kg_round_trips() {
    // Default constructor returns false.
    assert!(!PersistedIndex::default().skip_kg);

    // Loading legacy TOML without the field gives false (KG enabled).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].skip_kg,
        "missing field must default to false (issue #313 back-compat)"
    );

    // Explicit true survives round-trip and is written to disk.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "no_kg".into(),
            root_path: PathBuf::from("/tmp/v"),
            skip_kg: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("skip_kg"),
        "explicit true must be serialised — TOML was: {s}"
    );
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].skip_kg);

    // skip_kg and lexical_only can coexist independently.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "both_flags".into(),
            root_path: PathBuf::from("/tmp/v"),
            lexical_only: true,
            skip_kg: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].lexical_only, "lexical_only preserved");
    assert!(entries[0].skip_kg, "skip_kg preserved");
}

/// Issue #403: `colocated` defaults to `false` so existing `indexes.toml`
/// files load as legacy global storage. An explicit `true` survives a
/// save/load round-trip, and is written to TOML only when set.
///
/// Why: pins the backward-compat contract: a legacy `indexes.toml` without
/// the field keeps using global storage after a daemon upgrade.
/// What: default constructor, missing-field deserialization, explicit-true
/// round-trip, and verification that `colocated = true` + root_path-aware
/// helpers resolve inside the root.
/// Test: this test.
#[test]
fn colocated_flag_round_trips() {
    // Default constructor returns false.
    assert!(!PersistedIndex::default().colocated);

    // Loading legacy TOML without the field gives false (global storage).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy_col"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].colocated,
        "missing field must default to false (issue #403 back-compat)"
    );

    // Explicit true survives round-trip and is written to disk.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let root_dir = tempfile::tempdir().unwrap();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "colocated_idx".into(),
            root_path: root_dir.path().to_path_buf(),
            colocated: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("colocated"),
        "explicit true must be serialised — TOML was: {s}"
    );
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].colocated);

    // The root-path-aware helpers must resolve inside the root when colocated.
    let hnsw = super::hnsw_path_for_entry(&entries[0]).unwrap();
    assert!(
        hnsw.starts_with(root_dir.path()),
        "colocated hnsw path must be inside root; got {hnsw:?}"
    );
    let redb = super::corpus_redb_path_for_entry(&entries[0]).unwrap();
    assert!(
        redb.starts_with(root_dir.path()),
        "colocated redb path must be inside root; got {redb:?}"
    );
}

/// DOC-37 (issue #2611): `repo_identity` defaults to `None`, legacy TOML without
/// the field loads as `None` (backward compatible), and an explicit value both
/// round-trips and is serialised to disk.
#[test]
fn repo_identity_round_trips() {
    // Default constructor: no identity.
    assert_eq!(PersistedIndex::default().repo_identity, None);

    // Legacy TOML without the field must load as None (back-compat).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy_identity"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].repo_identity, None,
        "missing repo_identity must default to None (DOC-37 back-compat)"
    );

    // Explicit value survives round-trip and is written to disk.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "with_identity".into(),
            root_path: "/tmp/with_identity".into(),
            repo_identity: Some("bobmatnyc/trusty-tools".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("repo_identity") && s.contains("bobmatnyc/trusty-tools"),
        "explicit repo_identity must be serialised — TOML was: {s}"
    );
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(
        entries[0].repo_identity.as_deref(),
        Some("bobmatnyc/trusty-tools")
    );
}

/// Issue #1372: `extra_skip_dirs` / `data_file_max_bytes` default to the
/// targeted hygiene values on every code path — constructor, missing-field
/// deserialisation (legacy TOML picks up the fix), and explicit round-trip.
#[test]
fn data_file_hygiene_round_trips() {
    // Default constructor carries the targeted defaults.
    let def = PersistedIndex::default();
    assert!(def.extra_skip_dirs.contains(&"data".to_string()));
    assert_eq!(def.extra_skip_dirs.len(), 6);
    assert_eq!(def.data_file_max_bytes, Some(65_536));

    // Legacy TOML without the fields loads with the defaults (migration).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(
        &path,
        r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].extra_skip_dirs.contains(&"reports".to_string()),
        "missing field must default to the targeted set: {:?}",
        entries[0].extra_skip_dirs
    );
    assert_eq!(
        entries[0].data_file_max_bytes,
        Some(65_536),
        "missing field must default to Some(64 KiB)"
    );

    // Explicit values survive a save/load cycle.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "custom".into(),
            root_path: PathBuf::from("/tmp/v"),
            extra_skip_dirs: vec!["archive".to_string()],
            data_file_max_bytes: Some(8192),
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].extra_skip_dirs, vec!["archive".to_string()]);
    assert_eq!(entries[0].data_file_max_bytes, Some(8192));

    // resolve helper: None falls back to the 64 KiB default.
    assert_eq!(resolve_data_file_max_bytes(None), 65_536);
    assert_eq!(resolve_data_file_max_bytes(Some(4096)), 4096);
}

#[test]
fn registry_upsert_idempotent_unit() {
    // Exercise the upsert *logic* without touching disk: simulate the
    // load → modify → save round-trip by manipulating the vector directly.
    let mut entries = vec![PersistedIndex {
        id: "a".into(),
        root_path: PathBuf::from("/old"),
        ..Default::default()
    }];
    let new = PersistedIndex {
        id: "a".into(),
        root_path: PathBuf::from("/new"),
        ..Default::default()
    };
    if let Some(existing) = entries.iter_mut().find(|e| e.id == new.id) {
        existing.root_path = new.root_path.clone();
    } else {
        entries.push(new);
    }
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].root_path, PathBuf::from("/new"));
}

/// Issue #281/#718: `data_dir()` must return the override path when
/// `TRUSTY_DATA_DIR` is set (absolute, cwd-independent).
/// Why: isolated daemons and launchd restarts must land in the override dir.
/// What: set env var, call `data_dir()`, assert path matches and dir exists.
/// Test: this test. Timestamp round-trip tests live in `persistence_timestamps`.
#[test]
#[serial_test::serial]
fn data_dir_respects_trusty_data_dir_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let unique = tmp.path().join("persistence_data_dir_test");
    std::fs::create_dir_all(&unique).unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", &unique) };
    let result = data_dir();
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    let dir = result.expect("data_dir with TRUSTY_DATA_DIR must succeed");
    assert_eq!(dir, unique, "data_dir() should return the override path");
    assert!(
        dir.exists(),
        "data_dir() should ensure the directory exists"
    );
}

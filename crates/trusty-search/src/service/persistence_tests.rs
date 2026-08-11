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

/// Issue #2984 Phase 1: `skip_vector` defaults to `false` and is omitted from
/// the TOML when unset, preserving the compact shape of existing
/// `indexes.toml` files. An explicit `true` survives a save/load round-trip,
/// an `indexes.toml` written by an older daemon (without the field) loads as
/// `false` (vector lane enabled), and `skip_vector` coexists independently
/// with `skip_kg` (the vector-off/KG-on quadrant).
///
/// Why: mirrors `skip_kg_round_trips` — pins the same backward-compat
/// contract for the new per-component vector flag.
/// What: default constructor, missing-field deserialization, explicit-true
/// round-trip, and orthogonality with `skip_kg` — the same shapes as
/// `skip_kg_round_trips`.
/// Test: this test.
#[test]
fn skip_vector_round_trips() {
    // Default constructor returns false.
    assert!(!PersistedIndex::default().skip_vector);

    // Loading legacy TOML without the field gives false (vector enabled).
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
        !entries[0].skip_vector,
        "missing field must default to false (issue #2984 back-compat)"
    );

    // Explicit true survives round-trip and is written to disk.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "no_vector".into(),
            root_path: PathBuf::from("/tmp/v"),
            skip_vector: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("skip_vector"),
        "explicit true must be serialised — TOML was: {s}"
    );
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].skip_vector);

    // skip_vector and skip_kg can coexist independently (the KG-on,
    // vector-off quadrant).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_index_registry_at(
        &path,
        &[PersistedIndex {
            id: "vector_off_kg_on".into(),
            root_path: PathBuf::from("/tmp/v"),
            skip_kg: false,
            skip_vector: true,
            ..Default::default()
        }],
    )
    .unwrap();
    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].skip_kg, "skip_kg stays false");
    assert!(entries[0].skip_vector, "skip_vector preserved");
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

/// Issue #4255: isolating tests must not have moved production too.
///
/// Why: `default_data_dir` now branches on a runtime test-harness check. The
/// isolation tests prove the test branch is taken; nothing proved the OTHER
/// branch still points where the daemon's data actually lives. A silent
/// regression there would strand every existing index on the next release.
/// What: calls the production resolver directly (bypassing the branch) and
/// asserts it equals the well-known user location.
/// Test: this test.
#[test]
fn production_data_dir_is_the_real_user_location() {
    let Some(expected) = dirs::data_local_dir().map(|b| b.join("trusty-search")) else {
        // `dirs::data_local_dir()` unavailable — that is the issue #718
        // fallback path, covered by `data_dir_home_fallback_path_is_absolute`.
        return;
    };
    let dir = production_data_dir().expect("production data dir must resolve");
    assert_eq!(
        dir, expected,
        "the production data dir must stay the operator's real location"
    );
}

// ─── #4317 / #4871: registry read-modify-write integrity ─────────────────────

/// Build a minimal entry for the registry-integrity tests below.
fn reg_entry(id: &str) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/{id}")),
        ..Default::default()
    }
}

/// #4317 / #4871: an unparseable `indexes.toml` must be an ERROR, and the write
/// that follows must not run — the file is left byte-for-byte intact.
///
/// Why: this is the fail-open at the centre of both incidents. The loader mapped
/// a TOML parse failure to `Ok(vec![])`, so an unreadable registry read back as
/// "no index was ever registered". `upsert_index_registry_entry_at` then went
/// load(empty) → push one → save, publishing a 1-entry file over a real N-entry
/// registry and reporting success to its caller. That is the mass-deregistration
/// signature both issues recorded (73 → 31; 42 → 5 across six restarts).
/// What: writes a corrupt registry, asserts the load errors, drives an upsert,
/// asserts it errors and that the on-disk bytes are unchanged. Pre-fix the load
/// returns `Ok([])`, the upsert returns `Ok(())`, and the file is replaced by a
/// single `[[index]]` block — so all three assertions fail.
/// Test: this test.
#[test]
fn corrupt_registry_is_an_error_and_is_never_overwritten() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    let corrupt = "[[index]]\nid = \"real-one\"\nroot_pa";
    std::fs::write(&path, corrupt).expect("write corrupt registry");

    let loaded = load_index_registry_at(&path);
    assert!(
        loaded.is_err(),
        "#4317: a corrupt registry must NOT read back as an empty one — that view \
         is what every subsequent save deregisters the whole fleet from"
    );

    let upsert = upsert_index_registry_entry_at(&path, reg_entry("newcomer"));
    assert!(
        upsert.is_err(),
        "#4871: a write on top of an unreadable registry must fail loudly, not \
         report success while discarding every real entry"
    );

    let after = std::fs::read_to_string(&path).expect("registry still readable");
    assert_eq!(
        after, corrupt,
        "#4317: the corrupt file must be left intact for recovery, not replaced"
    );
}

/// #4871: concurrent registrations must all survive — none may be discarded by
/// another writer's stale snapshot.
///
/// Why: every mutation is load → mutate → whole-file save and nothing ordered
/// them. A busy daemon runs many concurrently (`search_handler` spawns a
/// fire-and-forget `update_last_queried_unix` per query alongside the
/// registration handlers), so a write landing between another task's load and
/// its save was silently erased — the observed 80 → 88 → byte-identical-80
/// revert where 8 real registrations vanished and every caller saw success.
/// What: 16 threads each upsert a distinct id into one registry file, then
/// assert all 16 are present. Pre-fix this drops entries nondeterministically.
/// Test: this test.
#[test]
fn concurrent_upserts_lose_no_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    save_index_registry_at(&path, &[]).expect("seed empty registry");

    const WRITERS: usize = 16;
    std::thread::scope(|scope| {
        for n in 0..WRITERS {
            let path = path.clone();
            scope.spawn(move || {
                upsert_index_registry_entry_at(&path, reg_entry(&format!("idx-{n}")))
                    .expect("upsert must succeed");
            });
        }
    });

    let entries = load_index_registry_at(&path).expect("registry must still parse");
    assert_eq!(
        entries.len(),
        WRITERS,
        "#4871: every concurrent registration must survive; lost {} of {WRITERS}. \
         A discarded write is indistinguishable from a successful one to its caller",
        WRITERS - entries.len(),
    );
    for n in 0..WRITERS {
        let want = format!("idx-{n}");
        assert!(
            entries.iter().any(|e| e.id == want),
            "#4871: '{want}' was written but is absent from the registry"
        );
    }
}

/// #4317: the boot reaper's own partition + removal must not erase an index
/// registered after its snapshot was taken.
///
/// Why (review MEDIUM): the previous version of this test called
/// `remove_index_registry_entries_at` directly, so at the parent commit it
/// failed only because that symbol did not exist — proving nothing about the
/// reaper. The defect is in `heal_boot_orphans`, which used to publish its
/// survivors with a whole-file `save_index_registry(&kept)` built from a
/// pre-boot snapshot: anything registered while it was deciding was erased,
/// while the reaper logged a clean self-heal.
/// What: drives the reaper's real partition (`partition_boot_orphans`, the
/// same call `heal_boot_orphans` makes) over a snapshot, registers a third
/// index out of band the way a concurrent `POST /indexes` would, then removes
/// the judged orphans by id — the sequence the fixed reaper performs. Asserts
/// the newcomer survives. Replaying `kept` instead, which is what the code did
/// before, drops it.
/// Test: this test.
#[test]
fn boot_orphan_reap_preserves_a_registration_made_after_its_snapshot() {
    use crate::service::orphan_reaper::partition_boot_orphans;

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    // A live root (this temp dir) and a reapable orphan (deleted child whose
    // parent still exists — `is_reapable_orphan`'s exact shape).
    let live = PersistedIndex {
        id: "keep-me".to_string(),
        root_path: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let orphan = PersistedIndex {
        id: "orphan".to_string(),
        root_path: tmp.path().join("deleted-worktree"),
        ..Default::default()
    };
    let snapshot = vec![live.clone(), orphan.clone()];
    save_index_registry_at(&path, &snapshot).expect("seed registry");

    // The reaper's decision, taken over the snapshot it read at boot.
    let (orphans, kept) = partition_boot_orphans(snapshot);
    assert_eq!(
        orphans.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        vec!["orphan"],
        "precondition: exactly the deleted root is judged orphaned"
    );
    assert_eq!(kept.len(), 1, "precondition: one survivor in the snapshot");

    // A registration lands after that snapshot but before the reaper writes.
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "registered-mid-sweep".to_string(),
            root_path: tmp.path().to_path_buf(),
            ..Default::default()
        },
    )
    .expect("concurrent registration");

    // The OLD reaper republished its snapshot's survivors wholesale. Run that
    // against an identical copy to show the defect directly, rather than
    // relying on which commit this test happens to run at — the by-id fix is
    // already an ancestor of this one, so a bare pre-fix run would pass.
    let old_way = tmp.path().join("old-way.toml");
    std::fs::copy(&path, &old_way).expect("copy registry");
    save_index_registry_at(&old_way, &kept).expect("replay the survivors snapshot");
    let old_ids: Vec<String> = load_index_registry_at(&old_way)
        .expect("load")
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(
        !old_ids.iter().any(|id| id == "registered-mid-sweep"),
        "#4317 precondition: republishing the pre-boot snapshot is what ERASED \
         the concurrent registration. If this no longer holds, the assertion \
         below proves nothing. Got {old_ids:?}"
    );

    // What the fixed reaper does: remove the judged orphans BY ID.
    let orphan_ids: Vec<&str> = orphans.iter().map(|o| o.id.as_str()).collect();
    remove_index_registry_entries_at(&path, &orphan_ids).expect("reap");

    let entries = load_index_registry_at(&path).expect("load");
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"registered-mid-sweep"),
        "#4317: an index registered during the sweep must not be collateral. \
         Republishing the reaper's `kept` snapshot ({} entries) instead of \
         removing by id is what erased it. Got {ids:?}",
        kept.len(),
    );
    assert!(
        ids.contains(&"keep-me"),
        "healthy entry must survive: {ids:?}"
    );
    assert!(
        !ids.contains(&"orphan"),
        "the judged orphan must actually be removed: {ids:?}"
    );
}

/// #4317: removing ids that are absent is a no-op, and the file is untouched.
///
/// Why: the reaper calls this on every boot; an idempotent delete keeps a
/// repeated sweep from churning the file (and from being a second write window).
/// What: removes two unknown ids from a two-entry registry, asserts both entries
/// remain.
/// Test: this test.
#[test]
fn remove_entries_by_id_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    save_index_registry_at(&path, &[reg_entry("a"), reg_entry("b")]).expect("seed");

    remove_index_registry_entries_at(&path, &["nope", "also-nope"]).expect("no-op remove");
    remove_index_registry_entries_at(&path, &[]).expect("empty remove");

    let entries = load_index_registry_at(&path).expect("load");
    assert_eq!(entries.len(), 2, "an unknown id must remove nothing");
}

/// #4317 review LOW: an abandoned staging file is reaped once it ages out, and
/// a fresh one is never touched.
///
/// Why: per-write staging names fixed the shared-tmp collision but removed the
/// accidental cleanup a single fixed name gave — a crash between `write` and
/// `rename` now leaks a uniquely-named file nothing reclaims. The age window is
/// what keeps the sweep from deleting a concurrent writer's live staging file.
/// What: plants one stale and one fresh `indexes.toml.tmp.*`, performs a normal
/// save, and asserts only the stale one is gone.
/// Test: this test.
#[test]
fn stale_staging_files_are_reaped_fresh_ones_are_not() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    save_index_registry_at(&path, &[reg_entry("live")]).expect("seed");

    let stale = tmp.path().join("indexes.toml.tmp.999.0");
    let fresh = tmp.path().join("indexes.toml.tmp.999.1");
    let unrelated = tmp.path().join("indexes.toml.bak-something");
    std::fs::write(&stale, "junk").expect("write stale");
    std::fs::write(&fresh, "junk").expect("write fresh");
    std::fs::write(&unrelated, "junk").expect("write unrelated");
    // Age the stale one past the window.
    let old =
        std::time::SystemTime::now() - STAGING_STALE_AFTER - std::time::Duration::from_secs(60);
    filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old))
        .expect("backdate stale staging file");

    save_index_registry_at(&path, &[reg_entry("live"), reg_entry("second")]).expect("save");

    assert!(
        !stale.exists(),
        "#4317: an aged-out staging file must be reaped"
    );
    assert!(
        fresh.exists(),
        "#4317: a fresh staging file may belong to a concurrent writer — never reap it"
    );
    assert!(
        unrelated.exists(),
        "#4317: only `indexes.toml.tmp.*` is in scope; a backup must survive"
    );
    let entries = load_index_registry_at(&path).expect("registry still parses");
    assert_eq!(entries.len(), 2, "the save itself must still have worked");
}

/// #4871 review MEDIUM: `patch_index_registry_entry_at` does its find-and-patch
/// inside the write lock, so a concurrent write to a DIFFERENT field of the
/// same entry is not clobbered.
///
/// Why: the LRU timestamp writers used to load, find their entry, mutate a
/// clone, and only then upsert — and the upsert re-loaded under the lock. The
/// find happened outside the critical section, so `update_last_queried_unix`
/// and `update_last_indexed_unix` racing on one entry could each persist a copy
/// that had never seen the other's field.
/// What: hammers one entry from two thread pools, one setting
/// `last_queried_unix` and the other `last_indexed_unix`, then asserts BOTH
/// fields survived. A load-clone-upsert implementation drops one of them.
/// Test: this test.
#[test]
fn patch_entry_is_atomic_against_a_concurrent_field_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    save_index_registry_at(&path, &[reg_entry("hot")]).expect("seed");

    const ROUNDS: u64 = 40;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 1..=ROUNDS {
                patch_index_registry_entry_at(&path, "hot", |e| e.last_queried_unix = Some(n))
                    .expect("queried patch");
            }
        });
        scope.spawn(|| {
            for n in 1..=ROUNDS {
                patch_index_registry_entry_at(&path, "hot", |e| e.last_indexed_unix = Some(n))
                    .expect("indexed patch");
            }
        });
    });

    let entries = load_index_registry_at(&path).expect("registry must still parse");
    assert_eq!(entries.len(), 1, "no entry may be lost: {entries:?}");
    let hot = &entries[0];
    assert_eq!(
        hot.last_queried_unix,
        Some(ROUNDS),
        "#4871: the last `last_queried_unix` write must survive — a stale clone \
         from outside the lock is what overwrote it"
    );
    assert_eq!(
        hot.last_indexed_unix,
        Some(ROUNDS),
        "#4871: and the concurrent `last_indexed_unix` write must survive too; \
         losing either field is the clobber this patch helper exists to prevent"
    );
}

/// A patch for an id that is not in the registry is a silent no-op.
///
/// Why: the timestamp writers fire after a query, and the index may have been
/// deleted in between — that must not error or create a rootless entry.
/// What: patches a missing id, asserts `Ok` and an unchanged file.
/// Test: this test.
#[test]
fn patch_entry_for_a_missing_id_is_a_no_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    save_index_registry_at(&path, &[reg_entry("present")]).expect("seed");
    let before = std::fs::read_to_string(&path).expect("read");

    patch_index_registry_entry_at(&path, "absent", |e| e.last_queried_unix = Some(7))
        .expect("a missing id must not be an error");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        before,
        "a no-op patch must not rewrite the file"
    );
}

//! #9274 palace-format gate tests.
//!
//! Why: the gate has to refuse a newer palace without changing a byte, and it
//! has to leave every existing (unstamped, format 0) palace opening exactly as
//! before. Both halves are proven here against real redb files.
//! What: fixtures are stamped through a raw `Database`, bypassing the gate.
//! A raised supported format is injected through `classify_with`. The 0 → 1
//! migration itself is tested in format_migration_tests.rs.
//! Test: this file.

use super::*;
use crate::engine::palace::{Drawer, Palace, PalaceId};
use crate::engine::retrieval::PalaceHandle;
use crate::engine::store::concurrent_open::OpenIntent;
use crate::engine::store::kg_redb::KgStoreRedb;
use crate::engine::store::kg_store::encode_value;
use crate::engine::store::palace_store::PalaceStore;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::tempdir;

/// Save a palace under `root/<id>` and return it.
fn saved_palace(root: &Path, id: &str) -> Palace {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(id),
    };
    PalaceStore::save_palace(&palace).expect("save palace");
    palace
}

/// Open and close a palace once, so every store file exists on disk.
fn populate(palace: &Palace) {
    drop(PalaceHandle::open_with_intent(palace, OpenIntent::Writer).expect("populate palace"));
}

/// Write raw bytes as the `palace_format` row, bypassing every gate.
fn stamp_marker_bytes(kg_path: &Path, bytes: &[u8]) {
    let db = trusty_common::redb_cache::create_palace_db(kg_path).expect("open kg.redb to stamp");
    let wtx = db.begin_write().expect("begin stamp txn");
    {
        let mut t = wtx.open_table(KG_SCHEMA).expect("open kg_schema");
        t.insert(KG_SCHEMA_PALACE_FORMAT, bytes)
            .expect("insert marker");
    }
    wtx.commit().expect("commit stamp");
}

/// Stamp the authoritative marker in `<dir>/kg.redb`.
fn stamp_marker(dir: &Path, format: u32) {
    let bytes = encode_value(&KgSchemaMarker {
        schema_version: format,
    })
    .expect("encode marker");
    stamp_marker_bytes(&dir.join(KG_FILE), &bytes);
}

/// Set the `format_version` mirror in `<dir>/palace.json`.
fn stamp_mirror(dir: &Path, format: u32) {
    let path = dir.join(PALACE_JSON);
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read palace.json")).expect("json");
    v["format_version"] = serde_json::json!(format);
    std::fs::write(&path, serde_json::to_vec_pretty(&v).expect("encode")).expect("write");
}

/// SHA-256 of every file under `dir`, keyed by relative path.
fn hash_tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let digest = Sha256::digest(std::fs::read(&path).expect("read file"));
                let rel = path.strip_prefix(dir).expect("relative").to_path_buf();
                out.insert(rel, digest.to_vec());
            }
        }
    }
    out
}

/// The `PalaceStoreError` inside an open error, or a panic naming the error.
fn store_error(err: &anyhow::Error) -> &PalaceStoreError {
    err.downcast_ref::<PalaceStoreError>()
        .unwrap_or_else(|| panic!("expected a PalaceStoreError, got: {err:#}"))
}

/// ADR-0067 D3 rule 1: a newer marker refuses every intent, and no byte under
/// the palace directory changes — no init transaction, no vector-store write.
#[test]
fn n_plus_one_palace_is_refused_and_bytes_unchanged() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "newer");
    populate(&palace);
    stamp_marker(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let before = hash_tree(&palace.data_dir);

    for intent in [OpenIntent::Writer, OpenIntent::ReadOnlyClient] {
        let err = PalaceHandle::open_with_intent_purging(&palace, intent, true)
            .err()
            .unwrap_or_else(|| panic!("{intent:?}: a newer palace must be refused"));
        assert!(
            matches!(
                store_error(&err),
                PalaceStoreError::FormatTooNew { found, supported, .. }
                    if *found == PALACE_FORMAT_SUPPORTED + 1
                        && *supported == PALACE_FORMAT_SUPPORTED
            ),
            "{intent:?}: {err:#}"
        );
        assert_eq!(
            before,
            hash_tree(&palace.data_dir),
            "{intent:?} changed a byte"
        );
    }
}

/// The mirror alone refuses the palace before any redb file is created; on
/// main `index.usearch.redb` was written first.
#[test]
fn n_plus_one_in_palace_json_mirror_is_refused_before_redb_opens() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "mirror-only");
    stamp_mirror(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let before = hash_tree(&palace.data_dir);

    let err = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer)
        .err()
        .expect("a newer mirror must refuse the open");
    assert!(
        matches!(
            store_error(&err),
            PalaceStoreError::FormatTooNew { found, .. } if *found == PALACE_FORMAT_SUPPORTED + 1
        ),
        "{err:#}"
    );
    assert_eq!(
        before,
        hash_tree(&palace.data_dir),
        "a file was created or changed"
    );
}

/// #9274: a new palace is born at the supported format — the KG marker in its
/// init transaction, the mirror at the writer open — and reopens under both
/// intents with its data.
#[test]
fn a_new_palace_is_stamped_at_the_supported_format() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "fresh");
    let drawer = Drawer::new(uuid::Uuid::new_v4(), "a new palace keeps this drawer");
    {
        let h = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer).expect("rw open");
        h.kg.upsert_drawer_sync(&drawer).expect("write a drawer");
    }
    for intent in [OpenIntent::ReadOnlyClient, OpenIntent::Writer] {
        let h = PalaceHandle::open_with_intent(&palace, intent)
            .unwrap_or_else(|e| panic!("{intent:?}: a new palace must reopen: {e:#}"));
        assert!(
            h.drawers.read().iter().any(|d| d.id == drawer.id),
            "{intent:?}: the drawer written before must be loaded"
        );
    }
    assert_eq!(
        read_mirror(&palace.data_dir).expect("mirror"),
        Some(PALACE_FORMAT_SUPPORTED)
    );
    assert_eq!(
        read_marker_without_writing(&palace.data_dir.join(KG_FILE)).expect("marker"),
        KgMarker::Stamped(PALACE_FORMAT_SUPPORTED)
    );
    assert!(!palace.data_dir.join(MIGRATION_RECORD).exists());
}

/// ADR-0067 D1: an older binary dropped the mirror, so it is below the
/// marker. The marker wins; a read-only open writes nothing and a writer
/// open rewrites the mirror, keeping fields this binary does not know.
#[test]
fn a_lower_mirror_is_repaired_by_a_writer_and_the_marker_wins() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "lower-mirror");
    populate(&palace);
    let path = palace.data_dir.join(PALACE_JSON);
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    let obj = v.as_object_mut().expect("object");
    obj.remove("format_version");
    obj.insert("added_by_a_later_1x".into(), serde_json::json!("kept"));
    std::fs::write(&path, serde_json::to_vec_pretty(&v).expect("encode")).expect("write");
    let before = std::fs::read(&path).expect("read");

    drop(PalaceHandle::open_with_intent(&palace, OpenIntent::ReadOnlyClient).expect("ro open"));
    assert_eq!(
        before,
        std::fs::read(&path).expect("reread"),
        "read-only wrote"
    );

    drop(PalaceHandle::open_with_intent(&palace, OpenIntent::Writer).expect("rw open"));
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    assert_eq!(
        after["format_version"],
        serde_json::json!(PALACE_FORMAT_SUPPORTED)
    );
    assert_eq!(after["added_by_a_later_1x"], serde_json::json!("kept"));
}

/// ADR-0067 D3 rule 2 with an injected supported format of 2: a palace stamped
/// 1 is refused read-only, and the error tells the caller to start the daemon.
#[test]
fn read_only_open_of_n_minus_1_is_refused_with_migration_hint() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "behind");
    populate(&palace);
    stamp_marker(&palace.data_dir, 1);
    stamp_mirror(&palace.data_dir, 1);

    let err = gate_read_only_with(&palace.data_dir, "behind", 2).expect_err("N-1 is refused");
    assert!(
        matches!(
            err,
            PalaceStoreError::FormatNeedsMigration {
                found: 1,
                supported: 2,
                ..
            }
        ),
        "{err}"
    );
    // #9918: the reworded hint; full text in `behind_format_refusals_tell_the_operator_what_to_do`.
    assert!(
        err.to_string()
            .contains("restart the daemon that owns the palace"),
        "{err}"
    );
}

/// Supervisor ruling Q2: a marker and mirror that disagree refuse the palace.
#[test]
fn markers_that_disagree_refuse_the_palace() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "split");
    populate(&palace);
    stamp_marker(&palace.data_dir, 1);
    stamp_mirror(&palace.data_dir, 2);

    let err = classify_with(&palace.data_dir, "split", 2).expect_err("disagreement");
    assert!(
        matches!(
            err,
            PalaceStoreError::FormatMarkerMismatch {
                marker: 1,
                mirror: 2,
                ..
            }
        ),
        "{err}"
    );
}

/// Fail closed: a marker row that does not decode is not format 0.
#[test]
fn an_undecodable_marker_refuses_the_palace() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "garbled");
    populate(&palace);
    stamp_marker_bytes(&palace.data_dir.join(KG_FILE), &[0xFF; 8]);

    let err = gate_palace(&palace.data_dir, "garbled").expect_err("garbled marker");
    assert!(
        matches!(err, PalaceStoreError::FormatMarkerUnreadable { .. }),
        "{err}"
    );
    let err = KgStoreRedb::open_with_intent(&palace.data_dir.join(KG_FILE), OpenIntent::Writer)
        .err()
        .expect("the KG open refuses it too");
    assert!(is_format_refusal(&err), "{err:#}");
}

/// `kg_rebuild` / `kg_twin_merge` open the KG without a palace handle; the KG
/// gate refuses a newer file under both intents with its bytes unchanged.
#[test]
fn a_direct_kg_open_of_an_n_plus_one_file_is_refused() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "direct");
    populate(&palace);
    stamp_marker(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let kg = palace.data_dir.join(KG_FILE);
    let before = std::fs::read(&kg).expect("read kg");

    for intent in [OpenIntent::Writer, OpenIntent::ReadOnlyClient] {
        let err = KgStoreRedb::open_with_intent(&kg, intent)
            .err()
            .unwrap_or_else(|| panic!("{intent:?}: a newer KG must be refused"));
        assert!(
            matches!(store_error(&err), PalaceStoreError::FormatTooNew { .. }),
            "{intent:?}: {err:#}"
        );
    }
    assert_eq!(
        before,
        std::fs::read(&kg).expect("reread kg"),
        "kg.redb changed"
    );
}

/// A held file cannot be read without writing, so the marker is read after
/// the open (here a snapshot) and still refuses the palace.
#[test]
fn a_held_kg_is_checked_after_open_and_refused() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "held");
    populate(&palace);
    stamp_marker(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let kg = palace.data_dir.join(KG_FILE);
    let _holder = trusty_common::redb_cache::create_palace_db(&kg).expect("hold kg.redb");

    assert_eq!(
        read_marker_without_writing(&kg).expect("read"),
        KgMarker::Deferred,
        "anti-vacuous: a held file must defer to the post-open check"
    );
    let err = KgStoreRedb::open_with_intent(&kg, OpenIntent::ReadOnlyClient)
        .err()
        .expect("the post-open check refuses the snapshot");
    assert!(
        matches!(store_error(&err), PalaceStoreError::FormatTooNew { .. }),
        "{err:#}"
    );
}

/// The open retry loops stop on a format refusal through context layers, and
/// only on one.
#[test]
fn format_errors_are_not_retried() {
    let p = || "p".to_string();
    let refusals = [
        PalaceStoreError::FormatTooNew {
            palace: p(),
            found: 2,
            supported: 1,
        },
        PalaceStoreError::FormatNeedsMigration {
            palace: p(),
            found: 0,
            supported: 1,
        },
        PalaceStoreError::FormatTooOld {
            palace: p(),
            found: 0,
            supported: 2,
        },
        PalaceStoreError::FormatMarkerMismatch {
            palace: p(),
            marker: 1,
            mirror: 0,
        },
        PalaceStoreError::FormatMarkerUnreadable {
            palace: p(),
            path: PathBuf::from("kg.redb"),
            reason: "x".into(),
        },
        PalaceStoreError::FormatMigrationFailed {
            palace: p(),
            from: 0,
            to: 1,
            reason: "x".into(),
        },
    ];
    for e in refusals {
        let wrapped = anyhow::Error::from(e)
            .context("open KG")
            .context("open palace");
        assert!(is_format_refusal(&wrapped), "{wrapped:#}");
    }
    let transient = anyhow::Error::from(PalaceStoreError::BackupFailed {
        path: PathBuf::from("b"),
        source: std::io::Error::other("busy"),
    });
    assert!(!is_format_refusal(&transient));
    assert!(!is_format_refusal(&anyhow::anyhow!(
        "database already open"
    )));
}

/// #9278: a refusal behind a `KgError::Palace` is still a refusal, and only a
/// refusal; `chain()` alone never reaches the transparent payload.
#[test]
fn format_refusal_is_seen_through_kg_error() {
    use crate::engine::store::kg::KgError;
    let too_new = PalaceStoreError::FormatTooNew {
        palace: "p".into(),
        found: 2,
        supported: 1,
    };
    let wrapped = anyhow::Error::from(KgError::Palace(too_new)).context("open");
    assert!(is_format_refusal(&wrapped), "{wrapped:#}");
    let absent = PalaceStoreError::NotFound(PathBuf::from("p"));
    assert!(!is_format_refusal(&anyhow::Error::from(KgError::Palace(
        absent
    ))));
    let other = KgError::from_internal(anyhow::anyhow!("boom"));
    assert!(!is_format_refusal(&anyhow::Error::from(other)));
}

/// Every distance from the supported format lands on its own arm.
#[test]
fn check_format_classifies_every_distance() {
    let name = |found| check_format("p", found, 2).err().map(|e| e.variant_name());
    assert_eq!(name(3), Some("FormatTooNew"));
    assert_eq!(name(2), None);
    assert_eq!(name(1), Some("FormatNeedsMigration"));
    // #9918: format 0 is migrated by the chain, not refused too old.
    assert_eq!(name(0), Some("FormatNeedsMigration"));
    let old = check_format_with("p", 0, 2, 1).expect_err("below the oldest");
    assert_eq!(old.variant_name(), "FormatTooOld", "{old}");
}

/// #9918: each behind-format refusal names what the operator does next.
#[test]
fn behind_format_refusals_tell_the_operator_what_to_do() {
    let needs = check_format("p", 0, 2).expect_err("behind");
    assert_eq!(
        needs.to_string(),
        "FormatNeedsMigration: palace p is at format 0, older than the format 2 this binary \
         reads; restart the daemon that owns the palace (the trusty-memory daemon, or `tm daemon \
         run` for the Session Manager and portfolio palaces), which migrates it to format 2 \
         behind a verified backup, then retry; if that daemon is already running, the migration \
         failed: run `trusty-memory doctor` for the reason"
    );
    let old = check_format_with("p", 0, 2, 1).expect_err("below the oldest");
    assert_eq!(
        old.to_string(),
        "FormatTooOld: palace p is at format 0, which this binary cannot migrate to format 2; \
         install a trusty-memory release that migrates format 0, start its daemon once so it \
         migrates the palace, then retry with this release"
    );
}

/// Every new arm reports its variant name first, so the MCP message names it.
#[test]
fn every_new_error_arm_names_its_variant() {
    let p = || "p".to_string();
    let io = || std::io::Error::other("x");
    let arms = [
        PalaceStoreError::FormatTooNew {
            palace: p(),
            found: 1,
            supported: 0,
        },
        PalaceStoreError::FormatNeedsMigration {
            palace: p(),
            found: 0,
            supported: 1,
        },
        PalaceStoreError::FormatTooOld {
            palace: p(),
            found: 0,
            supported: 2,
        },
        PalaceStoreError::FormatMarkerMismatch {
            palace: p(),
            marker: 1,
            mirror: 0,
        },
        PalaceStoreError::FormatMarkerUnreadable {
            palace: p(),
            path: PathBuf::from("k"),
            reason: "r".into(),
        },
        PalaceStoreError::FormatMigrationFailed {
            palace: p(),
            from: 0,
            to: 1,
            reason: "r".into(),
        },
        PalaceStoreError::BackupFailed {
            path: PathBuf::from("b"),
            source: io(),
        },
        PalaceStoreError::BackupVerifyMismatch {
            file: PathBuf::from("f"),
            expected: "a".into(),
            actual: "b".into(),
        },
        PalaceStoreError::InsufficientSpace {
            path: PathBuf::from("s"),
            needed: 2,
            available: 1,
        },
    ];
    for arm in arms {
        let shown = arm.to_string();
        assert!(shown.starts_with(arm.variant_name()), "{shown}");
        assert_eq!(
            arm.is_format_refusal(),
            arm.variant_name().starts_with("Format")
        );
    }
}

/// `save_palace` keeps a mirror it finds and refuses to rewrite a newer one.
#[test]
fn save_palace_keeps_the_format_mirror_and_refuses_a_newer_one() {
    let root = tempdir().expect("tempdir");
    let mut palace = saved_palace(root.path(), "renamed");
    stamp_mirror(&palace.data_dir, PALACE_FORMAT_SUPPORTED);
    palace.name = "renamed again".into();
    PalaceStore::save_palace(&palace).expect("save keeps the mirror");
    assert_eq!(
        read_mirror(&palace.data_dir).expect("read"),
        Some(PALACE_FORMAT_SUPPORTED)
    );

    stamp_mirror(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let before = std::fs::read(palace.data_dir.join(PALACE_JSON)).expect("read");
    let err = PalaceStore::save_palace(&palace).expect_err("a newer palace is not rewritten");
    assert!(
        matches!(err, PalaceStoreError::FormatTooNew { .. }),
        "{err}"
    );
    assert_eq!(
        before,
        std::fs::read(palace.data_dir.join(PALACE_JSON)).expect("reread")
    );
}

/// #9274: the chat-session store is a D2 primary file under the palace
/// directory, opened by the chat_session_* tools without a palace handle. A
/// newer palace refuses it before redb opens or writes, and the refusal is
/// still a `PalaceStoreError` through `is_format_refusal`.
#[test]
fn chat_session_store_refuses_a_newer_palace_with_bytes_unchanged() {
    use crate::engine::store::chat_sessions::ChatSessionStore;

    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "chat-newer");
    populate(&palace);
    let chat = palace.data_dir.join("chat_sessions.db");
    // A current palace opens its chat store as before.
    let store = ChatSessionStore::open(&chat).expect("current chat store opens");
    store
        .create_session(Some("kept".into()))
        .expect("seed a session");
    drop(store);

    stamp_marker(&palace.data_dir, PALACE_FORMAT_SUPPORTED + 1);
    let before = hash_tree(&palace.data_dir);
    assert!(
        before.contains_key(Path::new("chat_sessions.redb")),
        "anti-vacuous: the chat store file exists before the refused open"
    );

    let err = ChatSessionStore::open(&chat)
        .err()
        .expect("a newer palace must refuse the chat store");
    assert!(
        matches!(
            store_error(&err),
            PalaceStoreError::FormatTooNew { found, supported, .. }
                if *found == PALACE_FORMAT_SUPPORTED + 1
                    && *supported == PALACE_FORMAT_SUPPORTED
        ),
        "{err:#}"
    );
    assert!(is_format_refusal(&err), "{err:#}");
    assert_eq!(
        before,
        hash_tree(&palace.data_dir),
        "the refused chat open changed a byte or a file under the palace"
    );
}

/// PR2 critic MEDIUM (#9274): the marker is read again on the opened file. A
/// test seam stamps a newer marker between the pre-open read (which accepted
/// the file) and the open; the open must still refuse it before writing.
#[test]
fn the_post_open_gate_reads_the_opened_file_not_the_pre_open_marker() {
    use crate::engine::store::kg_redb::AFTER_PRE_OPEN;
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "post-open");
    populate(&palace);
    let kg = palace.data_dir.join(KG_FILE);
    assert_eq!(
        read_marker_without_writing(&kg).expect("read"),
        KgMarker::Stamped(PALACE_FORMAT_SUPPORTED),
        "anti-vacuous: the pre-open read accepts the file"
    );

    AFTER_PRE_OPEN.with(|h| h.set(Some(|p| stamp_marker(p.parent().expect("dir"), 9))));
    let result = KgStoreRedb::open_with_intent(&kg, OpenIntent::Writer);
    AFTER_PRE_OPEN.with(|h| h.set(None));
    let err = result
        .err()
        .expect("the opened file is newer and must be refused");
    assert!(
        matches!(
            store_error(&err),
            PalaceStoreError::FormatTooNew { found: 9, .. }
        ),
        "{err:#}"
    );
}

/// #9274, re-pointed by #9275: `doctor` reads a palace's format state without
/// writing; a format-1 palace is "needs migration", and an interrupted one
/// says so.
#[test]
fn status_reports_needs_migration_for_a_format_1_palace() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "status");
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::Current,
        "a palace with no KG yet is new, not behind"
    );
    populate(&palace);
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::Current
    );
    // Make it format 1: both markers at 1.
    stamp_marker(&palace.data_dir, 1);
    stamp_mirror(&palace.data_dir, 1);
    let before = hash_tree(&palace.data_dir);
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::NeedsMigration {
            found: 1,
            interrupted: false
        }
    );
    std::fs::write(palace.data_dir.join(MIGRATION_RECORD), b"{}").expect("record");
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::NeedsMigration {
            found: 1,
            interrupted: true
        }
    );
    std::fs::remove_file(palace.data_dir.join(MIGRATION_RECORD)).expect("rm");
    assert_eq!(before, hash_tree(&palace.data_dir), "status wrote a byte");
}

/// #9274 (PR3 critic MEDIUM): a `kg.redb` that cannot be read without
/// writing — here held, as a running daemon holds it — is `Unverified`, not
/// `Current`, and the read writes nothing.
#[test]
fn status_reports_a_held_kg_as_unverified() {
    let root = tempdir().expect("tempdir");
    let palace = saved_palace(root.path(), "held-status");
    populate(&palace);
    let kg = palace.data_dir.join(KG_FILE);
    let holder = trusty_common::redb_cache::create_palace_db(&kg).expect("hold kg.redb");
    let before = hash_tree(&palace.data_dir);
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::Unverified
    );
    assert_eq!(before, hash_tree(&palace.data_dir), "status wrote a byte");
    drop(holder);
    assert_eq!(
        palace_format_status(&palace.data_dir).expect("status"),
        PalaceFormatStatus::Current
    );
}

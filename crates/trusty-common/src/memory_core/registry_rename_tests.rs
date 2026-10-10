//! Tests for `PalaceRegistry::rename_palace` (#9544).
//!
//! Why: the rename moves a palace directory and rewrites the alias map, so
//! every refusal must leave both untouched and every partial run must either
//! roll back or be finishable by a re-run.
//! What: one test per refusal class, the happy path, `replace_empty`, both
//! resume points, the rolled-back move, and the reversed rename.
//! Test: this file.

use super::*;
use crate::memory_core::palace::{Drawer, Palace};
use crate::memory_core::store::chat_sessions::ChatSessionStore;
use std::collections::BTreeMap;
use tempfile::tempdir;

/// A short wait so `Busy` refusals answer quickly.
fn opts() -> RenameOptions<'static> {
    RenameOptions {
        busy_wait: Duration::from_millis(200),
        ..RenameOptions::default()
    }
}

/// Create palace `id` under `root` (its handle is dropped, so it is idle).
fn create(reg: &PalaceRegistry, root: &Path, id: &str) -> Arc<crate::memory_core::PalaceHandle> {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(id),
    };
    reg.create_palace(root, palace).expect("create palace")
}

fn json_id(root: &Path, dir: &str) -> (String, String) {
    let p = PalaceStore::load_palace(&root.join(dir)).expect("load palace.json");
    (p.id.0, p.name)
}

fn aliases(root: &Path) -> BTreeMap<String, String> {
    PalaceAliasStore::load_aliases(root).expect("load aliases")
}

/// Why: the happy path — directory moved, id and name rewritten, the old id
/// and every alias of it reach the renamed palace.
/// Test: itself.
#[test]
fn rename_palace_moves_dir_and_rewrites_palace_json() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    PalaceAliasStore::register_alias(root, "older", "old-p").unwrap();

    let out = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .expect("rename");
    assert_eq!((out.old.as_str(), out.new.as_str()), ("old-p", "new-p"));
    assert!(!out.resumed && out.name_rewritten && out.trashed_target.is_none());
    assert!(!root.join("old-p").exists());
    assert_eq!(json_id(root, "new-p"), ("new-p".into(), "new-p".into()));
    let map = aliases(root);
    assert_eq!(map.get("old-p").map(String::as_str), Some("new-p"));
    assert_eq!(map.get("older").map(String::as_str), Some("new-p"));
    for id in ["old-p", "older", "new-p"] {
        let h = reg.open_palace(root, &PalaceId::new(id)).expect("open");
        assert_eq!(h.id.as_str(), "new-p", "{id} reaches the renamed palace");
    }
}

/// Why: a target with drawers must never be replaced, even with
/// `replace_empty`, and the refusal changes nothing.
/// Test: itself.
#[test]
fn rename_palace_refuses_a_nonempty_target() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    let target = create(&reg, root, "new-p");
    target
        .kg
        .upsert_drawer_sync(&Drawer::new(uuid::Uuid::new_v4(), "kept"))
        .unwrap();
    drop(target);

    let replace = RenameOptions {
        replace_empty: true,
        ..opts()
    };
    let err = reg
        .rename_palace(root, "old-p", "new-p", &replace)
        .unwrap_err();
    assert!(
        matches!(
            &err,
            PalaceRenameError::TargetNotEmpty {
                reason: PalaceNotEmpty::Drawers(1),
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.is_conflict());
    assert_eq!(json_id(root, "old-p").0, "old-p");
    assert_eq!(json_id(root, "new-p").0, "new-p");
    assert!(aliases(root).is_empty());
}

/// Why (#9544): chat history alone makes a target non-empty.
/// Test: itself.
#[test]
fn rename_palace_refuses_a_target_with_chat_sessions() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    drop(create(&reg, root, "new-p"));
    let chat = ChatSessionStore::open(&root.join("new-p").join("chat_sessions.redb")).unwrap();
    chat.create_session(None).unwrap();
    drop(chat);

    let replace = RenameOptions {
        replace_empty: true,
        ..opts()
    };
    let err = reg
        .rename_palace(root, "old-p", "new-p", &replace)
        .unwrap_err();
    assert!(
        matches!(
            &err,
            PalaceRenameError::TargetNotEmpty {
                reason: PalaceNotEmpty::ChatSessions(1),
                ..
            }
        ),
        "{err:?}"
    );
    assert!(root.join("old-p").join("palace.json").exists());
}

/// Why: renaming an alias would move nothing it names; refuse and point at
/// the real palace.
/// Test: itself.
#[test]
fn rename_palace_refuses_an_alias_source() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "target-p"));
    PalaceAliasStore::register_alias(root, "alias-p", "target-p").unwrap();

    let err = reg
        .rename_palace(root, "alias-p", "new-p", &opts())
        .unwrap_err();
    assert!(
        matches!(&err, PalaceRenameError::SourceIsAlias { target, .. } if target == "target-p"),
        "{err:?}"
    );
    assert_eq!(json_id(root, "target-p").0, "target-p");
    assert!(!root.join("new-p").exists());
}

/// Why: a missing source is `NotFound`, distinct from a conflict.
/// Test: itself.
#[test]
fn rename_palace_refuses_a_missing_source() {
    let tmp = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    let err = reg
        .rename_palace(tmp.path(), "ghost", "new-p", &opts())
        .unwrap_err();
    assert!(
        matches!(&err, PalaceRenameError::NotFound(id) if id == "ghost"),
        "{err:?}"
    );
    assert!(!err.is_conflict());
    let bad = reg
        .rename_palace(tmp.path(), "ghost", "Bad_Id", &opts())
        .unwrap_err();
    assert!(
        matches!(bad, PalaceRenameError::InvalidTarget { .. }),
        "{bad:?}"
    );
}

/// Why: an empty target is refused without `replace_empty`, and with it is
/// moved to `<root>/.trash/<new>-replaced-<UTC>/`, never deleted.
/// Test: itself.
#[test]
fn rename_palace_replace_empty_trashes_the_target() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    drop(create(&reg, root, "new-p"));

    let err = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .unwrap_err();
    assert!(
        matches!(err, PalaceRenameError::TargetExists { .. }),
        "{err:?}"
    );
    assert!(aliases(root).is_empty(), "a refusal writes no alias");

    let replace = RenameOptions {
        replace_empty: true,
        ..opts()
    };
    let out = reg
        .rename_palace(root, "old-p", "new-p", &replace)
        .expect("rename");
    let trash = out.trashed_target.expect("target trashed");
    assert_eq!(trash.parent(), Some(root.join(".trash").as_path()));
    let name = trash.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("new-p-replaced-") && !name.contains("reclaim"),
        "{name}"
    );
    assert_eq!(
        PalaceStore::load_palace(&trash).unwrap().id.as_str(),
        "new-p",
        "the replaced palace sits in the trash intact"
    );
    assert_eq!(json_id(root, "new-p").0, "new-p");
    assert!(!root.join("old-p").exists());
}

/// Why: a handle another caller holds must not be popped; the rename waits
/// `busy_wait`, then refuses with nothing changed.
/// Test: itself.
#[test]
fn rename_palace_refuses_a_referenced_handle_and_leaves_state_unchanged() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    let held = create(&reg, root, "old-p");

    let err = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .unwrap_err();
    assert!(
        matches!(&err, PalaceRenameError::Busy { palace, .. } if palace == "old-p"),
        "{err:?}"
    );
    assert!(
        reg.peek(&PalaceId::new("old-p")).is_some(),
        "the held handle stays cached"
    );
    assert_eq!(json_id(root, "old-p").0, "old-p");
    assert!(!root.join("new-p").exists());
    assert!(aliases(root).is_empty());
    drop(held);
}

/// Why: a crash after the alias write and before the move must be finishable
/// by re-running the same rename.
/// Test: itself.
#[test]
fn rename_palace_resumes_after_alias_written_but_dir_unmoved() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    PalaceAliasStore::rename_target(root, "old-p", "new-p").unwrap();

    let out = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .expect("resume");
    assert!(out.resumed);
    assert!(!root.join("old-p").exists());
    assert_eq!(json_id(root, "new-p"), ("new-p".into(), "new-p".into()));
}

/// Why: a crash after the move and before the `palace.json` rewrite must be
/// finishable by re-running the same rename. #9544: in that state an open of
/// `new-p` caches its handle under the json id `old-p`; the resume must drop
/// it, or `old-p` serves a stale handle and `new-p` opens a second Writer.
/// Test: itself.
#[test]
fn rename_palace_resumes_after_dir_moved_but_id_unwritten() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new().with_writer_intent();
    drop(create(&reg, root, "old-p"));
    PalaceAliasStore::rename_target(root, "old-p", "new-p").unwrap();
    // Close the create-time handle (it points at the pre-move path).
    reg.remove(&PalaceId::new("old-p"));
    std::fs::rename(root.join("old-p"), root.join("new-p")).unwrap();
    assert_eq!(json_id(root, "new-p").0, "old-p");
    drop(
        reg.open_palace(root, &PalaceId::new("new-p"))
            .expect("open"),
    );
    assert!(
        reg.peek(&PalaceId::new("old-p")).is_some(),
        "cached under old"
    );

    let out = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .expect("resume");
    assert!(out.resumed && out.name_rewritten);
    assert_eq!(json_id(root, "new-p"), ("new-p".into(), "new-p".into()));
    assert!(
        reg.peek(&PalaceId::new("old-p")).is_none(),
        "stale handle gone"
    );
    let h = reg
        .open_palace(root, &PalaceId::new("new-p"))
        .expect("reopen");
    assert_eq!(h.id.as_str(), "new-p");
    assert!(!h.is_read_only(), "one Writer");
    assert_eq!(reg.len(), 1, "exactly one cached handle");
}

/// Why (#9544): `save_palace` refuses a newer-format palace, so a rename that
/// moved one first would stick half-done. The refusal must change nothing.
/// Test: itself.
#[test]
fn rename_palace_refuses_a_newer_format_source_and_changes_nothing() {
    use crate::memory_core::store::palace_format::PALACE_FORMAT_SUPPORTED;
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    reg.remove(&PalaceId::new("old-p"));
    let json = root.join("old-p").join("palace.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    v["format_version"] = serde_json::json!(PALACE_FORMAT_SUPPORTED + 1);
    std::fs::write(&json, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let before = std::fs::read(&json).unwrap();

    let err = reg
        .rename_palace(root, "old-p", "new-p", &opts())
        .unwrap_err();
    assert!(matches!(err, PalaceRenameError::Io { .. }), "{err:?}");
    assert!(format!("{err:#}").contains("FormatTooNew"), "{err}");
    assert!(aliases(root).is_empty(), "no alias written");
    assert!(!root.join("new-p").exists(), "dir not moved");
    assert_eq!(
        std::fs::read(&json).unwrap(),
        before,
        "palace.json unchanged"
    );
}

/// Why: a failed move must restore the replaced target and exactly the alias
/// keys the rename wrote.
/// Test: itself.
#[test]
fn rename_palace_failed_move_rolls_back_alias_keys() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "old-p"));
    drop(create(&reg, root, "new-p"));
    PalaceAliasStore::register_alias(root, "older", "old-p").unwrap();
    PalaceAliasStore::register_alias(root, "new-p", "elsewhere").unwrap();
    let before = aliases(root);
    let old_dir = root.join("old-p");
    let failing = |from: &Path, to: &Path| {
        if from == old_dir {
            Err(std::io::Error::other("injected move failure"))
        } else {
            std::fs::rename(from, to)
        }
    };
    let replace = RenameOptions {
        replace_empty: true,
        ..opts()
    };

    let err = reg
        .rename_palace_with(root, "old-p", "new-p", &replace, &failing)
        .unwrap_err();
    assert!(matches!(err, PalaceRenameError::Io { .. }), "{err:?}");
    assert!(err.to_string().contains("rolled back"), "{err}");
    assert_eq!(aliases(root), before, "every touched key restored");
    assert_eq!(json_id(root, "old-p").0, "old-p");
    assert_eq!(
        json_id(root, "new-p").0,
        "new-p",
        "the trashed target is restored"
    );
}

/// Why: renaming back (`new` is an alias of `old`) is allowed, and both names
/// resolve to one palace, so the rename must take that palace's open-lock
/// once — taking it twice waits on itself until `Busy`.
/// Test: itself.
#[test]
fn rename_palace_allows_the_reverse_rename() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let reg = PalaceRegistry::new();
    drop(create(&reg, root, "a-p"));
    reg.rename_palace(root, "a-p", "b-p", &opts())
        .expect("first rename");

    let out = reg
        .rename_palace(root, "b-p", "a-p", &opts())
        .expect("reverse rename");
    assert_eq!(out.new.as_str(), "a-p");
    assert_eq!(json_id(root, "a-p"), ("a-p".into(), "a-p".into()));
    assert!(!root.join("b-p").exists());
    let map = aliases(root);
    assert_eq!(map.get("b-p").map(String::as_str), Some("a-p"));
    assert_eq!(map.get("a-p"), None, "no alias is keyed by the live id");
}

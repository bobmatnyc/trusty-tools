//! Typed refusals of `palace restore` (#9282).
//!
//! Why: the PR 1 review requires a palace-id mismatch and a live daemon to be
//! refused with a typed error before anything is written; matching message
//! text in the binary tests cannot prove the type.
//! What: one fixture backs up a small on-disk palace with [`backup_offline`];
//! each test calls [`restore_into`] against a fresh data dir, downcasts the
//! error, and checks the data dir holds nothing.
//! Test: this file is the test.

use super::*;
use trusty_common::memory_core::palace::{Drawer, PalaceId, RoomType};
use trusty_common::memory_core::store::rooms::resolve_or_create_room_sync;
use trusty_common::memory_core::store::PalaceStore;

const ID: &str = "unit-bk";

/// A backup of a two-drawer palace, and a fresh, empty data dir.
struct Fixture {
    _tmp: tempfile::TempDir,
    snap: PathBuf,
    fresh: PathBuf,
}

fn fixture() -> Fixture {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let palace = Palace {
        id: PalaceId::new(ID),
        name: ID.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: tmp.path().join("src").join("palaces").join(ID),
    };
    PalaceStore::save_palace(&palace).expect("palace.json");
    {
        let handle = PalaceHandle::open(&palace).expect("open");
        let store = handle.kg.store();
        let room = resolve_or_create_room_sync(&store, &RoomType::Custom("unit".into()))
            .expect("room");
        for i in 0..2 {
            store
                .upsert_drawer(&Drawer::new(room, format!("unit drawer {i}")))
                .expect("drawer");
        }
    }
    let snap = tmp.path().join("snap");
    backup_offline(&palace, Some(&snap)).expect("backup");
    let fresh = tmp.path().join("fresh");
    std::fs::create_dir_all(&fresh).expect("fresh");
    Fixture {
        _tmp: tmp,
        snap,
        fresh,
    }
}

fn restore(fx: &Fixture, palace: Option<&str>, verdict: SocketVerdict) -> Result<RestoreReport> {
    let socket = fx.fresh.join("trusty-memory.sock");
    restore_into(&fx.fresh, &fx.snap, palace, &socket, verdict)
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).expect("ls").next().is_none()
}

/// Why (requirement 2): a backup of one palace restored under another id
/// hands that id's callers the wrong memories.
/// What: `--palace other` is `PalaceMismatch`, and the data dir stays empty;
/// the matching id then restores both drawers.
/// Test: itself.
#[test]
fn restore_into_refuses_a_palace_mismatch_before_writing() {
    let fx = fixture();
    let err = restore(&fx, Some("other"), SocketVerdict::NotServing).expect_err("mismatch");
    assert!(
        matches!(
            err.downcast_ref::<RestoreError>(),
            Some(RestoreError::PalaceMismatch { found, requested, .. })
                if found == ID && requested == "other"
        ),
        "{err:#}"
    );
    assert!(is_empty(&fx.fresh), "nothing may be written");

    let ok = restore(&fx, Some(ID), SocketVerdict::NotServing).expect("same id restores");
    assert_eq!(ok.target, fx.fresh.join(ID));
    assert_eq!(ok.manifest.drawer_count, 2);
}

/// Why: the manifest is not covered by its own checksums, so its palace id
/// is untrusted input that becomes a directory name.
/// What: a manifest naming `../escape` is `InvalidPalaceId`; nothing is
/// written inside or beside the data dir.
/// Test: itself.
#[test]
fn restore_into_refuses_an_invalid_manifest_palace_id() {
    let fx = fixture();
    let path = fx.snap.join(SNAPSHOT_MANIFEST);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
    manifest["palace"] = json!("../escape");
    std::fs::write(&path, serde_json::to_vec(&manifest).expect("json")).expect("write");

    let err = restore(&fx, None, SocketVerdict::NotServing).expect_err("invalid id");
    assert!(
        matches!(
            err.downcast_ref::<RestoreError>(),
            Some(RestoreError::InvalidPalaceId { .. })
        ),
        "{err:#}"
    );
    assert!(is_empty(&fx.fresh));
    assert!(!fx.fresh.parent().expect("parent").join("escape").exists());
}

/// Why (requirement 3): a daemon serving the root could load a half-written
/// palace; a probe that cannot decide must refuse too (fail closed).
/// What: `Serving` is `DaemonServing`, `Inconclusive` is `DaemonUnknown`, and
/// neither writes.
/// Test: itself.
#[test]
fn restore_into_refuses_a_serving_or_unknown_daemon() {
    let fx = fixture();
    let err = restore(&fx, None, SocketVerdict::Serving).expect_err("serving");
    assert!(
        matches!(
            err.downcast_ref::<RestoreError>(),
            Some(RestoreError::DaemonServing { .. })
        ),
        "{err:#}"
    );
    let err = restore(&fx, None, SocketVerdict::Inconclusive).expect_err("unknown");
    assert!(
        matches!(
            err.downcast_ref::<RestoreError>(),
            Some(RestoreError::DaemonUnknown { .. })
        ),
        "{err:#}"
    );
    assert!(is_empty(&fx.fresh));
}

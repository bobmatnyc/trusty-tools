//! Regression tests for the live L1 cache (#9719) and the L1 snapshot
//! gap-fill (#9722).
//!
//! Why: L1 was frozen at open (#9719), and a stale second handle's snapshot
//! could revive a forgotten drawer at the next open (#9722).
//! What: drives real on-disk palaces through `PalaceHandle::open`.
//! Test: this file IS the tests.

use super::*;
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

fn disk_palace(dir: &std::path::Path, id: &str) -> Palace {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("create palace dir");
    palace
}

async fn remember(handle: &Arc<PalaceHandle>, text: &str) -> Uuid {
    handle
        .remember(text.into(), RoomType::General, vec![], 0.9)
        .await
        .expect("remember")
}

fn l0_l1_ids(handle: &PalaceHandle) -> Vec<Uuid> {
    retrieve_l0_l1(handle).iter().map(|r| r.drawer.id).collect()
}

/// #9719: a drawer added to a live handle is served by L0/L1 without a reopen.
///
/// Why: `l1_drawers` was set only at open and `refresh_l1` had no production
/// caller, so a write never reached L1 until a restart.
/// What: opens an empty palace, remembers one drawer on the live handle, and
/// asserts `retrieve_l0_l1` serves it.
/// Test: this test.
#[tokio::test]
async fn a_drawer_added_after_open_is_served_by_l0_l1() {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-add-after-open");
    let handle = PalaceHandle::open(&palace).expect("open");

    let id = remember(&handle, "Tardigrades survive the vacuum of space").await;

    assert!(
        l0_l1_ids(&handle).contains(&id),
        "#9719: a drawer remembered on a live handle must enter L1"
    );
}

/// #9722: a stale second handle must not revive a forgotten drawer on reopen.
///
/// Why: handle B, opened before handle A forgets a drawer, still holds that
/// drawer in its table. B's next `remember` rewrote `l1_cache.json` with it,
/// and the open-time gap-fill copied it back into the drawer table as live.
/// What: two handles on one palace; A forgets, B remembers, both drop, the
/// palace reopens, and the forgotten id is absent from the drawer table, from
/// L0/L1, and from `list_drawers`.
/// Test: this test.
#[tokio::test]
async fn a_stale_second_handle_does_not_revive_a_forgotten_drawer_on_reopen() {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-stale-handle");
    let a = PalaceHandle::open(&palace).expect("open A");
    let gone = remember(&a, "Mantis shrimp see twelve colour channels").await;
    a.flush().expect("flush A");

    let b = PalaceHandle::open(&palace).expect("open B");
    assert!(
        b.drawers.read().iter().any(|d| d.id == gone),
        "precondition: B opened before the forget and holds the drawer"
    );

    assert_eq!(a.forget(gone).await.expect("forget"), ForgetOutcome::Deleted);
    let kept = remember(&b, "Octopuses taste with their suckers").await;
    drop(a);
    drop(b);

    let reopened = PalaceHandle::open(&palace).expect("reopen");
    assert!(
        !reopened.drawers.read().iter().any(|d| d.id == gone),
        "#9722: the forgotten drawer was revived into the drawer table"
    );
    assert!(
        !l0_l1_ids(&reopened).contains(&gone),
        "#9722: the forgotten drawer is served by L0/L1"
    );
    assert!(
        !reopened
            .list_drawers(None, None, 100)
            .iter()
            .any(|d| d.id == gone),
        "#9722: the forgotten drawer is listed"
    );
    assert!(
        reopened.drawers.read().iter().any(|d| d.id == kept),
        "B's own write must survive the reopen"
    );
}

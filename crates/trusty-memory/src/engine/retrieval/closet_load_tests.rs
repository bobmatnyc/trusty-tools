//! A palace loaded from disk has its closet index, not an empty one (#10377).
//!
//! Why: the L2 closet tag boost (0.15) read `handle.closets`, which was
//! in-memory only and empty after every open, so a restart, evict/reload or
//! restore cost every drawer 0.15 until the next write or dream cycle
//! (live: 0.6796 before, 0.5296 after).
//! What: recall scores must match across a reopen with no write and no dream;
//! an empty palace and an unreadable one open without a panic.
//! Test: this file is the test.

use super::tests::{init_embedder, open_disk_palace};
use super::*;
use crate::engine::palace::{Palace, PalaceId, RoomType};
use tempfile::tempdir;

const TOKEN: &str = "zqxjvkplorb7";

/// Why (#10377): the rank score of a drawer must not depend on whether the
/// handle was filled by a write or loaded from disk.
/// What: remembers two drawers (one holds `TOKEN`), records the L2 score of
/// the token drawer, reopens the palace with no write and no dream, and
/// asserts the same score and a populated closet index.
/// Test: this test.
#[tokio::test]
async fn closet_boost_survives_reopen_without_a_write() {
    init_embedder();
    let dir = tempdir().unwrap();
    let (palace, handle) = open_disk_palace(dir.path(), "closet-reopen");
    let target = handle
        .remember(
            format!("The deploy gate is named {TOKEN} in the runbook"),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    handle
        .remember(
            "Pangolin scales are made of keratin".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let embedder = shared_embedder().await.unwrap();
    let score_of = |hits: Vec<RecallResult>| {
        hits.iter()
            .find(|r| r.drawer.id == target)
            .map(|r| r.score)
            .expect("token drawer must be recalled")
    };
    let s0 = score_of(
        retrieve_l2(&handle, embedder.as_ref(), TOKEN, None, 10)
            .await
            .unwrap(),
    );
    handle.flush().unwrap();
    drop(handle);

    let reopened = PalaceHandle::open(&palace).unwrap();
    let s1 = score_of(
        retrieve_l2(&reopened, embedder.as_ref(), TOKEN, None, 10)
            .await
            .unwrap(),
    );
    assert!(
        (s0 - s1).abs() < 1e-6,
        "score changed across reopen: before {s0}, after {s1}"
    );
    assert!(
        reopened.closets.read().contains_key(TOKEN),
        "a loaded palace must have its closet index built"
    );
}

/// Why (#10377): the load-time rebuild must not break an open that has
/// nothing, or cannot read anything, to index.
/// What: an empty palace opens with an empty closet index; a palace whose
/// `kg.db` is garbage opens as a degraded, empty handle instead of panicking.
/// Test: this test.
#[test]
fn opening_an_empty_or_unreadable_palace_does_not_panic() {
    let dir = tempdir().unwrap();
    let (_p, empty) = open_disk_palace(dir.path(), "closet-empty");
    assert!(empty.closets.read().is_empty());

    let bad = Palace {
        id: PalaceId::new("closet-corrupt"),
        name: "closet-corrupt".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("closet-corrupt"),
    };
    std::fs::create_dir_all(&bad.data_dir).unwrap();
    std::fs::write(bad.data_dir.join("kg.db"), b"not a redb file").unwrap();
    // A corrupt kg.db degrades to a handle with no drawers; it is not an Err.
    let degraded = PalaceHandle::open(&bad).expect("corrupt kg.db opens degraded");
    assert!(degraded.drawers.read().is_empty());
    assert!(degraded.closets.read().is_empty());
}

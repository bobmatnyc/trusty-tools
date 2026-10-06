//! L2 and L3 rank tied drawers by drawer id (#9280).
//!
//! Why: the L2 and L3 sorts kept the vector lane's order among equal scores,
//! and above the exact-scan threshold that order comes from a graph the
//! parallel replay reshapes on every open.
//! What: seeds drawers whose vectors and effective importance are identical,
//! inserted in DESCENDING drawer-id order, and asserts both layers return them
//! in ascending id order.
//! Test: this file is the test.

use super::*;

/// Eight drawers that tie on every score component, upserted in descending
/// drawer-id order so the vector lane hands them back in that order.
async fn tied_palace(dir: &std::path::Path) -> (PalaceHandle, Vec<Uuid>) {
    init_embedder();
    let handle = make_handle(dir);
    let embedder = shared_embedder().await.unwrap();
    let text = "tied drawer about rust".to_string();
    let vector = embedder
        .embed_batch(std::slice::from_ref(&text))
        .await
        .unwrap()[0]
        .clone();
    // Old enough that importance decays to the floor, so a second boundary
    // between two `age_days` calls cannot split the tie.
    let created = chrono::Utc::now() - chrono::Duration::days(3_650);
    let mut drawers: Vec<Drawer> = (0..8)
        .map(|_| {
            let mut d = Drawer::new(Uuid::new_v4(), &text);
            d.created_at = created;
            d
        })
        .collect();
    drawers.sort_by(|a, b| b.id.cmp(&a.id));
    for d in &drawers {
        handle
            .vector_store
            .upsert(d.id, vector.clone())
            .await
            .unwrap();
    }
    let mut ids: Vec<Uuid> = drawers.iter().map(|d| d.id).collect();
    for d in drawers {
        handle.add_drawer(d);
    }
    ids.sort();
    (handle, ids)
}

/// Why (#9280): see the module header.
/// What: `retrieve_l2` and `retrieve_l3` over the tied palace both return the
/// drawers in ascending id order.
#[tokio::test]
async fn l2_and_l3_rank_tied_drawers_by_id() {
    let dir = tempdir().unwrap();
    let (handle, ascending) = tied_palace(dir.path()).await;
    let embedder = shared_embedder().await.unwrap();
    let ids = |hits: Vec<RecallResult>| hits.iter().map(|r| r.drawer.id).collect::<Vec<_>>();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), "rust", None, 8)
        .await
        .unwrap();
    assert_eq!(ids(l2), ascending, "L2 must rank tied drawers by id");
    let l3 = retrieve_l3(&handle, embedder.as_ref(), "rust", None, 8)
        .await
        .unwrap();
    assert_eq!(ids(l3), ascending, "L3 must rank tied drawers by id");
}

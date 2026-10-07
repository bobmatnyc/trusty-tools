//! #9279 AC1 end to end: "ruling e1" recalls the E1 ruling first.
//!
//! Why: in the live supervisor palace (61 drawers, so an exact vector scan)
//! the E1 ruling ranked 12th at 0.208 under other rulings scoring up to 0.298.
//! The keyword extractor dropped `e1`, and the embedding gives it almost no
//! weight. The unit checks live in trusty-common (`id_token_tests`); this file
//! proves the service and MCP recall surfaces carry the fix, with the BM25
//! lane on.
//! What: writes fifteen rulings through `memory_remember`, pins each vector to
//! a chosen cosine with the query (the mock embedder cannot express the live
//! gap), and recalls through `MemoryService::recall_ranked` and
//! `memory_recall`. Its own binary, so seeding the mock embedder cannot race
//! the lib tests' real singleton.
//! Test: this IS the test module.

mod recall_support;

use recall_support::{create_palaces, rank_of, recall, remember};
use tempfile::TempDir;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::{seed_shared_embedder_with_mock, shared_embedder};
use trusty_common::memory_core::store::vector::VectorStore;
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::service::MemoryService;
use trusty_memory::AppState;
use uuid::Uuid;

const PALACE: &str = "supervisor";
const QUERY: &str = "ruling e1";

/// A unit-length copy of `v`.
fn unit(v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / norm).collect()
}

/// A vector whose cosine with `query` is exactly `cos` (Gram-Schmidt on a
/// per-drawer seed, mixed back with `query` at the chosen angle).
fn at_cosine(query: &[f32], seed: usize, cos: f32) -> Vec<f32> {
    let q = unit(query.to_vec());
    let raw: Vec<f32> = (0..q.len())
        .map(|j| ((seed * 31 + j * 7) % 13) as f32 - 6.0)
        .collect();
    let dot: f32 = raw.iter().zip(&q).map(|(a, b)| a * b).sum();
    let ortho = unit(raw.iter().zip(&q).map(|(r, qi)| r - dot * qi).collect());
    let sin = (1.0 - cos * cos).sqrt();
    q.iter()
        .zip(&ortho)
        .map(|(a, b)| cos * a + sin * b)
        .collect()
}

/// An `AppState` with the BM25 lane armed and the supervisor palace created.
async fn state_with_lane(tmp: &TempDir) -> AppState {
    seed_shared_embedder_with_mock();
    let lane = Bm25Lane::new(tmp.path().to_path_buf());
    let state = AppState::new(tmp.path().to_path_buf())
        .with_rulings_palaces(Vec::new())
        .with_bm25_lane(lane);
    state.set_ready();
    assert!(
        state.bm25_lane().is_some(),
        "the lexical lane must be armed"
    );
    create_palaces(&state, tmp, &[PALACE]).await;
    state
}

/// Eleven rulings closer to the query than the E1 ruling, then the E1 ruling
/// at the live 0.208, then three below it. Returns the E1 ruling's id.
async fn seed_rulings(state: &AppState) -> Uuid {
    let mut rulings: Vec<(String, f32)> = (0..11)
        .map(|n| {
            (
                format!("ruling e{}: builders follow band rule number {n}", n + 2),
                0.30 - n as f32 * 0.008,
            )
        })
        .collect();
    rulings.push((
        "ruling E1: hold new builder dispatches while the band is red".into(),
        0.208,
    ));
    for (n, id) in ["f0", "fe", "ff"].iter().enumerate() {
        rulings.push((
            format!("ruling {id}: the PM files no separate fix round"),
            0.19 - n as f32 * 0.01,
        ));
    }

    let mut ids = Vec::new();
    for (content, _) in &rulings {
        ids.push(remember(state, PALACE, content, &["ruling"], None).await);
    }
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(PALACE))
        .expect("open palace");
    let embedder = shared_embedder().await.expect("embedder");
    let q = embedder
        .embed_batch(&[QUERY.to_string()])
        .await
        .expect("embed query")
        .remove(0);
    for (i, (id, (_, cos))) in ids.iter().zip(&rulings).enumerate() {
        handle
            .vector_store
            .upsert(*id, at_cosine(&q, i, *cos))
            .await
            .expect("pin vector");
    }
    ids[11]
}

/// Why (#9279 AC1): "ruling e1" must rank the E1 ruling 1st (today 12th).
/// What: the service recall and the MCP `memory_recall` both put the E1
/// ruling first, with the BM25 lane on.
#[tokio::test(flavor = "multi_thread")]
async fn ruling_e1_recalls_its_drawer_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with_lane(&tmp).await;
    let target = seed_rulings(&state).await;

    let ranked = MemoryService::new(state.clone())
        .recall_ranked(PALACE, QUERY, 10, false)
        .await
        .expect("service recall");
    let first = ranked.iter().find(|r| r.layer != 0).map(|r| r.drawer.id);
    assert_eq!(first, Some(target), "recall_ranked: {ranked:#?}");

    let results = recall(&state, PALACE, QUERY, 10).await;
    assert_eq!(
        rank_of(&results, target),
        Some(0),
        "memory_recall: {results:#?}"
    );
}

//! Short id tokens survive keyword extraction and lift their drawer (#9279).
//!
//! Why: "ruling e1" ranked its drawer 12th in the live supervisor palace. The
//! keyword extractor dropped `e1`, and the embedding gives it almost no weight.
//! What: a unit check on `extract_keywords` and two recall checks over a palace of rulings whose vectors are pinned to
//! chosen cosines with the query, mirroring the live score gap.
//! Test: this file is the test.

use super::*;
use crate::memory_core::dream::extract_keywords;

/// Why (#9279 AC2): tokens shorter than three characters are kept.
/// What: ids of two characters survive, in any case and with punctuation;
/// stop words and one-character tokens still drop.
#[test]
fn extract_keywords_keeps_short_id_tokens() {
    let kws = extract_keywords("Ruling E1: fe f0 #42 (v2) is to ok a x");
    for id in ["e1", "fe", "f0", "42", "v2", "ruling"] {
        assert!(kws.iter().any(|k| k == id), "{id} must survive: {kws:?}");
    }
    for dropped in ["is", "to", "ok", "a", "x"] {
        assert!(
            !kws.iter().any(|k| k == dropped),
            "{dropped} must drop: {kws:?}"
        );
    }
}

/// A unit-length copy of `v`.
fn unit(v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / norm).collect()
}

/// A vector whose cosine with `query` is exactly `cos`.
///
/// What: Gram-Schmidt removes `query` from a per-drawer seed, and the result
/// is mixed back with `query` at the chosen angle.
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

/// Seed `rulings` as `(content, cosine with query)`; returns their ids.
///
/// What: every drawer has the same age and importance, so similarity and the
/// boosts alone decide the order. Closets are rebuilt, as a write does live.
async fn seed_rulings(handle: &PalaceHandle, query: &str, rulings: &[(&str, f32)]) -> Vec<Uuid> {
    let embedder = shared_embedder().await.unwrap();
    let q = embedder.embed_batch(&[query.to_string()]).await.unwrap()[0].clone();
    let created = chrono::Utc::now() - chrono::Duration::days(3_650);
    let mut ids = Vec::new();
    for (i, (content, cos)) in rulings.iter().enumerate() {
        let mut d = Drawer::new(Uuid::new_v4(), *content);
        d.created_at = created;
        handle
            .vector_store
            .upsert(d.id, at_cosine(&q, i, *cos))
            .await
            .unwrap();
        ids.push(d.id);
        handle.add_drawer(d);
    }
    handle.rebuild_closets();
    ids
}

fn ids(hits: &[RecallResult]) -> Vec<Uuid> {
    hits.iter().map(|r| r.drawer.id).collect()
}

/// Why (#9279 AC1): "ruling e1" ranked its drawer 12th, at 0.208, under
/// rulings scoring up to 0.298, because every ruling earns the same closet
/// boost from "ruling" and nothing rewards `e1`.
/// What: eleven other rulings sit closer to the query than the E1 ruling; L2
/// and L3 must both rank the E1 ruling first.
#[tokio::test]
async fn ruling_e1_ranks_its_drawer_first_in_l2_and_l3() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "ruling e1";
    let mut rulings: Vec<(String, f32)> = (0..11)
        .map(|n| {
            (
                format!("ruling e{}: builders follow band rule {n}", n + 2),
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
            format!("ruling {id}: the PM files no fix round"),
            0.19 - n as f32 * 0.01,
        ));
    }
    let refs: Vec<(&str, f32)> = rulings.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let target = seeded[11];
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 10)
        .await
        .unwrap();
    assert_eq!(ids(&l2).first(), Some(&target), "L2 top hit: {l2:#?}");
    let l3 = retrieve_l3(&handle, embedder.as_ref(), query, None, 10)
        .await
        .unwrap();
    assert_eq!(ids(&l3).first(), Some(&target), "L3 top hit: {l3:#?}");
}

/// Why (Architect condition on the E11 boost): the id boost must not reorder
/// a query that names no id.
/// What: the same rulings, one of which mentions `cap` and `e1` and sits
/// lowest; the query "ruling cap policy" has no id-shaped token, so L2
/// returns the drawers in pure similarity order and no score carries the
/// boost.
#[tokio::test]
async fn a_query_without_an_id_token_keeps_its_order() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "ruling cap policy";
    let rulings: Vec<(String, f32)> = (0..6)
        .map(|n| {
            (
                format!("ruling {n}: builders follow band rule"),
                0.40 - n as f32 * 0.02,
            )
        })
        .chain(std::iter::once((
            "ruling e1: the builder cap is four".to_string(),
            0.25,
        )))
        .collect();
    let refs: Vec<(&str, f32)> = rulings.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 10)
        .await
        .unwrap();
    assert_eq!(ids(&l2), seeded, "similarity order must hold: {l2:#?}");
    let top = l2.first().map(|r| r.score).unwrap_or_default();
    // Similarity 0.40 plus the 0.15 closet boost; the 0.3 id boost would
    // push it far past this bound.
    assert!(top < 0.56, "no candidate may carry the id boost: {l2:#?}");
}

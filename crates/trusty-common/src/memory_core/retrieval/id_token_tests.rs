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
    seed_with_ids(handle, query, rulings, &[]).await
}

/// As [`seed_rulings`], with drawer ids taken from `fixed` when it is not empty.
async fn seed_with_ids(
    handle: &PalaceHandle,
    query: &str,
    rulings: &[(&str, f32)],
    fixed: &[Uuid],
) -> Vec<Uuid> {
    let embedder = shared_embedder().await.unwrap();
    let q = embedder.embed_batch(&[query.to_string()]).await.unwrap()[0].clone();
    let created = chrono::Utc::now() - chrono::Duration::days(3_650);
    let mut ids = Vec::new();
    for (i, (content, cos)) in rulings.iter().enumerate() {
        let mut d = Drawer::new(Uuid::new_v4(), *content);
        if let Some(id) = fixed.get(i) {
            d.id = *id;
        }
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

/// Why (#9279 review): `pm` is id-shaped but common. Boosting every drawer
/// that mentions it reordered an ordinary question.
/// What: six of eight drawers mention the PM and sit below two that do not;
/// "what should the PM do" must return all eight in similarity order.
#[tokio::test]
async fn a_common_short_word_keeps_similarity_order() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "what should the PM do";
    let mut drawers: Vec<(String, f32)> = vec![
        ("deploys run from the release branch".into(), 0.40),
        ("the cache key includes the toolchain".into(), 0.38),
    ];
    drawers.extend((0..6).map(|n| {
        (
            format!("the PM files ticket {n} for review"),
            0.36 - n as f32 * 0.02,
        )
    }));
    let refs: Vec<(&str, f32)> = drawers.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 10)
        .await
        .unwrap();
    assert_eq!(ids(&l2), seeded, "similarity order must hold: {l2:#?}");
}

/// Why (#9279 review): the prompt hook recalls with the whole prompt and
/// injects hits at or above `DEFAULT_RELEVANCE_FLOOR`. A 0.05-similarity
/// drawer that shares only "PR" with the prompt scored about 0.50 and was
/// injected.
/// What: "PR" is in every candidate, so the low drawer keeps its low score.
#[tokio::test]
async fn a_common_short_word_stays_below_the_relevance_floor() {
    use super::super::relevance::DEFAULT_RELEVANCE_FLOOR;

    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "check the PR status";
    let mut drawers: Vec<(String, f32)> = (0..9)
        .map(|n| (format!("PR {n} adds a cache layer"), 0.40 - n as f32 * 0.01))
        .collect();
    drawers.push(("the PR template moved".into(), 0.05));
    let refs: Vec<(&str, f32)> = drawers.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let low = seeded[9];
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 20)
        .await
        .unwrap();
    let score = l2
        .iter()
        .find(|r| r.drawer.id == low)
        .map(|r| r.score)
        .unwrap_or_else(|| panic!("low drawer missing: {l2:#?}"));
    assert!(
        score < DEFAULT_RELEVANCE_FLOOR,
        "a shared common word lifted {score} over the floor"
    );
}

/// Why (#9279 review): with boosts up to 0.45, a hard `min(1.0)` gave every
/// strong match the same 1.0 and let the drawer id order them.
/// What: three rulings at 0.99, 0.96 and 0.93 similarity, ids ascending in
/// the opposite order, must rank by similarity with distinct scores below 1.0.
#[tokio::test]
async fn strong_matches_keep_similarity_order_above_the_cap() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "ruling policy";
    let mut fixed: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    fixed.sort();
    fixed.reverse();
    let rulings = [
        ("ruling alpha holds", 0.99),
        ("ruling beta holds", 0.96),
        ("ruling gamma holds", 0.93),
    ];
    let seeded = seed_with_ids(&handle, query, &rulings, &fixed).await;
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 3)
        .await
        .unwrap();
    assert_eq!(ids(&l2), seeded, "similarity order must hold: {l2:#?}");
    let scores: Vec<f32> = l2.iter().map(|r| r.score).collect();
    assert!(
        scores.windows(2).all(|w| w[0] > w[1]) && scores[0] < 1.0,
        "scores must be distinct and below 1.0: {scores:?}"
    );
}

/// Fourteen low-similarity drawers that share no word with the queries below.
fn filler(count: usize, top: f32) -> Vec<(String, f32)> {
    (0..count)
        .map(|n| (format!("node {n} drains the cache"), top - n as f32 * 0.01))
        .collect()
}

/// Why (#9279, Architect ruling 2026-10-07): "PR" held by one drawer passed
/// the rarity gate, so a 0.30-similarity drawer earned +0.45 and cleared the
/// 0.35 hook floor. A two-letter word with no digit is not an id.
/// What: one of fifteen low-similarity drawers holds "PR"; it stays below
/// `DEFAULT_RELEVANCE_FLOOR`.
#[tokio::test]
async fn a_rare_two_letter_word_stays_below_the_relevance_floor() {
    use super::super::relevance::DEFAULT_RELEVANCE_FLOOR;

    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "check the PR status";
    let mut drawers = filler(14, 0.29);
    drawers.push(("the PR template moved".into(), 0.30));
    let refs: Vec<(&str, f32)> = drawers.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let pr = seeded[14];
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 20)
        .await
        .unwrap();
    let score = l2
        .iter()
        .find(|r| r.drawer.id == pr)
        .map(|r| r.score)
        .unwrap_or_else(|| panic!("PR drawer missing: {l2:#?}"));
    assert!(
        score < DEFAULT_RELEVANCE_FLOOR,
        "a rare two-letter word lifted {score} over the floor"
    );
}

/// Why (#9279): requiring a digit must not cost the ids it was built for.
/// What: `e1` and `v2`, each held by one of fifteen drawers at the lowest
/// similarity, rank first and second.
#[tokio::test]
async fn rare_digit_ids_keep_the_boost() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "ship e1 and v2 builds";
    let mut drawers = filler(13, 0.30);
    drawers.push(("Ruling E1: hold dispatches".into(), 0.12));
    drawers.push(("release v2 freezes".into(), 0.10));
    let refs: Vec<(&str, f32)> = drawers.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 15)
        .await
        .unwrap();
    assert_eq!(
        ids(&l2)[..2],
        [seeded[13], seeded[14]],
        "e1 and v2 must rank first: {l2:#?}"
    );
}

/// Why (#9279, Architect ruling 2026-10-07): a three-character id such as
/// `k8s` earned the closet boost on main whether or not it was common. The
/// rarity gate must not take that away.
/// What: eight of nine drawers hold `k8s` below a 0.40 drawer that does not;
/// the closet boost lifts the best `k8s` drawer to first.
#[tokio::test]
async fn a_common_three_char_id_keeps_the_closet_boost() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let query = "k8s rollout plan";
    let mut drawers: Vec<(String, f32)> =
        vec![("deploys run from the release branch".into(), 0.40)];
    drawers.extend((0..8).map(|n| {
        (
            format!("k8s node pool {n} drains first"),
            0.30 - n as f32 * 0.01,
        )
    }));
    let refs: Vec<(&str, f32)> = drawers.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let seeded = seed_rulings(&handle, query, &refs).await;
    let embedder = shared_embedder().await.unwrap();

    let l2 = retrieve_l2(&handle, embedder.as_ref(), query, None, 10)
        .await
        .unwrap();
    assert_eq!(
        ids(&l2).first(),
        Some(&seeded[1]),
        "the k8s closet boost must hold: {l2:#?}"
    );
}

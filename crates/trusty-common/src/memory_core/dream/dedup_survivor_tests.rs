//! Regression tests for #9172: dream dedup must persist the merged text,
//! choose the current drawer as survivor, and journal a survivor's removal.
//!
//! Why: the merge rewrote only the in-memory drawer table, so a loser's text
//! was gone at the next open; the survivor was picked by importance alone; and
//! a user forget of a survivor left no maintenance record.
//! What: drives `dedup_pass_with_embedder` with a constant embedder over
//! drawers whose stored vectors are identical, so any two drawers are
//! near-duplicates regardless of their text.
//! Test: itself.

use super::cycle::dedup_pass_with_embedder;
use crate::memory_core::embed::{EMBED_DIM, Embedder};
use crate::memory_core::maintenance_log::{DeletionReason, read_journal};
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{ForgetOutcome, PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::store::vector::VectorStore;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

/// Every text embeds to the same unit vector, so every pair scores 1.0.
struct ConstEmbedder;

#[async_trait]
impl Embedder for ConstEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| unit()).collect())
    }

    fn dimension(&self) -> usize {
        EMBED_DIM
    }
}

fn unit() -> Vec<f32> {
    let mut v = vec![0.0_f32; EMBED_DIM];
    v[0] = 1.0;
    v
}

fn palace_in(dir: &TempDir, name: &str) -> Palace {
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: dir.path().join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    palace
}

fn open(palace: &Palace) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    PalaceHandle::open(palace).unwrap()
}

/// What a test drawer differs in: its age, importance, tags and slot.
struct Spec {
    content: &'static str,
    importance: f32,
    age_days: i64,
    tags: &'static [&'static str],
    fact_key: Option<&'static str>,
}

/// Write `spec` to redb, the vector index (as [`unit`]) and the drawer table.
async fn put(handle: &Arc<PalaceHandle>, spec: &Spec) -> Uuid {
    put_with_vector(handle, spec, unit()).await
}

/// [`put`] with an explicit stored vector.
async fn put_with_vector(handle: &Arc<PalaceHandle>, spec: &Spec, vector: Vec<f32>) -> Uuid {
    let mut d = Drawer::new(Uuid::new_v4(), spec.content);
    d.importance = spec.importance;
    d.created_at = Utc::now() - ChronoDuration::days(spec.age_days);
    d.tags = spec.tags.iter().map(|t| t.to_string()).collect();
    d.fact_key = spec.fact_key.map(str::to_string);
    let id = d.id;
    handle.kg.upsert_drawer(&d).await.unwrap();
    handle.vector_store.upsert(id, vector).await.unwrap();
    handle.add_drawer(d);
    id
}

async fn dedup(handle: &Arc<PalaceHandle>) -> usize {
    dedup_pass_with_embedder(
        handle,
        Instant::now(),
        Duration::from_secs(60),
        0.95,
        &ConstEmbedder,
        Duration::from_secs(60),
    )
    .await
    .unwrap()
    .merged
}

fn ids(handle: &PalaceHandle) -> Vec<Uuid> {
    handle.drawers.read().iter().map(|d| d.id).collect()
}

fn content_of(handle: &PalaceHandle, id: Uuid) -> String {
    let drawers = handle.drawers.read();
    drawers
        .iter()
        .find(|d| d.id == id)
        .map(|d| d.content().to_string())
        .unwrap()
}

/// Why (#9172 closure 1): the "Also:" text of 16 merged losers was gone after
/// a daemon restart, because the merge only changed the in-memory table.
/// What: merges two different near-duplicates, drops the handle, reopens the
/// palace from disk, and asserts the survivor still holds the loser's text.
#[tokio::test]
async fn a_dedup_merge_survives_a_palace_reopen() {
    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "merge-reopen");
    let handle = open(&palace);
    let keep = Spec {
        content: "The release train ships every Tuesday after the gate run",
        importance: 0.8,
        age_days: 0,
        tags: &[],
        fact_key: None,
    };
    let lose = Spec {
        content: "Release trains leave on Tuesdays once the gates are green",
        importance: 0.4,
        age_days: 1,
        tags: &[],
        fact_key: None,
    };
    let keep_id = put(&handle, &keep).await;
    put(&handle, &lose).await;

    assert_eq!(dedup(&handle).await, 1);
    assert_eq!(ids(&handle), vec![keep_id]);
    drop(handle);

    let reopened = open(&palace);
    assert_eq!(ids(&reopened), vec![keep_id]);
    let merged = content_of(&reopened, keep_id);
    assert!(
        merged.contains(keep.content) && merged.contains(lose.content),
        "the merged text must survive a reopen, got {merged:?}"
    );
}

/// Why (#9172 closure 2): dedup kept the higher-importance drawer, so an older
/// status note could replace the newest one.
/// What: an older, more important note and a newer, less important duplicate;
/// the newer one must survive.
#[tokio::test]
async fn a_newer_status_note_survives_an_older_higher_importance_duplicate() {
    let dir = tempdir().unwrap();
    let handle = open(&palace_in(&dir, "newer-wins"));
    put(
        &handle,
        &Spec {
            content: "Status: PR 9122 is waiting on review",
            importance: 0.9,
            age_days: 3,
            tags: &[],
            fact_key: None,
        },
    )
    .await;
    let newer = put(
        &handle,
        &Spec {
            content: "Status: PR 9122 is merged and released",
            importance: 0.3,
            age_days: 0,
            tags: &[],
            fact_key: None,
        },
    )
    .await;

    assert_eq!(dedup(&handle).await, 1);
    assert_eq!(ids(&handle), vec![newer], "the newer note must survive");
    let survivor = handle.drawers.read()[0].clone();
    assert!(
        (survivor.importance - 0.9).abs() < f32::EPSILON,
        "the survivor keeps the higher importance"
    );
}

/// Why (#9172 closure 2): a drawer holding a live `fact_key` slot, or carrying
/// the `ruling` tag, is a current fact. A lower-importance, newer duplicate
/// must not replace it, and two slot holders must not merge at all.
/// What: one row per protected kind against an unprotected, newer, more
/// important duplicate; then two slot holders, which must both stay.
#[tokio::test]
async fn a_slot_holder_or_ruling_outlives_a_more_important_duplicate() {
    let cases: [(&str, &[&str], Option<&str>); 2] = [
        ("slot", &[], Some("pr:9172/state")),
        ("ruling", &["ruling"], None),
    ];
    for (name, tags, fact_key) in cases {
        let dir = tempdir().unwrap();
        let handle = open(&palace_in(&dir, &format!("protect-{name}")));
        let protected = put(
            &handle,
            &Spec {
                content: "Owner ruling: merge only on green CI",
                importance: 0.2,
                age_days: 5,
                tags,
                fact_key,
            },
        )
        .await;
        put(
            &handle,
            &Spec {
                content: "Owner ruling: merge only once CI is green",
                importance: 0.9,
                age_days: 0,
                tags: &[],
                fact_key: None,
            },
        )
        .await;
        assert_eq!(dedup(&handle).await, 1, "{name}");
        assert_eq!(
            ids(&handle),
            vec![protected],
            "{name}: protected must survive"
        );
    }

    let dir = tempdir().unwrap();
    let handle = open(&palace_in(&dir, "two-slots"));
    for (content, key) in [
        (
            "Workstream alpha resumes at the gate run",
            "ws:alpha/resume",
        ),
        (
            "Workstream alpha resumes after the gate run",
            "ws:beta/resume",
        ),
    ] {
        put(
            &handle,
            &Spec {
                content,
                importance: 0.5,
                age_days: 0,
                tags: &[],
                fact_key: Some(key),
            },
        )
        .await;
    }
    assert_eq!(dedup(&handle).await, 0, "two slot holders never merge");
    assert_eq!(ids(&handle).len(), 2);
}

/// Why (Fail-Open Check, #9172): the merge and the loser's removal are two
/// writes. If the merged survivor cannot be persisted, the loser is the only
/// durable copy of its text and must not be deleted.
/// What: stalls the survivor's redb upsert past a 100 ms transaction budget so
/// that write fails while deletes still commit; the loser must remain in the
/// drawer table and in redb, and the pass must report no merge.
#[tokio::test]
async fn a_failed_merge_persist_keeps_the_duplicate() {
    use crate::memory_core::store::kg_redb::BatchWriteOp;
    let dir = tempdir().unwrap();
    let handle = open(&palace_in(&dir, "persist-fails"));
    let keep = put(
        &handle,
        &Spec {
            content: "Gate runs use the shared cargo target directory",
            importance: 0.5,
            age_days: 0,
            tags: &[],
            fact_key: None,
        },
    )
    .await;
    let lose = put(
        &handle,
        &Spec {
            content: "Gate runs share one cargo target directory",
            importance: 0.5,
            age_days: 1,
            tags: &[],
            fact_key: None,
        },
    )
    .await;

    let store = handle.kg.redb_store();
    *store.test_hooks().txn_budget.lock().unwrap() = Some(Duration::from_millis(100));
    *store.test_hooks().after_batch_op.lock().unwrap() =
        Some(Arc::new(move |op: &BatchWriteOp| {
            if let BatchWriteOp::UpsertDrawer(d) = op
                && d.id == keep
            {
                std::thread::sleep(Duration::from_millis(400));
            }
        }));

    assert_eq!(
        dedup(&handle).await,
        0,
        "a merge that did not persist is not a merge"
    );
    *store.test_hooks().after_batch_op.lock().unwrap() = None;
    let mut left = ids(&handle);
    left.sort();
    let mut both = vec![keep, lose];
    both.sort();
    assert_eq!(left, both, "the duplicate must stay when the merge failed");
    assert!(
        handle.kg.load_drawer(lose).unwrap().is_some(),
        "the duplicate's redb row must stay"
    );
    assert_eq!(
        content_of(&handle, keep),
        "Gate runs use the shared cargo target directory",
        "the in-memory survivor must not show an unpersisted merge"
    );
}

/// Why (#9172 closure 3): two dedup survivors were later removed by a path
/// that left no journal record, so the text merged into them was untraceable.
/// What: merges a pair, then forgets the survivor through the user path; the
/// journal must hold a record for the survivor as well as for the loser.
#[tokio::test]
async fn forgetting_a_dedup_survivor_writes_a_journal_record() {
    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "survivor-forget");
    let handle = open(&palace);
    let keep = put(
        &handle,
        &Spec {
            content: "Dream dedup keeps the newest drawer of a pair",
            importance: 0.5,
            age_days: 0,
            tags: &[],
            fact_key: None,
        },
    )
    .await;
    put(
        &handle,
        &Spec {
            content: "Dream dedup keeps the most recent drawer of a pair",
            importance: 0.5,
            age_days: 2,
            tags: &[],
            fact_key: None,
        },
    )
    .await;
    assert_eq!(dedup(&handle).await, 1);
    assert_eq!(handle.forget(keep).await.unwrap(), ForgetOutcome::Deleted);

    let journal = read_journal(&palace.data_dir).unwrap();
    let rec = journal
        .records
        .iter()
        .find(|r| r.drawer_id == keep)
        .unwrap_or_else(|| panic!("no record for the survivor: {:?}", journal.records));
    // Compared by its journal name so this test also compiles before the fix.
    assert_eq!(
        serde_json::to_value(rec.reason).unwrap(),
        "forget_of_merged_survivor"
    );
    assert_ne!(rec.reason, DeletionReason::DreamDedup);
}

/// Why (#9172): a drawer no dedup pass ever named as survivor is an ordinary
/// user forget, not a survivor forget. #9283: it is journaled as
/// `user_forget`, with no content copy.
#[tokio::test]
async fn forgetting_an_unmerged_drawer_writes_only_a_user_forget_record() {
    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "plain-forget");
    let handle = open(&palace);
    let id = handle
        .remember(
            "A fact nobody merged".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    assert_eq!(handle.forget(id).await.unwrap(), ForgetOutcome::Deleted);
    let records = read_journal(&palace.data_dir).unwrap().records;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].reason, DeletionReason::UserForget);
    assert!(records[0].drawer.is_none(), "a user forget keeps no copy");
}

/// Why (#9172): an over-bound merge must not drop the loser's text either.
/// What: two near-duplicates whose merge would pass `MERGE_MAX_BYTES` by one
/// byte; the dedup pass merges nothing and both drawers keep their content.
#[tokio::test]
async fn a_merge_past_the_byte_bound_keeps_both_drawers() {
    use super::helpers::MERGE_MAX_BYTES;
    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "merge-bound");
    let handle = open(&palace);
    let half = MERGE_MAX_BYTES / 2;
    let rest = MERGE_MAX_BYTES - half - "\n\nAlso: ".len();
    let newer: &'static str = "n".repeat(half).leak();
    let older: &'static str = "o".repeat(rest + 1).leak();
    let spec = |content, age_days| Spec {
        content,
        importance: 0.5,
        age_days,
        tags: &[],
        fact_key: None,
    };
    let newer_id = put(&handle, &spec(newer, 0)).await;
    let older_id = put(&handle, &spec(older, 1)).await;

    assert_eq!(dedup(&handle).await, 0);
    assert_eq!(content_of(&handle, newer_id), newer);
    assert_eq!(content_of(&handle, older_id), older);
}

/// The vector the process-wide mock embedder gives `text` in a dream cycle.
async fn mock_vector(text: &str) -> Vec<f32> {
    let mock = crate::embedder::MockEmbedder::new(EMBED_DIM);
    mock.embed_batch(&[text.to_string()])
        .await
        .unwrap()
        .remove(0)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

/// Why (#9172 review): two dream cycles on one palace interleaved. A merged
/// Y into X; B merged Z into Y; A then deleted Y, and B deleted Z, so Z's text
/// left the live palace.
/// What: cycle A pauses at the seam after persisting X+Y. Z is written, and
/// cycle B starts on the same handle. If B reaches the seam too (it merged
/// Y+Z), A is resumed first and then B: the interleaving above. Every text
/// must still be in a live drawer afterwards. The stored vectors make Y's
/// nearest neighbour X while only X and Y exist, and Z once Z exists; X's own
/// query matches neither, so A cannot fold Z in after it resumes.
#[tokio::test]
async fn a_second_dream_cycle_on_a_dreaming_palace_loses_no_text() {
    use super::cycle::merge_seam;
    use super::{DreamConfig, Dreamer};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, mpsc};

    let (y_text, x_text, z_text) = (
        "alpha beta gamma delta epsilon zeta",
        "ZULU 4040 QUEBEC 7777 XRAY 0000",
        "omega sigma kappa lambda theta iota",
    );
    let y_query = mock_vector(y_text).await;
    assert!(
        cosine(&mock_vector(x_text).await, &y_query) < 0.9,
        "X must not match Z"
    );
    // X's stored vector: Y's query plus an orthogonal component, cosine ~0.96.
    let norm = y_query.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mut x_vec: Vec<f32> = y_query.iter().map(|x| x / norm).collect();
    let free = x_vec.iter().position(|x| *x == 0.0).expect("a zero slot");
    x_vec[free] = 0.3;

    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "single-flight-dream");
    let handle = open(&palace);
    let spec = |content, age_days| Spec {
        content,
        importance: 0.5,
        age_days,
        tags: &[],
        fact_key: None,
    };
    put_with_vector(&handle, &spec(y_text, 1), y_query.clone()).await;
    put_with_vector(&handle, &spec(x_text, 0), x_vec).await;

    let (paused_tx, mut paused) = mpsc::unbounded_channel::<usize>();
    let releases: Arc<[Notify; 2]> = Arc::new([Notify::new(), Notify::new()]);
    let calls = Arc::new(AtomicUsize::new(0));
    let hook: merge_seam::Hook = {
        let releases = releases.clone();
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            let (tx, releases) = (paused_tx.clone(), releases.clone());
            Box::pin(async move {
                let _ = tx.send(n);
                if let Some(release) = releases.get(n) {
                    release.notified().await;
                }
            })
        })
    };
    merge_seam::HOOKS
        .lock()
        .unwrap()
        .insert(palace.id.as_str().to_string(), hook);

    let dreamer = Arc::new(Dreamer::new(DreamConfig {
        dedup_threshold: 0.9,
        recall_benchmark_enabled: false,
        compact: false,
        semantic: crate::memory_core::semantic_consolidation::SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        ..DreamConfig::default()
    }));
    let cycle = |dreamer: Arc<Dreamer>, handle: Arc<PalaceHandle>| {
        tokio::spawn(async move { dreamer.dream_cycle(&handle).await })
    };
    let wait = Duration::from_secs(30);

    let a = cycle(dreamer.clone(), handle.clone());
    let first = tokio::time::timeout(wait, paused.recv()).await.unwrap();
    assert_eq!(first, Some(0), "cycle A merges X and Y");
    put_with_vector(&handle, &spec(z_text, 2), y_query.clone()).await;

    let mut b = cycle(dreamer.clone(), handle.clone());
    let b_paused = tokio::time::timeout(wait, async {
        tokio::select! {
            n = paused.recv() => { assert_eq!(n, Some(1)); true }
            joined = &mut b => { joined.unwrap().unwrap(); false }
        }
    })
    .await
    .unwrap();
    releases[0].notify_one();
    tokio::time::timeout(wait, a)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if b_paused {
        releases[1].notify_one();
        tokio::time::timeout(wait, b)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    merge_seam::HOOKS.lock().unwrap().remove(palace.id.as_str());

    let live: Vec<String> = handle
        .drawers
        .read()
        .iter()
        .map(|d| d.content().to_string())
        .collect();
    for text in [x_text, y_text, z_text] {
        assert!(
            live.iter().any(|c| c.contains(text)),
            "{text:?} left the live palace (B interleaved: {b_paused}): {live:?}"
        );
    }
}

/// Why (#8246): dedup persists each merge as it goes, so a cycle that fails
/// after one has still rewritten a survivor's text. trusty-memory's BM25 lane
/// learns of that only through the after-cycle hook, which fired on `Ok` alone.
/// What: two drawers that both store `older`'s query vector, so `older` finds
/// `newer` at score 1.0 and the pass merges them. The merge seam then fails the
/// pass. The cycle returns `Err`, a live drawer holds both texts, and the hook
/// was called once, with `None`.
#[tokio::test]
async fn a_cycle_that_fails_after_a_merge_still_calls_the_hook() {
    use super::cycle::merge_seam;
    use super::{DreamConfig, DreamStats, Dreamer};

    let (older, newer) = (
        "The rollback runbook lists every database snapshot step in order",
        "Feature flags gate the staged rollout across every region we serve",
    );
    let dir = tempdir().unwrap();
    let palace = palace_in(&dir, "dream-fails-after-merge");
    let handle = open(&palace);
    let spec = |content, age_days| Spec {
        content,
        importance: 0.5,
        age_days,
        tags: &[],
        fact_key: None,
    };
    let query = mock_vector(older).await;
    put_with_vector(&handle, &spec(older, 1), query.clone()).await;
    put_with_vector(&handle, &spec(newer, 0), query).await;
    merge_seam::FAIL_AFTER_MERGE
        .lock()
        .unwrap()
        .insert(palace.id.as_str().to_string());

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.9,
        recall_benchmark_enabled: false,
        compact: false,
        semantic: crate::memory_core::semantic_consolidation::SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        ..DreamConfig::default()
    })
    .with_after_cycle(Arc::new(
        move |id: &PalaceId, stats: Option<&DreamStats>| {
            sink.lock().unwrap().push((id.clone(), stats.cloned()))
        },
    ));
    let outcome = dreamer.dream_cycle(&handle).await;
    merge_seam::FAIL_AFTER_MERGE
        .lock()
        .unwrap()
        .remove(palace.id.as_str());

    assert!(
        outcome.is_err(),
        "precondition: the injected failure ends the cycle: {outcome:?}"
    );
    let live: Vec<String> = handle
        .drawers
        .read()
        .iter()
        .map(|d| d.content().to_string())
        .collect();
    assert!(
        live.iter().any(|c| c.contains(older) && c.contains(newer)),
        "precondition: the merge persisted before the failure: {live:?}"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(palace.id.clone(), None)],
        "#8246: a cycle that failed after a merge must still call the hook"
    );
}

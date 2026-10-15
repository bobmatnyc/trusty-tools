//! #9735 PR 1b: the backfill splits a drawer over the chunk cap.
//!
//! Why: a drawer whose text needs more than `MAX_CHUNKS_PER_DRAWER` chunks was
//! ledgered and retried once a day forever, so it never ended complete.
//! What: disk palaces driven through `reembed_slice` with the window-12 mock
//! (ten words per chunk, nine-word step). `OVER_CAP` words make 4097 chunks,
//! so a split makes five parts: four of 1024 chunks and one of a single chunk.
//! Every assertion reads the drawer table, redb, the vector index, the ledger
//! or the journal, never a counter the slice reports about itself.
//! Test: this file IS the tests.

use super::chunk_backfill::{SliceBounds, SliceReport, SliceStop};
use super::embed_store_tests::{Counting, DIM, chunks_at, handle, shape, words};
use super::handle::PalaceHandle;
use super::types::ForgetOutcome;
use crate::engine::embed::Embedder;
use crate::engine::maintenance_log::{DeletionReason, read_journal};
use crate::engine::palace::{Drawer, Palace, PalaceId};
use crate::engine::store::embed_ledger::{self, EmbedFailure};
use crate::engine::store::kg::Triple;
use crate::engine::store::kg_redb::BatchWriteOp;
use crate::engine::store::vector::VectorStore;
use anyhow::Result;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use trusty_common::embedder::{ChunkError, ChunkTokenizer, MockEmbedder};
use uuid::Uuid;

const WINDOW: usize = 12;
/// Chunks per part; mirrors `chunk_split::PART_CHUNKS` without importing it,
/// so this file also builds against the pre-split tree.
const PART_CHUNKS: usize = 1024;
/// Words that chunk to exactly `MAX_CHUNKS_PER_DRAWER + 1` = 4097 chunks.
const OVER_CAP: usize = 10 + 9 * 4096;
/// Parts `OVER_CAP` splits into.
const PARTS: usize = 5;

type Emb = Arc<dyn Embedder + Send + Sync>;

fn chunking() -> Emb {
    Arc::new(Counting::chunking(WINDOW))
}

/// No min-age, no backoff, no chunk or drawer cap and a budget that cannot
/// expire: one slice splits, the next embeds every part, however slow the
/// runner (#9735 PR 1b).
fn bounds() -> SliceBounds {
    SliceBounds {
        budget: Duration::MAX,
        max_drawers: usize::MAX,
        max_chunks: usize::MAX,
        min_age: Duration::ZERO,
        ledger_backoff: Duration::ZERO,
        ..SliceBounds::default()
    }
}

/// An oversized drawer in redb and the table, with one legacy vector row.
async fn oversized(h: &PalaceHandle, text: &str) -> Uuid {
    let mut d = Drawer::new(Uuid::new_v4(), text);
    d.created_at -= chrono::Duration::hours(1);
    d.importance = 0.9;
    d.tags = vec!["topic:split".to_string()];
    let id = d.id;
    h.kg.upsert_drawer(&d).await.unwrap();
    h.add_drawer(d);
    h.vector_store
        .upsert_chunks(id, vec![vec![1.0; h.vector_store.dim()]])
        .await
        .unwrap();
    id
}

/// Run slices until health has nothing to repair; at most six. A split slice
/// embeds nothing, so a slice's own counts cannot say the work is done.
async fn drive(h: &PalaceHandle, e: &Emb) -> Vec<SliceReport> {
    let mut reports = Vec::new();
    for _ in 0..6 {
        reports.push(h.reembed_slice(e, &bounds()).await);
        let health = h.embed_health_with(Some(e.as_ref()));
        if health.missing_vector_ids.is_empty() && health.short_chunk_ids.is_empty() {
            break;
        }
    }
    reports
}

fn split_of(head: Uuid) -> String {
    format!("split-of:{head}")
}

/// The head and every drawer tagged as its part, head first, then by id.
fn family(h: &PalaceHandle, head: Uuid) -> Vec<Drawer> {
    let tag = split_of(head);
    let mut out: Vec<Drawer> = h
        .drawers
        .read()
        .iter()
        .filter(|d| d.id == head || d.tags.contains(&tag))
        .cloned()
        .collect();
    out.sort_by_key(|d| (d.id != head, d.id));
    out
}

fn tails(h: &PalaceHandle, head: Uuid) -> Vec<Drawer> {
    family(h, head)
        .into_iter()
        .filter(|d| d.id != head)
        .collect()
}

/// The deterministic id of part `k` (2..=N) of `head`.
fn part_id(head: Uuid, k: usize) -> Uuid {
    Uuid::new_v5(&head, format!("split:{k}").as_bytes())
}

/// The chunking mock's bucket for `word` (FNV-1a modulo `DIM`).
fn bucket(word: &str) -> u64 {
    let h = word.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    h % DIM as u64
}

/// Part `j` (1-based): the head for 1, else the deterministic tail id.
fn part_at(head: Uuid, j: usize) -> Uuid {
    if j == 1 { head } else { part_id(head, j) }
}

fn content_of(h: &PalaceHandle, id: Uuid) -> Option<String> {
    h.drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .map(|d| d.content().to_string())
}

fn assert_split(h: &PalaceHandle, head: Uuid) {
    let n = family(h, head).len();
    assert_eq!(n, PARTS, "the oversized drawer splits into {PARTS} parts");
}

fn ledger_ids(dir: &std::path::Path) -> Vec<Uuid> {
    embed_ledger::load(dir)
        .iter()
        .map(|f| f.drawer_id)
        .collect()
}

fn ledger_row(dir: &std::path::Path, id: Uuid) {
    embed_ledger::record(
        dir,
        EmbedFailure {
            drawer_id: id,
            failed_at: chrono::Utc::now() - chrono::Duration::days(2),
            attempts: 1,
            reason: "drawer text needs 4097 chunks; at most 4096 are stored per drawer".to_string(),
        },
    )
    .unwrap();
}

/// Plan test 1: a cut inside a chunk, or parts not bounded, re-plans past
/// `PART_CHUNKS` or leaves a drawer incomplete.
#[tokio::test]
async fn every_part_replans_within_the_cap_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    assert_eq!(chunks_at(WINDOW, &words(OVER_CAP)), 4097, "fixture size");
    let head = oversized(&h, &words(OVER_CAP)).await;
    let e = chunking();
    let reports = drive(&h, &e).await;
    assert!(
        reports.iter().all(|r| !matches!(
            r.stop,
            SliceStop::StoreError(_) | SliceStop::EmbedTimeout | SliceStop::HostFailure
        )),
        "no op hits a store, embed or host bound: {reports:?}"
    );
    assert_split(&h, head);
    for d in family(&h, head) {
        let n = chunks_at(WINDOW, d.content());
        assert!(n <= PART_CHUNKS, "part {} re-plans to {n} chunks", d.id);
        assert_eq!(
            shape(&h, d.id),
            Some((n, (n - 1) as u32)),
            "part {} complete",
            d.id
        );
    }
    let health = h.embed_health_with(Some(e.as_ref()));
    assert!(health.short_chunk_ids.is_empty() && health.missing_vector_ids.is_empty());
    assert!(health.alias_audit.is_clean(), "{:?}", health.alias_audit);
}

/// Plan test 2: every original chunk sits whole inside one part, so a fact at
/// a seam is never cut, and the chunk at each seam is recalled at rank 1 from the adjacent part.
#[tokio::test]
async fn a_needle_at_every_seam_is_recalled_at_rank_1() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let mut text: Vec<String> = words(OVER_CAP).split(' ').map(str::to_string).collect();
    // Seam k starts part k+2 (1-based) at chunk (k+1)*1024, i.e. at word
    // 9*1024*(k+1). #9735: the mock is a 16-bucket word-hash bag, where 25 of
    // one part's 1024 `w<i>` chunks share a bag. So the chunk starting at each
    // seam is ten copies of a token with its own bucket: its stored vector is
    // one-hot and no other chunk matches it.
    let seams: Vec<usize> = (1..PARTS).map(|k| 9 * PART_CHUNKS * k).collect();
    let mut used = HashSet::new();
    let tokens: Vec<String> = (0..seams.len())
        .map(|k| {
            (0..)
                .map(|i| format!("seam{k}n{i}"))
                .find(|w| used.insert(bucket(w)))
                .expect("a free bucket")
        })
        .collect();
    for (seam, token) in seams.iter().zip(&tokens) {
        for slot in &mut text[*seam..*seam + 10] {
            *slot = token.clone();
        }
    }
    let text = text.join(" ");
    let head = oversized(&h, &text).await;
    let e = chunking();
    drive(&h, &e).await;
    assert_split(&h, head);

    let mock = MockEmbedder::new(DIM).with_chunking(WINDOW);
    let tok = mock.chunking_tokenizer().unwrap().expect("tokenizer");
    let parts: Vec<String> = family(&h, head)
        .iter()
        .map(|d| d.content().to_string())
        .collect();
    let chunks = trusty_common::embedder::chunk_text(&tok, &text, WINDOW).unwrap();
    for c in &chunks {
        assert!(
            parts.iter().any(|p| p.contains(c.text)),
            "original chunk at bytes {:?} is cut by a seam",
            c.byte_range
        );
    }
    // The seam chunk's own text embeds to its stored one-hot vector.
    for (k, token) in tokens.iter().enumerate() {
        let chunk = chunks
            .iter()
            .find(|c| c.text.split_whitespace().all(|w| w == token))
            .expect("the seam chunk survives chunking");
        let v = e
            .embed_batch(&[chunk.text.to_string()])
            .await
            .unwrap()
            .remove(0);
        let hits = h.vector_store.search(&v, 1).await.unwrap();
        let top = hits.first().expect("one hit").drawer_id;
        // Seam k lies between 1-based parts k+1 and k+2; part 1 is the head and
        // part j >= 2 has id `part_id(head, j)`.
        let (before, after) = (part_at(head, k + 1), part_at(head, k + 2));
        assert!(
            top == before || top == after,
            "seam {k} chunk recalls part {} or {}, got {top}",
            k + 1,
            k + 2
        );
    }
}

/// Plan test 3 and Fail-Open (a): the head's redb write fails after the tails
/// committed. The slice stops on a store error; the rerun re-upserts the same
/// ids and finishes with exactly N parts, not 2N.
#[tokio::test]
async fn split_is_idempotent_after_a_crash_between_tails_and_head() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let original = words(OVER_CAP);
    let head = oversized(&h, &original).await;
    let store = h.kg.redb_store();
    *store.test_hooks().txn_budget.lock().unwrap() = Some(Duration::from_millis(300));
    *store.test_hooks().after_batch_op.lock().unwrap() =
        Some(Arc::new(move |op: &BatchWriteOp| {
            if let BatchWriteOp::UpsertDrawer(d) = op
                && d.id == head
            {
                std::thread::sleep(Duration::from_millis(900));
            }
        }));
    ledger_row(dir.path(), head);
    let e = chunking();
    let first = h.reembed_slice(&e, &bounds()).await;
    assert!(
        matches!(first.stop, SliceStop::StoreError(_)),
        "a failed head write stops the slice: {first:?}"
    );
    assert_eq!(first.repaired, 0, "{first:?}");
    assert_eq!(content_of(&h, head).as_deref(), Some(original.as_str()));
    assert!(
        ledger_ids(dir.path()).contains(&head),
        "the ledger row stays"
    );
    assert_eq!(
        tails(&h, head).len(),
        PARTS - 1,
        "the tails committed first"
    );

    *store.test_hooks().after_batch_op.lock().unwrap() = None;
    *store.test_hooks().txn_budget.lock().unwrap() = None;
    drive(&h, &e).await;
    assert_split(&h, head);
    let ids: Vec<Uuid> = tails(&h, head).iter().map(|d| d.id).collect();
    let mut want: Vec<Uuid> = (2..=PARTS).map(|k| part_id(head, k)).collect();
    want.sort();
    assert_eq!(ids, want, "the rerun reused the deterministic ids");
    assert!(content_of(&h, head).is_some_and(|c| c.len() < original.len()));
}

/// Plan test 4a: the head keeps its id, its Tier C slot and its KG triples.
#[tokio::test]
async fn head_keeps_id_fact_key_and_kg_triples() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let mut d = Drawer::new(Uuid::new_v4(), words(OVER_CAP));
    d.created_at -= chrono::Duration::hours(1);
    d.fact_key = Some("pr:9735/state".to_string());
    let head = d.id;
    h.kg.upsert_drawer(&d).await.unwrap();
    h.add_drawer(d);
    let subject = format!("drawer:{head}");
    h.kg.assert(Triple {
        subject: subject.clone(),
        predicate: "mentions".into(),
        object: "split".into(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    drive(&h, &chunking()).await;
    assert_split(&h, head);
    let fam = family(&h, head);
    assert_eq!(fam[0].fact_key.as_deref(), Some("pr:9735/state"));
    assert!(
        fam[1..].iter().all(|t| t.fact_key.is_none()),
        "tails claim no slot"
    );
    assert_eq!(
        h.kg.drawer_id_for_fact_key("pr:9735/state").unwrap(),
        Some(head)
    );
    assert_eq!(h.kg.query_active(&subject).await.unwrap().len(), 1);
    let stored = h.kg.load_drawer(head).unwrap().expect("head row");
    assert_eq!(
        stored.content(),
        fam[0].content(),
        "redb holds the shrunk head"
    );
}

/// ADR-0028 D6 (#9735): the tails of a Tier C head carry no expiry, so the
/// sweep cannot hard-delete them while the head survives, and they keep the
/// head's access history (the prune boost).
#[tokio::test]
async fn tier_c_head_tails_survive_its_expiry_and_keep_access_history() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let expires = chrono::Utc::now() + chrono::Duration::seconds(6);
    let accessed = chrono::Utc::now() - chrono::Duration::minutes(5);
    let mut d = Drawer::new(Uuid::new_v4(), words(OVER_CAP));
    d.created_at -= chrono::Duration::hours(1);
    d.fact_key = Some("pr:9735/retiring".to_string());
    d.expires_at = Some(expires);
    d.access_count = 7;
    d.last_accessed_at = Some(accessed);
    let head = d.id;
    h.kg.upsert_drawer(&d).await.unwrap();
    h.add_drawer(d);
    h.reembed_slice(&chunking(), &bounds()).await;
    assert_split(&h, head);
    for t in tails(&h, head) {
        assert_eq!(t.expires_at, None, "a Tier C head's tail has no expiry");
        assert_eq!(t.access_count, 7, "tail keeps the head's access count");
        assert_eq!(t.last_accessed_at, Some(accessed));
    }
    let wait = (expires - chrono::Utc::now()).to_std().unwrap_or_default();
    tokio::time::sleep(wait + Duration::from_millis(200)).await;
    assert_eq!(h.purge_expired().await.unwrap(), 0, "D6: nothing is reaped");
    assert_split(&h, head);
}

/// Plan test 4b: tail ids are `v5(head, "split:k")`, so a rerun cannot mint
/// a second set.
#[tokio::test]
async fn tail_ids_are_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &chunking()).await;
    for k in 2..=PARTS {
        let t = h.kg.load_drawer(part_id(head, k)).unwrap();
        let t = t.unwrap_or_else(|| panic!("part {k} is in redb under its v5 id"));
        assert!(t.tags.contains(&split_of(head)));
        assert!(t.tags.contains(&format!("split-part:{k}/{PARTS}")));
    }
    let fam = family(&h, head);
    assert!(fam[0].tags.contains(&format!("split-part:1/{PARTS}")));
}

/// Wraps the chunking mock; once armed, its `arm_at`-th tokenizer request
/// rewrites or removes the head in the live table.
struct Mutating {
    inner: MockEmbedder,
    drawers: Arc<RwLock<Vec<Drawer>>>,
    head: Uuid,
    forget: bool,
    calls: AtomicUsize,
    arm_at: AtomicUsize,
}

#[async_trait]
impl Embedder for Mutating {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        self.inner.input_window()
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.arm_at.load(Ordering::SeqCst) {
            let mut drawers = self.drawers.write();
            if self.forget {
                drawers.retain(|d| d.id != self.head);
            } else if let Some(d) = drawers.iter_mut().find(|d| d.id == self.head) {
                d.set_content("rewritten while the split was planned");
            }
        }
        self.inner.chunking_tokenizer()
    }
}

/// Plan test 5 and Fail-Open (b): the head changes, or goes, between the read
/// and the commit. The split writes nothing and counts the drawer stale.
#[tokio::test]
async fn split_skips_when_head_content_changed_or_head_forgotten() {
    for forget in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let h = handle(dir.path());
        let head = oversized(&h, &words(OVER_CAP)).await;
        let m = Arc::new(Mutating {
            inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
            drawers: Arc::clone(&h.drawers),
            head,
            forget,
            calls: AtomicUsize::new(0),
            arm_at: AtomicUsize::new(usize::MAX),
        });
        // The health check's tokenizer requests are counted once, unarmed;
        // the first request after them is the slice's own plan of the head.
        h.embed_health_with(Some(m.as_ref() as &dyn Embedder));
        let in_health = m.calls.swap(0, Ordering::SeqCst);
        m.arm_at.store(in_health + 1, Ordering::SeqCst);
        let e: Emb = m;
        let report = h.reembed_slice(&e, &bounds()).await;
        assert_eq!(report.stale, 1, "forget={forget}: {report:?}");
        assert!(
            tails(&h, head).is_empty(),
            "forget={forget}: no tail written"
        );
        for k in 2..=PARTS {
            assert!(h.kg.load_drawer(part_id(head, k)).unwrap().is_none());
        }
    }
}

/// Plan test 6a: forgetting the head leaves no part, row or vector behind.
#[tokio::test]
async fn forget_head_removes_every_part() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &chunking()).await;
    assert_split(&h, head);
    let ids: Vec<Uuid> = family(&h, head).iter().map(|d| d.id).collect();
    assert_eq!(h.forget(head).await.unwrap(), ForgetOutcome::Deleted);
    assert!(family(&h, head).is_empty(), "no part is left in the table");
    for id in ids {
        assert!(h.kg.load_drawer(id).unwrap().is_none(), "{id} left in redb");
        assert_eq!(shape(&h, id), None, "{id} left vector rows");
    }
}

/// Plan test 6b: forgetting a part removes that part only.
#[tokio::test]
async fn forget_part_removes_only_that_part() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &chunking()).await;
    assert_split(&h, head);
    let gone = part_id(head, 3);
    assert_eq!(h.forget(gone).await.unwrap(), ForgetOutcome::Deleted);
    let left: Vec<Uuid> = family(&h, head).iter().map(|d| d.id).collect();
    assert_eq!(left.len(), PARTS - 1);
    assert!(!left.contains(&gone) && left.contains(&head));
    assert_eq!(shape(&h, gone), None);
}

/// Delta 3: the cascade journals every part, so the doctor drawer-count check
/// can explain the whole drop.
#[tokio::test]
async fn forget_head_journals_every_part() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &chunking()).await;
    assert_split(&h, head);
    let mut want: Vec<Uuid> = family(&h, head).iter().map(|d| d.id).collect();
    h.forget(head).await.unwrap();
    let journal = read_journal(dir.path()).unwrap();
    let mut got: Vec<Uuid> = journal
        .records
        .iter()
        .filter(|r| r.reason == DeletionReason::UserForget)
        .map(|r| r.drawer_id)
        .collect();
    got.sort();
    want.sort();
    assert_eq!(got, want, "one user_forget record per removed drawer");
}

/// Fail-Open: one part's redb delete fails during the cascade. The head is
/// gone and the caller gets an error, the failed part stays; a retried
/// `forget(head)` reports `NotFound` yet still finishes the cascade.
#[tokio::test]
async fn forget_head_retry_finishes_the_cascade_after_a_part_delete_fails() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &chunking()).await;
    assert_split(&h, head);
    let stuck = part_id(head, 3);
    let store = h.kg.redb_store();
    *store.test_hooks().txn_budget.lock().unwrap() = Some(Duration::from_millis(300));
    *store.test_hooks().after_batch_op.lock().unwrap() =
        Some(Arc::new(move |op: &BatchWriteOp| {
            if let BatchWriteOp::DeleteDrawer(id) = op
                && *id == stuck
            {
                std::thread::sleep(Duration::from_millis(900));
            }
        }));
    assert!(h.forget(head).await.is_err(), "a failed part is surfaced");
    let left: Vec<Uuid> = family(&h, head).iter().map(|d| d.id).collect();
    assert_eq!(left, vec![stuck], "only the failed part is left behind");

    *store.test_hooks().after_batch_op.lock().unwrap() = None;
    *store.test_hooks().txn_budget.lock().unwrap() = None;
    assert_eq!(h.forget(head).await.unwrap(), ForgetOutcome::NotFound);
    assert!(family(&h, head).is_empty(), "the retry removed the part");
    assert!(h.kg.load_drawer(stuck).unwrap().is_none());
}

/// Plan test 7b: every part of an ordinary head carries its TTL, so
/// `purge_expired` takes all of them.
#[tokio::test]
async fn purge_expired_takes_every_part_of_an_ordinary_head() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let expires = chrono::Utc::now() + chrono::Duration::seconds(6);
    let mut d = Drawer::new(Uuid::new_v4(), words(OVER_CAP));
    d.created_at -= chrono::Duration::hours(1);
    d.expires_at = Some(expires);
    let head = d.id;
    h.kg.upsert_drawer(&d).await.unwrap();
    h.add_drawer(d);
    h.reembed_slice(&chunking(), &bounds()).await;
    assert_split(&h, head);
    assert!(
        family(&h, head)
            .iter()
            .all(|d| d.expires_at == Some(expires))
    );
    let wait = (expires - chrono::Utc::now()).to_std().unwrap_or_default();
    tokio::time::sleep(wait + Duration::from_millis(200)).await;
    assert_eq!(h.purge_expired().await.unwrap(), PARTS);
    assert!(family(&h, head).is_empty());
}

/// Plan test 8a: the committed split clears the head's ledger row, so the
/// shrunk head is not backed off for a day.
#[tokio::test]
async fn ledger_row_cleared_after_split() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let head = oversized(&h, &words(OVER_CAP)).await;
    ledger_row(dir.path(), head);
    h.reembed_slice(&chunking(), &bounds()).await;
    assert_split(&h, head);
    assert!(
        !ledger_ids(dir.path()).contains(&head),
        "{:?}",
        ledger_ids(dir.path())
    );
}

/// Plan test 8b: parts of a young head embed despite the two-minute rule.
#[tokio::test]
async fn split_part_younger_than_two_minutes_still_embeds() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let d = Drawer::new(Uuid::new_v4(), words(OVER_CAP));
    let head = d.id;
    h.kg.upsert_drawer(&d).await.unwrap();
    h.add_drawer(d);
    // A legacy row makes the young head short, which ignores the age rule.
    h.vector_store
        .upsert_chunks(head, vec![vec![1.0; DIM]])
        .await
        .unwrap();
    let young = SliceBounds {
        min_age: Duration::from_secs(120),
        ..bounds()
    };
    let e = chunking();
    for _ in 0..3 {
        h.reembed_slice(&e, &young).await;
    }
    assert_split(&h, head);
    for d in family(&h, head) {
        assert!(
            shape(&h, d.id).is_some_and(|(rows, _)| rows == chunks_at(WINDOW, d.content())),
            "part {} embedded",
            d.id
        );
    }
}

/// Plan test 9: after a reopen the split palace is whole and clean.
#[tokio::test]
async fn palace_reopen_after_split_has_clean_alias_audit_and_vector_count_eq_drawer_count() {
    super::embedder::seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("split-reopen"),
        name: "split-reopen".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("split-reopen"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let h = PalaceHandle::open(&palace).unwrap();
    let dim = h.vector_store.dim();
    let e: Emb = Arc::new(MockEmbedder::new(dim).with_chunking(WINDOW));
    let head = oversized(&h, &words(OVER_CAP)).await;
    drive(&h, &e).await;
    assert_split(&h, head);
    h.flush().unwrap();
    drop(h);

    let h = PalaceHandle::open(&palace).unwrap();
    assert_split(&h, head);
    let health = h.embed_health_with(Some(e.as_ref()));
    assert!(health.alias_audit.is_clean(), "{:?}", health.alias_audit);
    assert!(health.missing_vector_ids.is_empty() && health.short_chunk_ids.is_empty());
    let with_vector = h.vector_store.chunk_shapes().unwrap().len();
    assert_eq!(
        with_vector, health.drawer_count,
        "one vector group per drawer"
    );
}

/// Delta 4: the live L1 serves the shrunk head, not the original text.
#[tokio::test]
async fn l1_serves_the_shrunk_head() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let original = words(OVER_CAP);
    let head = oversized(&h, &original).await;
    h.refresh_l1();
    h.reembed_slice(&chunking(), &bounds()).await;
    assert_split(&h, head);
    let l1 = h.l1_drawers.read();
    let served = l1.iter().find(|d| d.id == head).expect("head in L1");
    assert!(
        served.content().len() < original.len(),
        "L1 still serves the original"
    );
    assert_eq!(Some(served.content().to_string()), content_of(&h, head));
}

/// Fail-Open (c): a busy palace stops the slice before anything is written;
/// the ledger row stays and nothing counts as repaired. The next slice splits.
#[tokio::test]
async fn a_lock_timeout_stops_the_split_and_keeps_the_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let original = words(OVER_CAP);
    let head = oversized(&h, &original).await;
    ledger_row(dir.path(), head);
    let tight = SliceBounds {
        lock_wait: Duration::from_millis(100),
        ..bounds()
    };
    let e = chunking();
    let mutex = h.write_mutex_for_test();
    let guard = mutex.lock().await;
    let report = h.reembed_slice(&e, &tight).await;
    drop(guard);
    assert!(
        matches!(report.stop, SliceStop::LockTimeout(_)),
        "a busy palace stops the slice: {report:?}"
    );
    assert_eq!((report.repaired, report.failed), (0, 0), "{report:?}");
    assert_eq!(content_of(&h, head).as_deref(), Some(original.as_str()));
    assert!(
        tails(&h, head).is_empty(),
        "nothing written under a lock timeout"
    );
    assert!(
        ledger_ids(dir.path()).contains(&head),
        "the ledger row stays"
    );

    h.reembed_slice(&e, &bounds()).await;
    assert_split(&h, head);
}

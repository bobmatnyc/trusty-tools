//! Unit and integration tests for the memory recall ranking pipeline.
//!
//! Why: Split from retrieval.rs (issue #633) to keep that file within the
//! 500-line allowlist budget after adding similarity-ranking tests.
//! What: All test items from retrieval.rs plus the new issue-#633 ranking tests.
//! Test: This file IS the tests — run with:
//!   cargo test -p trusty-common --features memory-core retrieval::tests

use super::*;
use crate::memory_core::analytics::RecallLog;
use crate::memory_core::palace::{Drawer, DrawerType, Palace, PalaceId, RoomType};
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore, vector::VectorStore};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

use super::layers::{L1_NO_SIMILARITY_PENALTY, uuid_prefix_eq};

/// Pre-seed the process-wide shared embedder with `MockEmbedder` so no
/// HuggingFace download is attempted. Safe to call multiple times (no-op
/// after the first invocation — `OnceCell` semantics). Issue #850.
fn init_embedder() {
    seed_shared_embedder_with_mock();
}

fn make_handle(dir: &std::path::Path) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    PalaceHandle::new(PalaceId::new("test"), "Test palace".to_string(), vs, kg)
}

#[test]
fn l0_l1_always_present() {
    let dir = tempdir().unwrap();
    let mut handle = make_handle(dir.path());
    let room_id = uuid::Uuid::new_v4();
    let mut d = Drawer::new(room_id, "important fact");
    d.importance = 0.9;
    handle.add_drawer(d);
    handle.refresh_l1();

    let results = retrieve_l0_l1(&handle);
    assert!(results.iter().any(|r| r.layer == 0), "L0 identity missing");
    assert!(results.iter().any(|r| r.layer == 1), "L1 drawer missing");
}

#[tokio::test]
async fn l2_returns_relevant_drawer() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();

    let room_id = uuid::Uuid::new_v4();
    let drawer = Drawer::new(room_id, "Rust is a systems programming language");
    let drawer_id = drawer.id;

    let vecs = embedder
        .embed_batch(std::slice::from_ref(&drawer.content))
        .await
        .unwrap();
    handle
        .vector_store
        .upsert(drawer_id, vecs[0].clone())
        .await
        .unwrap();
    handle.add_drawer(drawer);

    let results = retrieve_l2(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        None,
        5,
    )
    .await
    .unwrap();
    assert!(!results.is_empty(), "L2 should return results");
    assert!(
        uuid_prefix_eq(results[0].drawer.id, drawer_id),
        "Top L2 result should match the upserted drawer (got {:?}, want {:?})",
        results[0].drawer.id,
        drawer_id
    );
    assert_eq!(results[0].layer, 2);
}

/// Why: Issue #3274 — `room_filter` used to be a silent no-op (an empty
/// if-body in `retrieve_l2`), so a caller asking for one room's drawers
/// silently got every room back. Asserts the filter is now enforced.
/// What: Upserts two semantically-similar drawers stamped into different
/// rooms (via the legacy `room_to_uuid` fold — these drawers are never
/// persisted, so the palace has no ROOMS row and `resolve_room_filter_id`
/// falls back to exactly that fold), queries with `room_filter` set to only
/// one of them, and asserts
/// the non-matching drawer never appears in the results while the matching
/// one does.
/// Test: This test itself.
#[tokio::test]
async fn l2_room_filter_excludes_other_rooms() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();

    let backend_room_id = crate::memory_core::room_identity::room_to_uuid(&RoomType::Backend);
    let frontend_room_id = crate::memory_core::room_identity::room_to_uuid(&RoomType::Frontend);

    let backend_drawer = Drawer::new(backend_room_id, "Rust is a systems programming language");
    let backend_id = backend_drawer.id;
    let frontend_drawer = Drawer::new(frontend_room_id, "Rust is a systems programming toolkit");
    let frontend_id = frontend_drawer.id;

    for d in [&backend_drawer, &frontend_drawer] {
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&d.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(d.id, vecs[0].clone())
            .await
            .unwrap();
    }
    handle.add_drawer(backend_drawer);
    handle.add_drawer(frontend_drawer);

    let results = retrieve_l2(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        Some(RoomType::Backend),
        5,
    )
    .await
    .unwrap();

    assert!(
        !results.is_empty(),
        "L2 should return the matching-room drawer"
    );
    assert!(
        results
            .iter()
            .all(|r| uuid_prefix_eq(r.drawer.id, backend_id)),
        "room_filter must exclude drawers from other rooms, got: {:?}",
        results.iter().map(|r| r.drawer.id).collect::<Vec<_>>()
    );
    assert!(
        !results
            .iter()
            .any(|r| uuid_prefix_eq(r.drawer.id, frontend_id)),
        "Frontend-room drawer leaked through a Backend room_filter"
    );
}

/// Why: End-to-end confirmation that `remember` + `recall` round-trip
/// through the embedder and vector store correctly.
/// What: Build a palace handle backed by a tempdir, remember three
/// drawers in distinct rooms, recall on a keyword from one of them, and
/// assert the matching drawer appears in the L2 results.
/// Test: This test itself.
#[tokio::test]
async fn cli_remember_and_recall() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("test"),
        name: "Test".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    let _id = handle
        .remember(
            "Rust async runtime is tokio".into(),
            RoomType::Backend,
            vec!["rust".into()],
            0.7,
        )
        .await
        .unwrap();
    handle
        .remember(
            "React uses a virtual DOM".into(),
            RoomType::Frontend,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let results = recall_with_default_embedder(&handle, "tokio rust async", 5)
        .await
        .unwrap();
    assert!(
        results.iter().any(|r| r.drawer.content.contains("tokio")),
        "expected to recall the tokio drawer; got {results:?}"
    );
}

/// Why: Confirm `forget` removes a drawer from both the in-memory table
/// and the vector store.
/// What: Remember one drawer, forget it, then recall the same keyword and
/// assert the drawer is no longer in the result list.
/// Test: This test itself.
#[tokio::test]
async fn cli_forget_removes_drawer() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("forget-test"),
        name: "Forget".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("forget-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    let id = handle
        .remember(
            "ephemeral fact about Quokkas".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    assert_eq!(handle.forget(id).await.unwrap(), ForgetOutcome::Deleted);

    let results = recall_with_default_embedder(&handle, "Quokkas ephemeral", 5)
        .await
        .unwrap();
    assert!(
        !results.iter().any(|r| r.drawer.id == id),
        "forgotten drawer should not appear in recall results"
    );
}

/// Build a real on-disk palace handle rooted under `dir`.
///
/// Why: the forget-outcome tests need `PalaceHandle::open` (redb + L1 on disk),
/// not the in-memory `make_handle`, because the reopen assertion is the point.
/// What: creates `<dir>/<id>` and opens a handle over it.
/// Test: used by the `forget_*` tests below.
fn open_disk_palace(dir: &std::path::Path, id: &str) -> (Palace, Arc<PalaceHandle>) {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();
    (palace, handle)
}

/// Regression test for issue #5231.
///
/// Why: `forget` returned `Ok(())` for a drawer id that was never stored, so
/// `memory_forget` reported `"deleted"` for a delete that never happened.
/// `drawers.retain` cannot report a miss, and nothing else checked.
/// What: forgets a random UUID against a populated palace and asserts the
/// outcome is `NotFound` and the stored drawer is untouched.
/// Test: this test.
#[tokio::test]
async fn forget_reports_not_found_for_an_unknown_drawer() {
    init_embedder();
    let dir = tempdir().unwrap();
    let (_palace, handle) = open_disk_palace(dir.path(), "forget-miss");

    let kept = handle
        .remember(
            "Numbats eat about twenty thousand termites a day".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let outcome = handle.forget(Uuid::new_v4()).await.unwrap();
    assert_eq!(outcome, ForgetOutcome::NotFound);
    assert!(!outcome.is_deleted());

    assert!(
        handle.drawers.read().iter().any(|d| d.id == kept),
        "a no-op forget must not disturb the drawers that do exist"
    );
}

/// Issue #5231 durability half: `Deleted` has to mean durably deleted.
///
/// Why: `open_with_intent` reloads the drawer table from redb and fills gaps
/// from the L1 snapshot, so a drawer whose metadata row survived a forget comes
/// back on the next open — which would make a reported `"deleted"` a lie one
/// restart later. This is why `forget` now fails rather than warns when the
/// metadata delete fails for a drawer that existed.
/// What: remembers a drawer, forgets it (asserting `Deleted`), drops the
/// handle, reopens the same palace directory, and asserts the drawer is gone
/// from the reloaded table.
/// Test: this test.
#[tokio::test]
async fn forget_reports_deleted_and_the_drawer_stays_gone_after_reopen() {
    init_embedder();
    let dir = tempdir().unwrap();
    let (palace, handle) = open_disk_palace(dir.path(), "forget-durable");

    let id = handle
        .remember(
            "Pangolin scales are made of keratin, like fingernails".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let outcome = handle.forget(id).await.unwrap();
    assert_eq!(outcome, ForgetOutcome::Deleted);
    assert!(!handle.drawers.read().iter().any(|d| d.id == id));
    handle.flush().unwrap();
    drop(handle);

    let reopened = PalaceHandle::open(&palace).unwrap();
    assert!(
        !reopened.drawers.read().iter().any(|d| d.id == id),
        "a drawer reported deleted must not come back on reopen"
    );
    // And forgetting it a second time is now honest about having found nothing.
    assert_eq!(
        reopened.forget(id).await.unwrap(),
        ForgetOutcome::NotFound,
        "second forget of the same id must report not_found"
    );
}

/// Regression test for issue #154: concurrent `remember_with_options`
/// calls on the same palace must not race on the L1 snapshot write.
///
/// Why: Pre-fix, 20 concurrent writers against the same palace produced
/// 30–60% "No such file or directory" failures because multiple writers
/// would write the same `l1_cache.json.tmp` file and the first
/// `rename(tmp -> target)` removed the tmp before the second rename
/// could see it. The per-palace write mutex + per-call tmp naming
/// together eliminate the race.
/// What: Spawns 32 concurrent `remember` tasks on the same handle,
/// waits for all of them, and asserts every single one returned `Ok`.
/// After the burst the drawer table contains all 32 entries.
/// Test: this test.
#[tokio::test]
async fn remember_concurrent_does_not_lose_writes() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("concurrent-test"),
        name: "Concurrent".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("concurrent-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    // Spawn 32 concurrent writers. Each writes a distinct payload that
    // is long enough to pass the default token filter (>=8 tokens).
    let mut tasks = Vec::with_capacity(32);
    for i in 0..32u32 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.remember(
                format!(
                    "concurrent write test payload number {i} with enough \
                     tokens to satisfy the default token filter check"
                ),
                RoomType::General,
                vec!["concurrent".into(), format!("idx-{i}")],
                0.5,
            )
            .await
        }));
    }

    let mut ok = 0usize;
    let mut errs = Vec::new();
    for t in tasks {
        match t.await.expect("task panicked") {
            Ok(_id) => ok += 1,
            Err(e) => errs.push(format!("{e:#}")),
        }
    }
    assert_eq!(
        ok, 32,
        "expected all 32 concurrent remembers to succeed; failures: {errs:?}"
    );

    // Every write should be present in the in-memory drawer table.
    let drawer_count = handle.drawers.read().len();
    assert_eq!(
        drawer_count, 32,
        "expected 32 drawers after concurrent burst, got {drawer_count}"
    );

    // No leaked tmp files from racing renames.
    let leaked: Vec<_> = std::fs::read_dir(&palace.data_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("l1_cache.json") && n.contains(".tmp."))
        .collect();
    assert!(
        leaked.is_empty(),
        "expected no .tmp.* orphans after concurrent saves; found {leaked:?}"
    );
}

/// Why: Confirm the room filter in `list_drawers` actually narrows the
/// returned set to drawers whose deterministic room id matches.
/// What: Remember three drawers in three distinct rooms, list with the
/// Backend filter, and assert exactly one drawer comes back.
/// Test: This test itself.
#[tokio::test]
async fn cli_list_filters_by_room() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("list-test"),
        name: "List".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("list-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    handle
        .remember(
            "backend fact about the test fixture".into(),
            RoomType::Backend,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    handle
        .remember(
            "frontend fact about the test fixture".into(),
            RoomType::Frontend,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    handle
        .remember(
            "docs fact about the test fixture".into(),
            RoomType::Documentation,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let backend_only = handle.list_drawers(Some(RoomType::Backend), None, 10);
    assert_eq!(
        backend_only.len(),
        1,
        "expected exactly 1 backend drawer, got {backend_only:?}"
    );
    assert!(backend_only[0].content.contains("backend"));
}

/// Why: Confirm the recall_log wiring actually fires events end-to-end.
/// What: Build a handle with a `RecallLog`, upsert one drawer, run
/// `recall`, then poll `hit_count` on the spawned logger task until it
/// reports >=1 (with a small bounded retry to allow the spawn to flush).
/// Test: This test itself.
#[tokio::test]
async fn recall_logs_events_when_log_present() {
    init_embedder();
    let dir = tempdir().unwrap();
    let log = Arc::new(RecallLog::open(&dir.path().join("recall.db")).unwrap());
    let mut handle = make_handle(dir.path()).with_recall_log(log.clone());
    let embedder = shared_embedder().await.unwrap();

    let room_id = uuid::Uuid::new_v4();
    let drawer = Drawer::new(room_id, "Rust is a systems programming language");
    let drawer_id = drawer.id;
    let vecs = embedder
        .embed_batch(std::slice::from_ref(&drawer.content))
        .await
        .unwrap();
    handle
        .vector_store
        .upsert(drawer_id, vecs[0].clone())
        .await
        .unwrap();
    handle.add_drawer(drawer);
    handle.refresh_l1();

    let _ = recall(&handle, embedder.as_ref(), "systems programming Rust", 5)
        .await
        .unwrap();

    // The logger task is spawned; poll briefly for it to land at least
    // one event for our drawer.
    let mut hits = 0u64;
    for _ in 0..20 {
        hits = log.hit_count(drawer_id).await.unwrap();
        if hits >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(hits >= 1, "expected at least one logged hit, got {hits}");
}

/// Why: Issue #53 — `PalaceHandle::open` (the production palace-load path
/// used by `PalaceRegistry::open_palace`) must auto-attach a recall log so
/// the MCP daemon and CLI both get analytics for free without having to
/// call `with_recall_log` manually.
/// What: Open a palace from disk and assert `handle.recall_log` is `Some`,
/// and that a recall fires a logged event end-to-end.
/// Test: This test itself.
#[tokio::test]
async fn open_attaches_recall_log_automatically() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("analytics-auto"),
        name: "AnalyticsAuto".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("analytics-auto"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    assert!(
        handle.recall_log.is_some(),
        "PalaceHandle::open must auto-attach a RecallLog (issue #53)"
    );
    // Issue #57 migrated RecallLog from SQLite to redb. The legacy
    // `recall.db` path passed by retrieval.rs is silently rewritten to
    // `recall.redb`; assert the redb file lands on disk after open.
    assert!(
        palace.data_dir.join("recall.redb").exists(),
        "recall.redb must exist on disk after open"
    );

    // End-to-end: remember + recall should produce at least one logged hit.
    let drawer_id = handle
        .remember(
            "the platypus is a monotreme native to eastern Australia".into(),
            RoomType::Research,
            vec!["wildlife".into()],
            0.7,
        )
        .await
        .unwrap();

    let embedder = shared_embedder().await.unwrap();
    let _ = recall(&handle, embedder.as_ref(), "platypus monotreme", 5)
        .await
        .unwrap();

    let log = handle.recall_log.as_ref().unwrap().clone();
    let mut hits = 0u64;
    for _ in 0..20 {
        hits = log.hit_count(drawer_id).await.unwrap();
        if hits >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        hits >= 1,
        "auto-attached recall log must record events; got {hits}"
    );
}

/// Why: After `remember`, L2 tag-boosting depends on the closet index being
/// up-to-date without waiting for a dream cycle.
/// What: Remember a drawer with a distinctive keyword, then read the closet
/// map and assert the keyword maps to the drawer's id.
/// Test: This test itself.
#[tokio::test]
async fn closet_updated_after_remember() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("closet-test"),
        name: "Closet".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("closet-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    let id = handle
        .remember(
            "Quokkas are happy marsupials".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let closets = handle.closets.read();
    let entry = closets
        .get("quokkas")
        .expect("expected `quokkas` keyword in closet index");
    assert!(
        entry.contains(&id),
        "closet entry for `quokkas` should contain the new drawer id"
    );
}

/// Why: Query expansion must inject the right synonyms when speed/vector
/// triggers fire so the embedder is steered toward technical phrasing.
/// What: Call `expand_query` with the q5 benchmark question and assert the
/// expanded string contains the expected synonym tokens.
/// Test: This test itself.
#[test]
fn expand_query_adds_synonyms() {
    let out = expand_query("how fast is vector search?");
    assert!(out.contains("HNSW"), "expected HNSW synonym, got: {out}");
    assert!(
        out.contains("latency"),
        "expected latency synonym, got: {out}"
    );
}

/// Why: Borrow/ownership queries should still expand, but unmatched topics
/// must remain unchanged so unrelated queries aren't polluted.
/// What: Verify the borrow trigger fires (and adds Rust terms), and that a
/// query with no triggers comes back identical.
/// Test: This test itself.
#[test]
fn expand_query_noop_for_unmatched() {
    let out = expand_query("what is a borrow checker?");
    assert!(
        out.contains("borrow checker"),
        "expected original query preserved, got: {out}"
    );
    assert!(
        out.contains("ownership") || out.contains("lifetime"),
        "expected ownership/lifetime synonyms, got: {out}"
    );

    let untouched = expand_query("what colour is the sky on Tuesday");
    assert_eq!(
        untouched, "what colour is the sky on Tuesday",
        "queries with no triggers must pass through unchanged"
    );
}

/// Why: Regression test for issue #32 — after a cold restart, L2/L3 must
/// still resolve vector hits to drawers beyond the top-15 L1 snapshot.
/// What: Remember 20 drawers, drop the handle, reopen the palace from the
/// same data_dir, and recall a keyword from a drawer that is NOT in the
/// top-15 by importance. The drawer must still come back.
/// Test: Requires real ONNX embedder for semantic similarity; mark `--include-ignored`
/// to run locally. Skipped in CI to avoid HuggingFace download / 429 flake (issue #850).
#[ignore = "requires real ONNX embedder (issue #850)"]
#[tokio::test]
async fn cold_restart_recalls_beyond_l1_snapshot() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("cold-restart"),
        name: "Cold".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("cold-restart"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();

    // Use a separate scope so the first handle (and its Arc-wrapped
    // vector store) is fully dropped before we reopen.
    let needle_id = {
        let handle = PalaceHandle::open(&palace).unwrap();
        // 19 high-importance filler drawers (importance 0.9) — these will
        // dominate the top-15 L1 snapshot.
        for i in 0..19 {
            handle
                .remember(
                    format!("filler drawer number {i} about generic topics"),
                    RoomType::General,
                    vec![],
                    0.9,
                )
                .await
                .unwrap();
        }
        // The needle: low importance so it cannot be in the L1 top-15,
        // distinctive vocabulary so the query lands on it.
        handle
            .remember(
                "the pangolin is a scaly nocturnal mammal".into(),
                RoomType::Research,
                vec![],
                0.1,
            )
            .await
            .unwrap()
    };

    // Reopen the palace — simulating a cold restart.
    let handle2 = PalaceHandle::open(&palace).unwrap();

    // Drawer table should be fully hydrated, not just the 15-entry L1.
    let count = handle2.drawers.read().len();
    assert!(
        count >= 20,
        "expected >=20 drawers after cold reopen, got {count}"
    );

    let results = recall_with_default_embedder(&handle2, "pangolin scaly mammal", 10)
        .await
        .unwrap();
    assert!(
        results.iter().any(|r| r.drawer.id == needle_id),
        "low-importance drawer beyond L1 must still be recallable after cold restart; got {results:?}"
    );
}

/// Why: Issue #57 — at most one FastEmbedder must exist process-wide.
/// `shared_embedder` must return the same `Arc` on every call so callers
/// transitively share one ONNX session.
/// What: Call `shared_embedder` twice and assert the `Arc` pointers are
/// identical via `Arc::ptr_eq`.
/// Test: This test itself.
#[tokio::test]
async fn shared_embedder_is_singleton() {
    init_embedder();
    let a = shared_embedder().await.unwrap();
    let b = shared_embedder().await.unwrap();
    assert!(
        Arc::ptr_eq(&a, &b),
        "shared_embedder must return the same Arc on every call"
    );
}

/// Why (#4836): callers need to ask "is the embedder live?" without triggering a
/// cold init. The recall handlers use exactly this to avoid serving a
/// query-independent fallback while a stale readiness latch says `Warming`, so
/// the accessor must report the cell's real state rather than a cached guess.
/// What: seeds the shared cell, then asserts the accessor reports initialised.
/// (The pre-seed state is not asserted: this static is process-wide and other
/// tests in this binary may legitimately have initialised it first.)
/// Test: This test itself.
#[test]
fn shared_embedder_initialized_flips_after_seeding() {
    seed_shared_embedder_with_mock();
    assert!(
        shared_embedder_initialized(),
        "the shared embedder must report initialised once seeded"
    );
}

/// Why: Closet tag boost should raise a tagged drawer's rank above an
/// untagged but otherwise-similar drawer.
/// What: Insert two drawers — one whose content shares keywords with the
/// query, one that doesn't — and assert the keyword-matched drawer ranks
/// first in L2 results.
/// Test: This test itself.
#[tokio::test]
async fn retrieve_l2_tag_boost_raises_rank() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("boost-test"),
        name: "Boost".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("boost-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    // Drawer A: contains keywords "vector" and "search" and "performance".
    let id_tagged = handle
        .remember(
            "Vector search performance benchmarks show low latency".into(),
            RoomType::Backend,
            vec!["vector-search".into()],
            0.5,
        )
        .await
        .unwrap();
    // Drawer B: unrelated topic, no shared keywords.
    let _id_other = handle
        .remember(
            "React components render through a virtual DOM".into(),
            RoomType::Frontend,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let embedder = shared_embedder().await.unwrap();
    let results = retrieve_l2(
        &handle,
        embedder.as_ref(),
        "vector search performance",
        None,
        5,
    )
    .await
    .unwrap();

    assert!(!results.is_empty(), "L2 should return results");
    assert!(
        uuid_prefix_eq(results[0].drawer.id, id_tagged),
        "tagged drawer should rank first; got {:?}",
        results[0].drawer.content
    );
}

/// Why: Cross-palace recall is the foundation of `memory_recall_all` —
/// agents need to fan a query across every palace and merge the hits.
/// Without this test a regression in the merge/dedup/rerank logic could
/// silently return a single palace's results or drop palace_id tagging.
/// What: Build two disk-backed palaces with distinct distinctive drawers,
/// run `recall_across_palaces_with_default_embedder`, and assert at least
/// one result from each palace appears in the merged output sorted by
/// score descending.
/// Test: This test itself.
#[tokio::test]
async fn recall_across_palaces_merges_results() {
    init_embedder();
    let dir = tempdir().unwrap();

    let palace_a = Palace {
        id: PalaceId::new("alpha"),
        name: "Alpha".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("alpha"),
    };
    std::fs::create_dir_all(&palace_a.data_dir).unwrap();
    let handle_a = PalaceHandle::open(&palace_a).unwrap();
    handle_a
        .remember(
            "the pangolin is a scaly nocturnal mammal".into(),
            RoomType::Research,
            vec![],
            0.6,
        )
        .await
        .unwrap();

    let palace_b = Palace {
        id: PalaceId::new("beta"),
        name: "Beta".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("beta"),
    };
    std::fs::create_dir_all(&palace_b.data_dir).unwrap();
    let handle_b = PalaceHandle::open(&palace_b).unwrap();
    handle_b
        .remember(
            "the platypus is a venomous monotreme".into(),
            RoomType::Research,
            vec![],
            0.6,
        )
        .await
        .unwrap();

    let handles = vec![handle_a, handle_b];
    let results = recall_across_palaces_with_default_embedder(
        &handles,
        "pangolin platypus mammal",
        10,
        false,
    )
    .await
    .unwrap();

    assert!(!results.is_empty(), "expected merged results, got none");
    assert!(
        results.iter().any(|r| r.palace_id == "alpha"),
        "expected at least one alpha result; got {:?}",
        results.iter().map(|r| &r.palace_id).collect::<Vec<_>>()
    );
    assert!(
        results.iter().any(|r| r.palace_id == "beta"),
        "expected at least one beta result; got {:?}",
        results.iter().map(|r| &r.palace_id).collect::<Vec<_>>()
    );

    // Sorted by score descending.
    for w in results.windows(2) {
        assert!(
            w[0].result.score >= w[1].result.score,
            "results not sorted: {} < {}",
            w[0].result.score,
            w[1].result.score
        );
    }
}

/// Issue #61: short content must be rejected with an actionable error.
#[tokio::test]
async fn remember_rejects_short_content() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let err = handle
        .remember("too short".to_string(), RoomType::General, vec![], 0.5)
        .await
        .expect_err("should reject");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("too short")
            || msg.contains("memory_note")
            || msg.contains("tokens"),
        "expected actionable error, got: {msg}"
    );
}

/// Issue #61: known noise patterns must be rejected even when long.
#[tokio::test]
async fn remember_rejects_known_noise_patterns() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let cases = [
        "Tool use: search_files with query parameter very_long_string_here",
        "feat(memory): add filter for noise patterns to reduce drawer clutter",
        "Running cargo test --workspace --all-features for the entire monorepo...",
    ];
    for c in cases {
        let err = handle
            .remember(c.to_string(), RoomType::General, vec![], 0.5)
            .await
            .expect_err("should reject");
        assert!(
            format!("{err:#}").to_lowercase().contains("noise")
                || format!("{err:#}").to_lowercase().contains("low-signal"),
            "expected noise-pattern reject for: {c}",
        );
    }
}

/// Issue #61: `force = true` bypasses every filter.
#[tokio::test]
async fn remember_force_bypasses_filter() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = handle
        .remember_with_options(
            "x".to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions::forced(),
        )
        .await
        .expect("force should bypass filter");
    assert_ne!(id, uuid::Uuid::nil());
}

/// Why (issue #2520, two-tier `force` MAJOR fix): `force = true` must bypass
/// the QUALITY gates (noise/short-content/non-alphabetic) but must NEVER
/// silently bypass secret detection — an automated writer (trusty-code's
/// per-turn memory sink) always sets `force: true` and would otherwise
/// persist raw credentials with zero screening.
/// What: asserts a `force: true` write of secret-shaped content is still
/// rejected, with the error naming it as a `PotentialSecret`.
/// Test: itself.
#[tokio::test]
async fn remember_force_still_blocks_secret() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let err = handle
        .remember_with_options(
            "sk-abcdefghijklmnopqrstuvwxyz01234567890123".to_string(), // pragma: allowlist secret
            RoomType::General,
            vec![],
            0.5,
            RememberOptions::forced(),
        )
        .await
        .expect_err("force=true must still reject secret-shaped content");
    assert!(
        format!("{err:#}").to_lowercase().contains("secret"),
        "expected a secret-gate rejection, got: {err:#}"
    );
}

/// Why (issue #2520, two-tier `force` MAJOR fix): the separate
/// `allow_secret_like` opt-in is the ONLY way to bypass the secret gate — a
/// caller that sets both `force` and `allow_secret_like` gets the full
/// bypass (quality gates AND secret gate).
/// What: asserts a write with both flags set successfully stores
/// secret-shaped content that `remember_force_still_blocks_secret` proves is
/// otherwise rejected.
/// Test: itself.
#[tokio::test]
async fn remember_force_and_allow_secret_like_stores_secret_shaped_content() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let mut opts = RememberOptions::forced();
    opts.allow_secret_like = true;
    let id = handle
        .remember_with_options(
            "sk-abcdefghijklmnopqrstuvwxyz01234567890123".to_string(), // pragma: allowlist secret
            RoomType::General,
            vec![],
            0.5,
            opts,
        )
        .await
        .expect("force + allow_secret_like must bypass the secret gate too");
    assert_ne!(id, uuid::Uuid::nil());
}

/// Issue #61: `memory_note` preset accepts short curated facts but
/// classifies them as `UserFact`.
#[tokio::test]
async fn note_options_skip_token_check_but_keep_noise_filter() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = handle
        .remember_with_options(
            "User prefers snake_case".to_string(),
            RoomType::General,
            vec![],
            1.0,
            RememberOptions::note(),
        )
        .await
        .expect("note should accept short curated fact");
    // Copy the field we need out under a tightly-scoped guard so no lock is
    // held across the subsequent `.await` (clippy::await_holding_lock).
    let stored_type = {
        let drawers = handle.drawers.read();
        let stored = drawers.iter().find(|d| d.id == id).expect("present");
        stored.drawer_type
    };
    assert_eq!(stored_type, DrawerType::UserFact);

    // Noise still rejected even in note mode.
    let err = handle
        .remember_with_options(
            "Tool use: x".to_string(),
            RoomType::General,
            vec![],
            1.0,
            RememberOptions::note(),
        )
        .await
        .expect_err("note must still reject noise patterns");
    assert!(format!("{err:#}").to_lowercase().contains("noise"));
}

/// Issue #61: commit-shaped content is classified as `Commit` when
/// passed through with `force` (so the classifier still fires).
#[tokio::test]
async fn remember_classifies_commit_messages() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    // Use force so the noise filter doesn't reject before classify runs.
    let id = handle
        .remember_with_options(
            "feat(scope): non-empty long enough message body here please".to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions::forced(),
        )
        .await
        .expect("forced commit message");
    let drawers = handle.drawers.read();
    let stored = drawers.iter().find(|d| d.id == id).expect("present");
    assert_eq!(stored.drawer_type, DrawerType::Commit);
}

/// Issue #61: TTL sweep only drops drawers whose expires_at is in the
/// past; future / `None` entries survive.
#[tokio::test]
async fn purge_expired_drops_only_past_ttl() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let room_id = uuid::Uuid::new_v4();

    // Expired drawer.
    let mut expired = Drawer::new(room_id, "expired");
    expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
    let expired_id = expired.id;

    // Future-TTL drawer.
    let mut future = Drawer::new(room_id, "future");
    future.expires_at = Some(chrono::Utc::now() + chrono::Duration::days(7));
    let future_id = future.id;

    // Never-expires drawer.
    let permanent = Drawer::new(room_id, "permanent");
    let permanent_id = permanent.id;

    handle.add_drawer(expired);
    handle.add_drawer(future);
    handle.add_drawer(permanent);

    let pruned = handle.purge_expired().await.expect("purge");
    assert_eq!(pruned, 1, "exactly one drawer should be pruned");

    let remaining: Vec<uuid::Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(!remaining.contains(&expired_id));
    assert!(remaining.contains(&future_id));
    assert!(remaining.contains(&permanent_id));
}

/// Regression test for #4885: a drawer that expires mid-session must stop
/// being served WITHOUT reopening the palace.
///
/// Why this is the whole ticket: production opens the palace once as
/// `OpenIntent::Writer` and holds the handle for the daemon's lifetime, so the
/// open-time sweep never runs again. Before the read-time filter, an expired
/// drawer kept being injected until the daemon restarted. The handle here is
/// built once and never reopened, which is exactly the production shape.
///
/// It also covers a second gap: `l1_drawers` is filled from the L1 cache
/// snapshot, and the open-time sweep only retains over the full drawer table —
/// so an expired drawer in that snapshot survived even a reopen.
#[test]
fn expired_l1_drawer_is_excluded_without_reopen() {
    let dir = tempdir().unwrap();
    let mut handle = make_handle(dir.path());
    let room_id = uuid::Uuid::new_v4();

    let mut expired = Drawer::new(room_id, "PR #4818 is in flight at head d3963848");
    expired.importance = 0.95;
    expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    let expired_id = expired.id;

    let mut live = Drawer::new(room_id, "write plainly");
    live.importance = 0.9;
    let live_id = live.id;

    handle.add_drawer(expired);
    handle.add_drawer(live);
    handle.refresh_l1();

    // Both are in L1 — the filter has to run at read time, not at load time.
    assert!(
        handle.l1_drawers.iter().any(|d| d.id == expired_id),
        "precondition: the expired drawer is cached in L1"
    );

    let results = retrieve_l0_l1(&handle);
    let returned: Vec<uuid::Uuid> = results.iter().map(|r| r.drawer.id).collect();
    assert!(
        !returned.contains(&expired_id),
        "an expired drawer must not be served by L0/L1"
    );
    assert!(
        returned.contains(&live_id),
        "an unexpired drawer must still be served"
    );
}

/// #4885: the same gate on the semantic path. The vector index still holds an
/// expired drawer's embedding until a sweep compacts it, so L2 can score a hit
/// against a drawer whose TTL passed after this handle was opened.
#[tokio::test]
async fn expired_drawer_is_excluded_from_l2() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let room_id = uuid::Uuid::new_v4();

    let mut expired = Drawer::new(room_id, "Rust is a systems programming language");
    expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    let expired_id = expired.id;

    let vecs = embedder
        .embed_batch(std::slice::from_ref(&expired.content))
        .await
        .unwrap();
    handle
        .vector_store
        .upsert(expired_id, vecs[0].clone())
        .await
        .unwrap();
    handle.add_drawer(expired);

    let results = retrieve_l2(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        None,
        5,
    )
    .await
    .unwrap();
    assert!(
        !results.iter().any(|r| r.drawer.id == expired_id),
        "L2 must not return a drawer past its TTL, even on a strong vector hit"
    );
}

/// #4885: read-time enforcement FILTERS, it does not delete. The expired
/// drawer stays in the store — reclamation belongs to `purge_expired` and the
/// open-time sweep — so a recall can never fail because a cleanup failed.
#[test]
fn read_time_expiry_filters_without_deleting() {
    let dir = tempdir().unwrap();
    let mut handle = make_handle(dir.path());
    let room_id = uuid::Uuid::new_v4();

    let mut expired = Drawer::new(room_id, "stale point-in-time fact");
    expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    let expired_id = expired.id;
    handle.add_drawer(expired);
    handle.refresh_l1();

    let results = retrieve_l0_l1(&handle);
    assert!(!results.iter().any(|r| r.drawer.id == expired_id));

    assert!(
        handle.drawers.read().iter().any(|d| d.id == expired_id),
        "the read path must leave the drawer in place for the sweep to reclaim"
    );
}

/// Regression test for issue #633: recall must rank by semantic similarity
/// rather than raw importance.
///
/// Why: Before the fix, a high-importance (1.0) but off-topic drawer
/// (stored in L1 with score=1.0) always outranked low-importance on-topic
/// drawers that scored well in the vector search.
/// What: Insert two drawers — one high-importance but semantically unrelated
/// to the query, one low-importance but closely matching the query — run
/// `recall`, and assert the on-topic drawer appears first in the results.
/// Test: Requires real ONNX embedder for semantic similarity; mark `--include-ignored`
/// to run locally. Skipped in CI to avoid HuggingFace download / 429 flake (issue #850).
#[ignore = "requires real ONNX embedder (issue #850)"]
#[tokio::test]
async fn recall_ranks_by_similarity_over_importance() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("similarity-ranking-test"),
        name: "SimilarityRank".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("similarity-ranking-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    // High importance (1.0), but content completely unrelated to the query
    // about "pangolin scaly nocturnal mammal".
    let _high_imp_id = handle
        .remember(
            "concurrent write regression test fixture number one payload here".into(),
            RoomType::General,
            vec![],
            1.0,
        )
        .await
        .unwrap();

    // Low importance (0.1), but content exactly matching the query topic.
    let on_topic_id = handle
        .remember(
            "the pangolin is a scaly nocturnal mammal with protective keratin scales".into(),
            RoomType::Research,
            vec![],
            0.1,
        )
        .await
        .unwrap();

    let embedder = shared_embedder().await.unwrap();
    let results = recall(
        &handle,
        embedder.as_ref(),
        "pangolin scaly nocturnal mammal",
        10,
    )
    .await
    .unwrap();

    assert!(
        !results.is_empty(),
        "recall must return at least one result"
    );

    // Find the rank of the on-topic drawer (skip L0 identity which is always first).
    let on_topic_rank = results
        .iter()
        .enumerate()
        .find(|(_, r)| r.drawer.id == on_topic_id)
        .map(|(i, _)| i);

    assert!(
        on_topic_rank.is_some(),
        "on-topic drawer must appear in recall results"
    );

    // The on-topic drawer must appear in the first few results (ranks 0-2),
    // not buried behind all the high-importance-but-irrelevant L1 entries.
    let rank = on_topic_rank.unwrap();
    assert!(
        rank <= 2,
        "on-topic drawer (importance=0.1) should rank in top-3 for a semantically \
         matching query, but ranked at position {rank}. \
         Results: {:?}",
        results
            .iter()
            .map(|r| format!(
                "[layer={} imp={:.2} score={:.3}] {}",
                r.layer,
                r.drawer.importance,
                r.score,
                &r.drawer.content[..r.drawer.content.len().min(40)]
            ))
            .collect::<Vec<_>>()
    );

    // Verify the result list is sorted by score descending (invariant).
    for w in results.windows(2) {
        assert!(
            w[0].score >= w[1].score,
            "results must be sorted by score descending: {} >= {} failed",
            w[0].score,
            w[1].score
        );
    }
}

/// Unit test for `rescore_l1_by_similarity`: verify that L1 entries with
/// a matching score in the similarity map get upgraded, and those without
/// a match get the penalty-scaled importance.
#[test]
fn rescore_l1_by_similarity_patches_scores() {
    let room_id = uuid::Uuid::new_v4();

    // L0 identity entry (must remain untouched).
    let identity_drawer = Drawer {
        id: Uuid::nil(),
        room_id: Uuid::nil(),
        content: "identity".into(),
        importance: 1.0,
        source_file: None,
        created_at: chrono::Utc::now(),
        tags: Vec::new(),
        last_accessed_at: None,
        access_count: 0,
        drawer_type: crate::memory_core::palace::DrawerType::UserFact,
        expires_at: None,
        completed_at: None,
        fact_key: None,
    };

    // L1 drawer that appears in the similarity map.
    let mut matched = Drawer::new(room_id, "matched drawer");
    matched.importance = 0.9;
    let matched_id = matched.id;

    // L1 drawer NOT in the similarity map (off-topic).
    let mut unmatched = Drawer::new(room_id, "unmatched drawer");
    unmatched.importance = 1.0;

    let mut results = vec![
        RecallResult {
            drawer: identity_drawer,
            score: 1.0,
            layer: 0,
        },
        RecallResult {
            drawer: matched.clone(),
            score: matched.importance, // starts as importance
            layer: 1,
        },
        RecallResult {
            drawer: unmatched.clone(),
            score: unmatched.importance, // starts as importance
            layer: 1,
        },
    ];

    let mut sim_scores = HashMap::new();
    sim_scores.insert(matched_id, 0.75_f32);

    rescore_l1_by_similarity(&mut results, &sim_scores);

    // L0 entry is untouched.
    assert!(
        (results[0].score - 1.0).abs() < 1e-6,
        "L0 identity score must not change"
    );

    // L1 matched entry gets the similarity score.
    assert!(
        (results[1].score - 0.75).abs() < 1e-6,
        "matched L1 entry must get similarity score 0.75, got {}",
        results[1].score
    );

    // L1 unmatched entry gets importance * penalty.
    let expected_penalty = 1.0_f32 * L1_NO_SIMILARITY_PENALTY;
    assert!(
        (results[2].score - expected_penalty).abs() < 1e-6,
        "unmatched L1 entry must get importance * L1_NO_SIMILARITY_PENALTY = {expected_penalty}, got {}",
        results[2].score
    );
}

/// Regression test for issue #877 — recall must never return more than
/// top_k results even when the palace has more L0+L1 entries than top_k.
///
/// Why: `recall` builds L0+L1 (up to L1_CAP+1 = 16 entries) before merging
/// L2 hits. Without a final `.truncate(top_k)` call the caller receives
/// L0+L1+L2 results, which can far exceed the requested top_k (e.g.
/// top_k=5 → 15 results when the palace has 14 L1 drawers plus identity).
/// What: Populate a palace with more drawers than top_k, run recall with a
/// small top_k, and assert `results.len() <= top_k` for both the shallow
/// and deep paths.
/// Test: This test itself.
#[tokio::test]
async fn recall_top_k_caps_result_count() {
    init_embedder();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new("topk-cap-test"),
        name: "TopKCap".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join("topk-cap-test"),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();

    // Insert 20 drawers — enough to populate all 15 L1 slots plus 5 extras,
    // so L0+L1 alone would return 16 entries (identity + 15 L1 drawers).
    for i in 0..20u32 {
        handle
            .remember(
                format!(
                    "test fact number {i} about the recall top-k cap regression \
                     with enough tokens to pass the default filter check"
                ),
                RoomType::General,
                vec![],
                // Varying importance so the L1 snapshot gets different drawers.
                (i as f32) * 0.04 + 0.1,
            )
            .await
            .unwrap();
    }

    let embedder = shared_embedder().await.unwrap();
    let top_k = 5_usize;

    // Shallow recall (L0+L1+L2) must return <= top_k.
    let shallow = recall(&handle, embedder.as_ref(), "test fact recall", top_k)
        .await
        .unwrap();
    assert!(
        shallow.len() <= top_k,
        "shallow recall: expected at most {top_k} results, got {}",
        shallow.len()
    );

    // Deep recall (L0+L1+L3) must also return <= top_k.
    let deep = recall_deep(&handle, embedder.as_ref(), "test fact recall", top_k)
        .await
        .unwrap();
    assert!(
        deep.len() <= top_k,
        "deep recall: expected at most {top_k} results, got {}",
        deep.len()
    );
}

// ── ADR-0027 T7: the room filter reaches L3 and both recall entry points ──

/// Seed one drawer per room into a fresh handle, vectors included.
///
/// Why: every room-filter test below needs two semantically-similar drawers
/// that differ only by room, so the filter — not the ranking — is what decides
/// the outcome.
async fn seed_two_rooms(
    handle: &PalaceHandle,
    embedder: &dyn crate::memory_core::embed::Embedder,
) -> (Uuid, Uuid) {
    // The legacy fold ids: these drawers are never persisted, so the palace
    // has no ROOMS row and `resolve_room_filter_id` falls back to the fold.
    let backend = Drawer::new(
        crate::memory_core::room_identity::room_to_uuid(&RoomType::Backend),
        "Rust is a systems programming language",
    );
    let frontend = Drawer::new(
        crate::memory_core::room_identity::room_to_uuid(&RoomType::Frontend),
        "Rust is a systems programming toolkit",
    );
    let (backend_id, frontend_id) = (backend.id, frontend.id);
    for d in [&backend, &frontend] {
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&d.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(d.id, vecs[0].clone())
            .await
            .unwrap();
    }
    handle.add_drawer(backend);
    handle.add_drawer(frontend);
    (backend_id, frontend_id)
}

/// Why: ADR-0027 T7 — before this, `memory_recall_deep` was the one recall
/// path a room scope could not reach, so a caller narrowing to one room
/// silently got every room back (the invisible-failure class D4.4 rejects).
/// What: two similar drawers in different rooms; an L3 search filtered to one.
/// Test: this test itself.
#[tokio::test]
async fn l3_room_filter_excludes_other_rooms() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (backend_id, frontend_id) = seed_two_rooms(&handle, embedder.as_ref()).await;

    let results = retrieve_l3(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        Some(RoomType::Backend),
        5,
    )
    .await
    .unwrap();

    assert!(!results.is_empty(), "L3 should return the in-room drawer");
    assert!(
        results
            .iter()
            .all(|r| uuid_prefix_eq(r.drawer.id, backend_id)),
        "room_filter must exclude drawers from other rooms, got: {:?}",
        results.iter().map(|r| r.drawer.id).collect::<Vec<_>>()
    );
    assert!(
        !results
            .iter()
            .any(|r| uuid_prefix_eq(r.drawer.id, frontend_id)),
        "Frontend-room drawer leaked through a Backend room_filter"
    );
}

/// Why: `retrieve_l3(None)` must behave exactly as it did before T7 — an
/// added parameter that changed unfiltered results would silently regress
/// every existing `recall_deep` caller.
/// What: a single indexed drawer (so the ANN search is deterministic —
/// asserting that a k-NN query returns BOTH of two near-identical vectors is
/// an approximate-search coin flip, not a filter assertion). `None` must
/// return it; a filter naming a different room must not.
#[tokio::test]
async fn l3_without_a_room_filter_returns_every_room() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();

    let drawer = Drawer::new(
        crate::memory_core::room_identity::room_to_uuid(&RoomType::Frontend),
        "React uses a virtual DOM for rendering",
    );
    let drawer_id = drawer.id;
    let vecs = embedder
        .embed_batch(std::slice::from_ref(&drawer.content))
        .await
        .unwrap();
    handle
        .vector_store
        .upsert(drawer_id, vecs[0].clone())
        .await
        .unwrap();
    handle.add_drawer(drawer);

    let unfiltered = retrieve_l3(&handle, embedder.as_ref(), "virtual DOM React", None, 5)
        .await
        .unwrap();
    assert!(
        unfiltered
            .iter()
            .any(|r| uuid_prefix_eq(r.drawer.id, drawer_id)),
        "an unfiltered L3 must return the drawer regardless of its room"
    );

    let other_room = retrieve_l3(
        &handle,
        embedder.as_ref(),
        "virtual DOM React",
        Some(RoomType::Backend),
        5,
    )
    .await
    .unwrap();
    assert!(
        other_room.is_empty(),
        "a Backend filter must not return a Frontend drawer, got {:?}",
        other_room.iter().map(|r| r.drawer.id).collect::<Vec<_>>()
    );
}

/// Why: T7's user-visible contract is on `memory_recall` /
/// `memory_recall_deep`, so the filter must survive the L0/L1 merge those
/// entry points perform — not just work one layer down.
#[tokio::test]
async fn recall_in_room_scopes_l2_hits() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (_backend_id, frontend_id) = seed_two_rooms(&handle, embedder.as_ref()).await;

    let results = recall_in_room(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        Some(RoomType::Backend),
        10,
    )
    .await
    .unwrap();
    assert!(
        !results
            .iter()
            .any(|r| r.layer == 2 && uuid_prefix_eq(r.drawer.id, frontend_id)),
        "an out-of-room drawer reached a room-scoped recall"
    );
}

/// Symmetric with `recall_in_room_scopes_l2_hits`, over the L3 lane.
#[tokio::test]
async fn recall_deep_in_room_scopes_l3_hits() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (_backend_id, frontend_id) = seed_two_rooms(&handle, embedder.as_ref()).await;

    let results = recall_deep_in_room(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        Some(RoomType::Backend),
        10,
    )
    .await
    .unwrap();
    assert!(
        !results
            .iter()
            .any(|r| r.layer == 3 && uuid_prefix_eq(r.drawer.id, frontend_id)),
        "an out-of-room drawer reached a room-scoped deep recall"
    );
}

// ── ADR-0027 T9: wing scoping ────────────────────────────────────────────
//
// These live here rather than beside `scope.rs` because the wing-scoped read
// paths need a real `PalaceHandle` (vector store + KG), and `make_handle`
// above is that harness. Duplicating it next to `scope.rs` would fork the
// fixture.

use crate::memory_core::room_identity::DEFAULT_WING_ID;
use crate::memory_core::store::rooms::resolve_or_create_room_in_wing_sync;
use crate::memory_core::store::wings::{ensure_default_wing, resolve_or_create_wing_sync};

/// Seed the default wing plus a named one, and return `(wing_id, room_id)`
/// for a room called `label` inside it.
fn wing_with_room(handle: &PalaceHandle, wing: &str, label: &str) -> (Uuid, Uuid) {
    ensure_default_wing(&handle.kg).expect("seed default wing");
    let store = handle.kg.store();
    let (wing_id, _) = resolve_or_create_wing_sync(&store, wing).expect("create wing");
    let room = RoomType::parse(label);
    let room_id = resolve_or_create_room_in_wing_sync(&store, &room, wing_id).expect("create room");
    (wing_id, room_id)
}

#[test]
fn scope_all_matches_everything() {
    // `RecallScope::All` must resolve to NO filter — distinct from a filter
    // that admits nothing. Collapsing the two would turn an unresolvable
    // scope into an unfiltered read.
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    assert!(
        RecallScope::All.allowed_room_ids(&handle.kg).is_none(),
        "All must be 'no filter', not 'empty filter'"
    );
    assert!(scope_admits(&None, Uuid::new_v4()));
}

#[test]
fn wing_scope_is_fail_closed() {
    // A scope boundary that fails open is a leak (#3064's criterion), so an
    // unknown wing admits NOTHING rather than everything.
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    ensure_default_wing(&handle.kg).expect("seed");
    let allowed = RecallScope::Wing(Uuid::from_u128(999)).allowed_room_ids(&handle.kg);
    assert_eq!(allowed, Some(std::collections::HashSet::new()));
    assert!(!scope_admits(&allowed, Uuid::new_v4()));
}

#[test]
fn wing_scope_returns_only_that_wings_drawers() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let (engineer, eng_room) = wing_with_room(&handle, "engineer", "Planning");
    let (pm, pm_room) = wing_with_room(&handle, "pm", "Planning");

    let eng_drawer = Drawer::new(eng_room, "engineer planning note");
    let eng_id = eng_drawer.id;
    let pm_drawer = Drawer::new(pm_room, "pm planning note");
    let pm_id = pm_drawer.id;
    handle.add_drawer(eng_drawer);
    handle.add_drawer(pm_drawer);

    let eng_hits = list_drawers_in_wing(&handle, engineer, None, 50);
    assert_eq!(eng_hits.len(), 1, "engineer wing sees only its own drawer");
    assert_eq!(eng_hits[0].id, eng_id);

    let pm_hits = list_drawers_in_wing(&handle, pm, None, 50);
    assert_eq!(pm_hits.len(), 1);
    assert_eq!(pm_hits[0].id, pm_id);

    // The default wing owns neither of them.
    assert!(list_drawers_in_wing(&handle, DEFAULT_WING_ID, None, 50).is_empty());
}

/// Seed `count` drawers that tie on importance, newest last and highest-id last.
///
/// Why (#4836): reproduces the shape a live palace actually hands to
/// `list_drawers` — one importance value across the whole tag, and a drawer
/// table iterated in UUID order that is uncorrelated with age. Giving the
/// NEWEST drawer the LARGEST id is what makes an importance-only sort strand it
/// at the end of the vector, where `truncate(limit)` drops it.
/// What: ids `1..=count`, `created_at` ascending with the id, every drawer
/// tagged `tag` at importance 0.5 and filed in `room_id`. Returns the newest
/// drawer's id.
fn seed_importance_tie(handle: &PalaceHandle, room_id: Uuid, tag: &str, count: u128) -> Uuid {
    let now = chrono::Utc::now();
    let mut newest = Uuid::nil();
    for i in 0..count {
        let mut drawer = Drawer::new(room_id, format!("tied drawer {i}"));
        drawer.id = Uuid::from_u128(i + 1);
        drawer.importance = 0.5;
        drawer.created_at = now - chrono::Duration::minutes((count - 1 - i) as i64);
        drawer.tags = vec![tag.to_string()];
        newest = drawer.id;
        handle.add_drawer(drawer);
    }
    newest
}

/// #4836: a `limit` must not cut the newest drawer when importance ties.
///
/// Why: this is the measured defect. `memory_list(palace = "trusty-tools",
/// tag = "pre-authorized", limit = 12)` matched 94 drawers and returned the 12
/// with the smallest UUIDs — every one of the nine drawers written that day
/// fell outside the window, including the one the caller was looking for.
/// What: 20 drawers tied at importance 0.5, newest carrying the largest id, and
/// a listing capped at 5. Pre-fix the sort is importance-only and stable, so the
/// returned five are ids 1-5 (the OLDEST five) and this fails.
#[test]
fn list_drawers_keeps_the_newest_drawer_within_an_importance_tie() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let newest = seed_importance_tie(&handle, Uuid::nil(), "pre-authorized", 20);

    let listed = handle.list_drawers(None, Some("pre-authorized".to_string()), 5);

    assert_eq!(listed.len(), 5, "the tag matches 20 drawers, limit is 5");
    assert_eq!(
        listed[0].id, newest,
        "an importance tie must be broken by recency, so the newest drawer leads; got {:?}",
        listed.iter().map(|d| &d.content).collect::<Vec<_>>()
    );
}

/// #4836: the wing-scoped lister must break ties the same way.
///
/// Why: `list_drawers_in_wing` carried its own copy of the importance-only sort,
/// so fixing only `PalaceHandle::list_drawers` would leave the two read paths
/// disagreeing on what a `limit` means.
#[test]
fn list_drawers_in_wing_keeps_the_newest_drawer_within_an_importance_tie() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let (wing, room) = wing_with_room(&handle, "engineer", "Planning");
    let newest = seed_importance_tie(&handle, room, "pre-authorized", 20);

    let listed = list_drawers_in_wing(&handle, wing, Some("pre-authorized".to_string()), 5);

    assert_eq!(listed.len(), 5);
    assert_eq!(
        listed[0].id, newest,
        "the wing lister must order ties by recency too"
    );
}

/// #4836 guard: recency is the TIE-break, never the primary key.
///
/// Why: the cheap way to make the two tests above pass is to sort by
/// `created_at` outright, which would silently demote the curated
/// importance-1.0 facts that L1 and every `memory_list` caller depend on
/// surfacing first. This fails if the fix overshoots that way.
#[test]
fn list_drawers_ranks_importance_above_recency() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    seed_importance_tie(&handle, Uuid::nil(), "pre-authorized", 5);

    // Older than every drawer above, but curated.
    let mut curated = Drawer::new(Uuid::nil(), "curated essential".to_string());
    curated.id = Uuid::from_u128(9_999);
    curated.importance = 1.0;
    curated.created_at = chrono::Utc::now() - chrono::Duration::days(30);
    curated.tags = vec!["pre-authorized".to_string()];
    let curated_id = curated.id;
    handle.add_drawer(curated);

    let listed = handle.list_drawers(None, Some("pre-authorized".to_string()), 3);
    assert_eq!(
        listed[0].id, curated_id,
        "importance still outranks recency; recency only breaks ties"
    );
}

#[test]
fn same_named_rooms_in_two_wings_stay_distinct() {
    // ADR-0027 D2 pattern 3: `engineer/Planning` and `pm/Planning` coexist
    // without `Custom("engineer-planning")` name mangling.
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let (_e, eng_room) = wing_with_room(&handle, "engineer", "Planning");
    let (_p, pm_room) = wing_with_room(&handle, "pm", "Planning");
    assert_ne!(
        eng_room, pm_room,
        "one label in two wings must be two rooms"
    );
}

#[tokio::test]
async fn wing_scoped_recall_returns_only_that_wing() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (engineer, eng_room) = wing_with_room(&handle, "engineer", "Planning");
    let (_pm, pm_room) = wing_with_room(&handle, "pm", "Planning");

    // Deliberately DISTINCT content per wing. Near-identical vectors would
    // make the targeted query's hit depend on approximate-index recall rather
    // than on the wing filter, which is the thing under test here.
    let eng = Drawer::new(eng_room, "Rust is a systems programming language");
    let eng_id = eng.id;
    let pm = Drawer::new(pm_room, "the launch review happens every quarter");
    let pm_id = pm.id;
    for d in [&eng, &pm] {
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&d.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(d.id, vecs[0].clone())
            .await
            .unwrap();
    }
    handle.add_drawer(eng);
    handle.add_drawer(pm);

    let results = retrieve_l2_scoped(
        &handle,
        embedder.as_ref(),
        "systems programming Rust",
        &RecallScope::Wing(engineer),
        5,
    )
    .await
    .unwrap();

    assert!(
        !results.is_empty(),
        "the engineer wing has a matching drawer"
    );
    assert!(
        results.iter().all(|r| uuid_prefix_eq(r.drawer.id, eng_id)),
        "wing scope must exclude the other wing, got: {:?}",
        results.iter().map(|r| r.drawer.id).collect::<Vec<_>>()
    );
    assert!(
        !results.iter().any(|r| uuid_prefix_eq(r.drawer.id, pm_id)),
        "a pm-wing drawer leaked through an engineer-wing scope"
    );
}

#[tokio::test]
async fn unscoped_recall_is_unchanged_by_wings() {
    // The "wing is never required of a caller" guarantee at the read surface:
    // an unscoped L2 over a palace that HAS wings must return exactly what an
    // unfiltered L2 always returned — both drawers, neither hidden.
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (_e, eng_room) = wing_with_room(&handle, "engineer", "Planning");
    let (_p, pm_room) = wing_with_room(&handle, "pm", "Planning");

    let eng = Drawer::new(eng_room, "Rust is a systems programming language");
    let pm = Drawer::new(pm_room, "the launch review happens every quarter");
    let ids = [eng.id, pm.id];
    let pm_id = pm.id;
    for d in [&eng, &pm] {
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&d.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(d.id, vecs[0].clone())
            .await
            .unwrap();
    }
    handle.add_drawer(eng);
    handle.add_drawer(pm);

    // "Nothing is hidden" is asserted on `list_drawers`, which reads the
    // in-memory drawer table exactly. Asserting it on L2 instead would demand
    // 100% recall from an APPROXIMATE HNSW index — a guarantee the vector
    // store does not make and must not be tested for. That mistake made an
    // earlier version of this test flaky.
    let listed = handle.list_drawers(None, None, 50);
    for id in ids {
        assert!(
            listed.iter().any(|d| d.id == id),
            "an unscoped list must still see every wing's drawers"
        );
    }

    // And an unscoped L2 is genuinely un-filtered by wings: a drawer living in
    // a NON-default wing is reachable when no wing is named. One targeted
    // query, one expected hit — no reliance on the index returning everything.
    let results = retrieve_l2(
        &handle,
        embedder.as_ref(),
        "the launch review happens every quarter",
        None,
        5,
    )
    .await
    .unwrap();
    assert!(
        results.iter().any(|r| uuid_prefix_eq(r.drawer.id, pm_id)),
        "an unscoped recall must not filter out a non-default wing's drawer"
    );
}

#[tokio::test]
async fn unscoped_write_still_lands_in_the_default_wing() {
    // The write-side half of the same guarantee: a caller that never mentions
    // a wing gets the default wing and the pre-T9 room id, unchanged.
    // `remember` embeds, so the process-wide shared embedder must be seeded
    // with the mock first (issue #850) — without this the real ONNX embedder
    // can win the `OnceCell` race and change what every other test in this
    // binary embeds with.
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    ensure_default_wing(&handle.kg).expect("seed");
    let id = handle
        .remember(
            "a perfectly ordinary curated fact about the project".to_string(),
            RoomType::Planning,
            vec![],
            0.5,
        )
        .await
        .expect("remember");

    let drawer = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .expect("stored");
    let scoped = crate::memory_core::store::wings::rooms_in_wing(&handle.kg, DEFAULT_WING_ID)
        .expect("scope");
    assert!(
        scoped.contains(&drawer.room_id),
        "a wing-less write must land in the default wing"
    );
    assert!(
        list_drawers_in_wing(&handle, DEFAULT_WING_ID, None, 50)
            .iter()
            .any(|d| d.id == id)
    );
}

#[tokio::test]
async fn wing_scoped_deep_recall_returns_only_that_wing() {
    // ADR-0027 T9: L3 must honour a wing exactly as L2 does. Without this,
    // `recall_deep` would silently ignore a scope — the invisible-failure class
    // the ADR exists to remove. Library-level so the guarantee is pinned where
    // the filter lives, not only through the MCP surface.
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();
    let (engineer, eng_room) = wing_with_room(&handle, "engineer", "Planning");
    let (_pm, pm_room) = wing_with_room(&handle, "pm", "Planning");

    let eng = Drawer::new(eng_room, "Rust is a systems programming language");
    let eng_id = eng.id;
    let pm = Drawer::new(pm_room, "the launch review happens every quarter");
    let pm_id = pm.id;
    for d in [&eng, &pm] {
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&d.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(d.id, vecs[0].clone())
            .await
            .unwrap();
    }
    handle.add_drawer(eng);
    handle.add_drawer(pm);

    let results = retrieve_l3_scoped(
        &handle,
        embedder.as_ref(),
        "Rust systems programming",
        &RecallScope::Wing(engineer),
        5,
    )
    .await
    .unwrap();

    assert!(
        results.iter().any(|r| uuid_prefix_eq(r.drawer.id, eng_id)),
        "engineer-wing deep recall returned no matching drawer"
    );
    assert!(
        !results.iter().any(|r| uuid_prefix_eq(r.drawer.id, pm_id)),
        "a pm-wing drawer leaked through an engineer-wing deep recall"
    );
}

// ── #4904: importance must tilt the ranking, not replace it ─────────────────

/// Build a unit vector at a chosen cosine similarity to `reference`.
///
/// Why (#4904): the ranking defect only shows up when two candidates are close
/// in similarity and far apart in importance — the shape the live palace
/// measured (0.067 similarity spread against a 0.436 importance spread). A
/// hash-based `MockEmbedder` cannot be steered to that shape, so the test
/// synthesises the vectors and inserts them into the store directly.
/// What: normalises `reference`, picks a direction orthogonal to it by
/// Gram-Schmidt against a `seed`-selected sign pattern, and returns
/// `cos * r̂ + sqrt(1 - cos²) * ô`. Varying `seed` spreads the fillers over
/// distinct directions instead of stacking them on one plane.
/// Test: used by `l2_ranks_similar_low_importance_above_less_similar_high_importance`.
fn vec_at_cosine(reference: &[f32], cos: f32, seed: usize) -> Vec<f32> {
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let rn = norm(reference);
    let r: Vec<f32> = reference.iter().map(|x| x / rn).collect();
    // Sign pattern keyed on `seed` — guarantees a non-parallel starting vector
    // for any non-degenerate reference, and a different one per seed.
    let base: Vec<f32> = (0..r.len())
        .map(|i| {
            if (i / (seed + 1)).is_multiple_of(2) {
                1.0
            } else {
                -1.0
            }
        })
        .collect();
    let dot: f32 = base.iter().zip(&r).map(|(s, x)| s * x).sum();
    let mut o: Vec<f32> = base.iter().zip(&r).map(|(s, x)| s - dot * x).collect();
    let on = norm(&o);
    for x in o.iter_mut() {
        *x /= on;
    }
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    r.iter().zip(&o).map(|(a, b)| cos * a + sin * b).collect()
}

/// Fill a handle's vector index with off-topic drawers around `qv`.
///
/// Why (#4904): `hnsw_rs` recall on a two-point graph is not reliable — the
/// pre-existing `dream_cycle_merges_duplicates` failure is the same shape, a
/// top-3 search over two vectors that intermittently returns one. A ranking
/// assertion must not inherit that, so the index is padded to a couple of dozen
/// points before anything is asserted about ordering.
///
/// #5162 corrects what that padding buys. It does NOT make the search
/// exhaustive, as this comment used to claim: measured over 20,000 builds of
/// this fixture, the miss rate is 0.3–0.6% at every pool size from 6 to 42
/// points, because the miss is a connectivity property of the layer-0 graph and
/// not a density one. See [`ranking_fixture_with_both_candidates`] for the
/// measurement and for what the ranking tests do about it.
/// What: inserts `n` drawers at cosines stepping down from 0.60, each on its own
/// orthogonal direction, with mid-range importance so they cannot dominate under
/// either the old or the new formula.
/// Test: used by the two #4904 ranking tests below.
async fn pad_index(handle: &PalaceHandle, qv: &[f32], n: usize) {
    let room_id = Uuid::new_v4();
    for i in 0..n {
        let mut d = Drawer::new(room_id, format!("unrelated filler drawer {i}"));
        d.importance = 0.5;
        handle
            .vector_store
            .upsert(d.id, vec_at_cosine(qv, 0.60 - 0.02 * i as f32, i + 2))
            .await
            .unwrap();
        handle.add_drawer(d);
    }
}

/// Why (#4904): `rank_score` is the one place the three ranking inputs meet, so
/// the tiebreaker property is asserted on it directly rather than inferred from
/// an end-to-end ordering.
/// What: asserts a 0.05-wide similarity gap survives a full-width importance
/// gap, that importance still orders equal-similarity candidates, and that the
/// clamp holds.
/// Test: this is the test.
#[test]
fn rank_score_keeps_importance_a_tiebreaker() {
    use super::layers::rank_score;

    // The measured production shape: similarity differs by 0.05, importance by
    // the full range. Similarity must win.
    let more_similar_less_important = rank_score(0.80, 0.05, 0.0);
    let less_similar_most_important = rank_score(0.75, 1.00, 0.0);
    assert!(
        more_similar_less_important > less_similar_most_important,
        "importance overrode a 0.05 similarity gap: {more_similar_less_important} vs \
         {less_similar_most_important}"
    );

    // With similarity equal, importance still decides — it is a tiebreaker,
    // not a no-op.
    assert!(
        rank_score(0.70, 1.00, 0.0) > rank_score(0.70, 0.05, 0.0),
        "importance stopped breaking ties between equally-similar candidates"
    );

    // The closet boost and the 1.0 clamp are unchanged.
    assert!(rank_score(0.70, 0.5, 0.15) > rank_score(0.70, 0.5, 0.0));
    assert_eq!(rank_score(1.0, 1.0, 0.15), 1.0);
}

/// Why (#4904): the hook injected ~1,211 tokens per prompt while missing
/// curated facts because L2 scored `eff_importance * similarity`. With
/// `eff_importance` spanning 0.436 inside a candidate pool whose similarity
/// spans 0.067, the product ranked by importance and ignored relevance — a
/// 400-drawer self-retrieval probe on the live palace scored recall@1 102/400
/// under that formula against 291/400 for similarity alone.
/// What: two drawers with hand-built vectors 0.05 apart in cosine and at
/// opposite ends of the importance range. Under the old formula the
/// high-importance drawer wins (0.75 × 1.0 = 0.75 beats 0.80 × 0.05 = 0.04);
/// under [`rank_score`] the more similar one does.
/// Test: this is the test.
#[tokio::test]
async fn l2_ranks_similar_low_importance_above_less_similar_high_importance() {
    init_embedder();
    let embedder = shared_embedder().await.unwrap();

    let query = "context budget startup memory migration duplicate deletion";
    let qv = embedder
        .embed_batch(&[query.to_string()])
        .await
        .unwrap()
        .remove(0);

    let (_dir, handle, on_topic_id, loud_id) = ranking_fixture_with_both_candidates(&qv).await;

    let results = retrieve_l2(&handle, embedder.as_ref(), query, None, 5)
        .await
        .unwrap();

    assert_eq!(results.len(), 5, "top_k=5 over a padded index");
    assert!(
        uuid_prefix_eq(results[0].drawer.id, on_topic_id),
        "the more similar drawer must rank first; importance won instead \
         (top={:?} scores={:?})",
        results[0].drawer.id,
        results.iter().map(|r| r.score).collect::<Vec<_>>()
    );
    // #5162: this assertion used to carry no evidence, so the CI failure it
    // produced said only "expected the high-importance drawer second" and the
    // investigation had to reconstruct the scores from scratch. Dump the same
    // components the first assertion does.
    assert!(
        uuid_prefix_eq(results[1].drawer.id, loud_id),
        "expected the high-importance drawer second, got {:?} \
         (order={:?} scores={:?})",
        results[1].drawer.id,
        results.iter().map(|r| r.drawer.id).collect::<Vec<_>>(),
        results.iter().map(|r| r.score).collect::<Vec<_>>()
    );
}

/// Build the #4904 ranking fixture on an index whose candidate pool provably
/// contains both drawers.
///
/// Why (#5162): `hnsw_rs` seeds its level-assignment RNG from OS entropy inside
/// `Hnsw::new`, so every index build is a different random graph, and layer-0
/// neighbour lists are pruned by Navarro's heuristic. Some of those graphs leave
/// the 0.75-cosine drawer unreachable from the descent pivot — `search` walks
/// the pivot's connected component and returns 15–20 of the 26 points, never
/// offering that drawer as a candidate at all. Measured over 20,000 builds of
/// this exact fixture: 85 misses (0.42%), always the runner-up and never the
/// winner, which is precisely the shape of the `main` failure — `results[0]`
/// held, `results[1]` did not. It is not density: the rate is 0.3–0.6% at every
/// pool size from 6 to 42 points, and neither `set_keeping_pruned` (58/20,000)
/// nor `set_extend_candidates` (90/20,000) removes it.
///
/// Whether the approximate index surfaces a candidate is a property of the
/// index, with its own tests. It is not the ranking rule this test is named for,
/// and it must not decide whether that rule looks broken.
/// What: rebuilds the fixture until `vector_store::search` returns a full
/// candidate pool containing both drawers, then hands it back. The retry
/// condition reads the CANDIDATE POOL only, never the ranked output, so it
/// cannot mask a ranking regression: a broken `rank_score` still yields a pool
/// with both drawers on the first attempt and still fails the assertions.
/// Test: `l2_ranks_similar_low_importance_above_less_similar_high_importance`.
async fn ranking_fixture_with_both_candidates(
    qv: &[f32],
) -> (tempfile::TempDir, PalaceHandle, Uuid, Uuid) {
    // At a 0.42% per-build miss rate, 12 attempts leave a 4e-30 chance of
    // exhausting them — the assertion below is a corruption check, not a
    // flake budget.
    const MAX_ATTEMPTS: usize = 12;
    // The pool `retrieve_l2` will see: top_k=5 over-fetches 3x.
    const POOL: usize = 15;

    for _ in 0..MAX_ATTEMPTS {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let room_id = Uuid::new_v4();

        let mut on_topic = Drawer::new(room_id, "the fact the user was actually asking for");
        on_topic.importance = 0.05;
        let on_topic_id = on_topic.id;

        let mut loud = Drawer::new(room_id, "an unrelated but maximally important drawer");
        loud.importance = 1.0;
        let loud_id = loud.id;

        handle
            .vector_store
            .upsert(on_topic_id, vec_at_cosine(qv, 0.80, 0))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(loud_id, vec_at_cosine(qv, 0.75, 1))
            .await
            .unwrap();
        handle.add_drawer(on_topic);
        handle.add_drawer(loud);
        pad_index(&handle, qv, 24).await;

        let pool = handle.vector_store.search(qv, POOL).await.unwrap();
        let has_both = [on_topic_id, loud_id]
            .iter()
            .all(|id| pool.iter().any(|h| uuid_prefix_eq(h.drawer_id, *id)));
        if has_both && pool.len() == POOL {
            return (dir, handle, on_topic_id, loud_id);
        }
    }
    panic!(
        "{MAX_ATTEMPTS} index builds in a row failed to surface both ranking \
         drawers in a {POOL}-candidate pool; at the measured 0.42% miss rate \
         that is not chance — the vector store or the fixture is broken"
    );
}

/// Why (#4904): the three explanations for a fact missing from an injection —
/// absent from the candidate set, present but below the cutoff, never queried —
/// are indistinguishable from the truncated top-k alone. The pre-truncation
/// trace is what separates them, so it must survive refactors of the scoring
/// loop.
/// What: captures `tracing` events on [`RANK_TRACE_TARGET`] across a `top_k=1`
/// L2 call and asserts every candidate the HNSW pool yielded was traced, not
/// just the one that survived truncation.
/// Test: this is the test.
#[tokio::test]
async fn l2_rank_trace_emits_one_event_per_candidate() {
    use std::sync::Mutex;
    use tracing::subscriber::with_default;

    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = shared_embedder().await.unwrap();

    let query = "trace every candidate before truncation";
    let qv = embedder
        .embed_batch(&[query.to_string()])
        .await
        .unwrap()
        .remove(0);
    pad_index(&handle, &qv, 24).await;

    // The pool `retrieve_l2` will see: `top_k=1` over-fetches to 3.
    let pool = handle.vector_store.search(&qv, 3).await.unwrap();
    assert_eq!(
        pool.len(),
        3,
        "expected a 3-wide over-fetched candidate pool"
    );

    #[derive(Clone, Default)]
    struct Counter(Arc<Mutex<usize>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Counter {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == super::layers::RANK_TRACE_TARGET {
                *self.0.lock().unwrap() += 1;
            }
        }
    }

    let counter = Counter::default();
    let seen = counter.0.clone();
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::DEBUG)
            .with(counter);
        // `retrieve_l2` is async; drive it to completion inside the dispatch
        // scope so every event is attributed to this subscriber.
        let fut = retrieve_l2(&handle, embedder.as_ref(), query, None, 1);
        let results = with_default(subscriber, || futures::executor::block_on(fut)).unwrap();
        assert_eq!(results.len(), 1, "top_k=1 must truncate to one result");
    }

    assert_eq!(
        *seen.lock().unwrap(),
        pool.len(),
        "every candidate must be traced, including the ones truncation drops"
    );
}

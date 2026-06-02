//! Unit and integration tests for the memory recall ranking pipeline.
//!
//! Why: Split from retrieval.rs (issue #633) to keep that file within the
//! 500-line allowlist budget after adding similarity-ranking tests.
//! What: All test items from retrieval.rs plus the new issue-#633 ranking tests.
//! Test: This file IS the tests — run with:
//!   cargo test -p trusty-common --features memory-core retrieval::tests

use super::*;
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use tempfile::tempdir;

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
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let embedder = crate::memory_core::embed::FastEmbedder::new()
        .await
        .unwrap();

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

    let results = retrieve_l2(&handle, &embedder, "systems programming Rust", None, 5)
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

/// Why: End-to-end confirmation that `remember` + `recall` round-trip
/// through the embedder and vector store correctly.
/// What: Build a palace handle backed by a tempdir, remember three
/// drawers in distinct rooms, recall on a keyword from one of them, and
/// assert the matching drawer appears in the L2 results.
/// Test: This test itself.
#[tokio::test]
async fn cli_remember_and_recall() {
    use crate::memory_core::palace::Palace;
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
    use crate::memory_core::palace::Palace;
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
    handle.forget(id).await.unwrap();

    let results = recall_with_default_embedder(&handle, "Quokkas ephemeral", 5)
        .await
        .unwrap();
    assert!(
        !results.iter().any(|r| r.drawer.id == id),
        "forgotten drawer should not appear in recall results"
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
    use crate::memory_core::palace::Palace;
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
    use crate::memory_core::palace::Palace;
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
    let dir = tempdir().unwrap();
    let log = Arc::new(RecallLog::open(&dir.path().join("recall.db")).unwrap());
    let mut handle = make_handle(dir.path()).with_recall_log(log.clone());
    let embedder = crate::memory_core::embed::FastEmbedder::new()
        .await
        .unwrap();

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

    let _ = recall(&handle, &embedder, "systems programming Rust", 5)
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
    use crate::memory_core::palace::Palace;
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

    let embedder = crate::memory_core::embed::FastEmbedder::new()
        .await
        .unwrap();
    let _ = recall(&handle, &embedder, "platypus monotreme", 5)
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
    use crate::memory_core::palace::Palace;
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
/// Test: This test itself.
#[tokio::test]
async fn cold_restart_recalls_beyond_l1_snapshot() {
    use crate::memory_core::palace::Palace;
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
    let a = shared_embedder().await.unwrap();
    let b = shared_embedder().await.unwrap();
    assert!(
        Arc::ptr_eq(&a, &b),
        "shared_embedder must return the same Arc on every call"
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
    use crate::memory_core::palace::Palace;
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

    let embedder = crate::memory_core::embed::FastEmbedder::new()
        .await
        .unwrap();
    let results = retrieve_l2(&handle, &embedder, "vector search performance", None, 5)
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
    use crate::memory_core::palace::Palace;
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

/// Issue #61: `memory_note` preset accepts short curated facts but
/// classifies them as `UserFact`.
#[tokio::test]
async fn note_options_skip_token_check_but_keep_noise_filter() {
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

/// Regression test for issue #633: recall must rank by semantic similarity
/// rather than raw importance.
///
/// Why: Before the fix, a high-importance (1.0) but off-topic drawer
/// (stored in L1 with score=1.0) always outranked low-importance on-topic
/// drawers that scored well in the vector search.
/// What: Insert two drawers — one high-importance but semantically unrelated
/// to the query, one low-importance but closely matching the query — run
/// `recall`, and assert the on-topic drawer appears first in the results.
/// Test: This test itself.
#[tokio::test]
async fn recall_ranks_by_similarity_over_importance() {
    use crate::memory_core::palace::Palace;
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

    let embedder = crate::memory_core::embed::FastEmbedder::new()
        .await
        .unwrap();
    let results = recall(&handle, &embedder, "pangolin scaly nocturnal mammal", 10)
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

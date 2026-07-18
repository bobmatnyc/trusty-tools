//! Branch-aware search, corpus-store roundtrip, grep-fallback, archive
//! down-rank, and mode-filter tests for [`CodeIndexer`].
//!
//! Why: split out of the former monolithic `tests.rs` to keep each test
//! file under the 1500-SLOC cap (issue #1195).
//! What: covers branch-boost application/clamping, corpus redb
//! roundtrip/migration/warm-boot, match-reason labels, grep fallback,
//! grep-lane merge, archive down-rank/exclusion, line-number
//! preservation, and Code/Text/Data/All mode filters.
//! Test: this module.
use super::super::*;
use super::*;

// ---- Branch-aware search (issue #122) ----------------------------------

fn make_branch_query(text: &str, files: Vec<String>, boost: f32) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        branch_files: Some(files),
        branch_boost: boost,
        branch: None,
        mode: SearchMode::Code,
        exclude_archived: false,
        stage: None,
        refine_query: None,
    }
}

#[tokio::test]
async fn test_branch_boost_applied_to_matching_chunks() {
    // Why: chunks whose file is in `branch_files` must out-rank otherwise
    // equivalent chunks. We use two files with the same BM25-relevant
    // content so the baseline ranking is a stable tie broken by the boost.
    // What: build a corpus with two chunks ("on-branch" and "off-branch"),
    // run a query with `branch_files=[on-branch path]`, assert the
    // on-branch chunk ranks first and carries `on_branch: true`.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "branch-aware search must return hits");
    let on_branch = results
        .iter()
        .find(|c| c.file == abs("src/on.rs"))
        .expect("on-branch chunk in results");
    let off_branch = results.iter().find(|c| c.file == abs("src/off.rs"));

    assert!(on_branch.on_branch, "on_branch must be true for on.rs");
    if let Some(off) = off_branch {
        assert!(!off.on_branch, "on_branch must be false for off.rs");
        assert!(
            on_branch.score >= off.score,
            "branch boost must make on.rs >= off.rs (got {} vs {})",
            on_branch.score,
            off.score
        );
    }
    assert_eq!(
        results[0].file,
        abs("src/on.rs"),
        "on-branch chunk must rank first; got {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_branch_boost_clamped_to_3x() {
    // Why: callers must not be able to drown out all off-branch results by
    // passing wild multipliers (e.g. 10x). The pipeline must clamp.
    // What: feed a query with `branch_boost = 10.0` and a single on-branch
    // chunk; verify the resolved boost equals 3.0 via `resolve_branch_set`.
    // Test: this test (direct helper) + the integration test above.
    let q = make_branch_query("foo", vec!["src/on.rs".to_string()], 10.0);
    let root = std::path::PathBuf::from("/tmp/test");
    let (set, boost) = super::search::resolve_branch_set(&q, &root);
    assert!(set.is_some(), "branch set must be present");
    assert!(
        (boost - 3.0).abs() < f32::EPSILON,
        "branch_boost=10.0 must clamp to 3.0, got {boost}"
    );

    // Floor: 0.0 must clamp up to 1.0 (no-op).
    let q_low = make_branch_query("foo", vec!["src/on.rs".to_string()], 0.0);
    let (set_low, boost_low) = super::search::resolve_branch_set(&q_low, &root);
    assert!(
        (boost_low - 1.0).abs() < f32::EPSILON,
        "branch_boost=0.0 must clamp to 1.0, got {boost_low}"
    );
    // 1.0 disables boosting → the set is dropped to skip per-chunk work.
    assert!(
        set_low.is_none(),
        "branch_boost=1.0 must drop the set (no-op)"
    );
}

#[tokio::test]
async fn test_on_branch_set_correctly() {
    // Why: every returned chunk must carry an accurate `on_branch` flag so
    // clients can highlight branch work in UI without re-doing the lookup.
    // What: index two chunks, query with branch_files=[one], assert each
    // result's flag matches set membership.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    let on_abs = abs("src/on.rs");
    let off_abs = abs("src/off.rs");
    for c in &results {
        if c.file == on_abs {
            assert!(c.on_branch, "on.rs must be flagged on_branch=true");
        } else if c.file == off_abs {
            assert!(!c.on_branch, "off.rs must be flagged on_branch=false");
        }
    }

    // Normalize leading "./" — branch_files entries with "./src/on.rs" must
    // still match a chunk whose file is "src/on.rs".
    let q2 = make_branch_query("fn authenticate", vec!["./src/on.rs".to_string()], 1.5);
    let results2 = idx.search(&q2).await.unwrap();
    let on2 = results2
        .iter()
        .find(|c| c.file == on_abs)
        .expect("on-branch chunk in results");
    assert!(on2.on_branch, "leading './' must be normalized away");
}

#[tokio::test]
async fn test_no_boost_when_branch_files_absent() {
    // Why: a vanilla query with no branch context must not pay any branch
    // overhead and must report `on_branch: false` on every result.
    // What: run the baseline search query and confirm scores match the
    // pre-#122 behavior (i.e. on_branch is always false, no panic).
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/auth.rs:1:5",
        "src/auth.rs",
        "fn authenticate(user: &str, password: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/render.rs:1:3",
        "src/render.rs",
        "fn render_ui_components() { /* svelte */ }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "fn authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        mode: SearchMode::Code,
        exclude_archived: false,
        stage: None,
        refine_query: None,
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty());
    for c in &results {
        assert!(
            !c.on_branch,
            "on_branch must default to false when no branch context provided"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue #28 — durable redb corpus integration.
// ---------------------------------------------------------------------------

use crate::core::corpus::CorpusStore;

/// Phase 2 + 3: a committed batch must persist to redb, and a fresh indexer
/// pointed at the same redb file must rehydrate the corpus on warm-boot.
#[tokio::test]
async fn test_corpus_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    // Phase 1: index two files into an indexer with a durable corpus.
    {
        let idx = make_indexer_with_corpus(&redb_path);
        idx.index_files_batch(&[
            ("src/auth.rs".into(), "fn authenticate() {}".into()),
            ("src/token.rs".into(), "fn verify_token() {}".into()),
        ])
        .await
        .expect("index batch");
        assert!(idx.chunk_count() >= 2);
    } // indexer (and its CorpusStore Arc) dropped here — simulates shutdown.

    // The redb file must hold the committed chunks.
    {
        let store = CorpusStore::open(&redb_path).unwrap();
        assert!(
            store.chunk_count().unwrap() >= 2,
            "committed batch was not persisted to redb"
        );
    }

    // Phase 2: a fresh indexer warm-boots from the redb corpus — no reindex.
    let restored = make_indexer_with_corpus(&redb_path);
    let n = restored
        .load_chunks_from_redb()
        .await
        .expect("warm-boot from redb");
    assert!(n >= 2, "warm-boot rehydrated {n} chunks, expected >= 2");
    assert_eq!(restored.chunk_count(), n);

    // BM25 must be rebuilt from the rehydrated corpus.
    let bm25 = restored.bm25.read().await;
    let hits = bm25.score_query_all("authenticate", 5);
    drop(bm25);
    assert!(
        !hits.is_empty(),
        "BM25 not rebuilt from redb-restored chunks"
    );
}

/// Phase 3: warm-boot from an empty / missing redb corpus must yield zero
/// chunks (the first-run / post-upgrade fallback that triggers a reindex).
#[tokio::test]
async fn test_corpus_store_warm_boot_empty_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("fresh.redb"));
    let n = idx.load_chunks_from_redb().await.unwrap();
    assert_eq!(n, 0, "empty redb corpus must rehydrate zero chunks");

    // An indexer with no corpus store at all also yields zero (BM25-only).
    let bare = CodeIndexer::new("bare", "/tmp/bare");
    assert_eq!(bare.load_chunks_from_redb().await.unwrap(), 0);
}

/// Phase 2: `remove_file` / `remove_chunk` must evict from the durable redb
/// corpus too — otherwise a warm-boot resurrects the deleted chunks.
#[tokio::test]
async fn test_corpus_store_deletes_on_remove() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    let idx = make_indexer_with_corpus(&redb_path);
    idx.index_files_batch(&[
        ("src/keep.rs".into(), "fn keep_me() {}".into()),
        ("src/drop.rs".into(), "fn drop_me() {}".into()),
    ])
    .await
    .unwrap();
    let before = idx.chunk_count();
    assert!(before >= 2);

    // Remove one file — this must delete its chunks from redb as well.
    idx.remove_file("src/drop.rs").await.unwrap();
    drop(idx);

    // Re-open the redb corpus directly: the dropped file's chunks must be gone.
    // redb is single-process-exclusive, so this store MUST be dropped before
    // the warm-boot indexer below re-opens the same file.
    let chunks = {
        let store = CorpusStore::open(&redb_path).unwrap();
        store.load_all_chunks().unwrap()
    };
    assert!(
        chunks.iter().all(|c| c.file != "src/drop.rs"),
        "removed file's chunks still present in redb after remove_file"
    );
    assert!(
        chunks.iter().any(|c| c.file == "src/keep.rs"),
        "remove_file evicted the wrong file's chunks from redb"
    );

    // Warm-boot a fresh indexer: the removal must survive the restart.
    let restored = make_indexer_with_corpus(&redb_path);
    restored.load_chunks_from_redb().await.unwrap();
    let ids = restored.find_chunk_id("drop.rs", None).await;
    assert!(ids.is_none(), "deleted chunk resurrected on warm-boot");
}

/// Phase 3 migration: a daemon upgraded from a JSON-snapshot build has a
/// populated `chunks.json` and an empty `index.redb`. `migrate_corpus_to_redb`
/// must seed redb so the next restart uses the fast path.
#[tokio::test]
async fn test_corpus_store_migrates_from_json() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("chunks.json");
    let redb_path = dir.path().join("index.redb");

    // Stage a legacy JSON snapshot via an indexer with no corpus store.
    {
        let legacy = make_indexer();
        legacy
            .add_chunk(raw("a", "src/a.rs", "fn legacy_a() {}"))
            .await
            .unwrap();
        legacy
            .add_chunk(raw("b", "src/b.rs", "fn legacy_b() {}"))
            .await
            .unwrap();
        legacy.save_chunks_to_disk(&json_path).await.unwrap();
    }
    assert!(json_path.exists());

    // Warm-boot path: load the JSON snapshot, then migrate it into redb.
    let idx = make_indexer_with_corpus(&redb_path);
    let n = idx.load_chunks_from_disk(&json_path).await.unwrap();
    assert_eq!(n, 2);
    idx.migrate_corpus_to_redb().await;
    drop(idx);

    // The redb corpus must now hold the migrated chunks, so a subsequent
    // restart can skip the JSON file entirely.
    let restored = make_indexer_with_corpus(&redb_path);
    let m = restored.load_chunks_from_redb().await.unwrap();
    assert_eq!(m, 2, "redb corpus was not seeded by the JSON migration");
}

/// Phase 4: `swap_corpus_store` / `take_corpus_store` give the reindex
/// orchestrator the ability to stage a rebuilt corpus in a temp file and then
/// restore the indexer's durable store — without losing the original.
#[tokio::test]
async fn test_corpus_store_swap_and_take() {
    let dir = tempfile::tempdir().unwrap();
    let live_path = dir.path().join("index.redb");
    let tmp_path = dir.path().join("index.redb.tmp");

    let mut idx = make_indexer_with_corpus(&live_path);
    assert!(idx.has_corpus_store());

    // Stage a fresh tmp corpus, capturing the live one it replaced. The prior
    // store's `Arc` is dropped immediately: redb is single-process-exclusive,
    // and `commit_force_corpus_swap` likewise drops the prior handle before
    // the rename. We only assert its path first.
    let staged = Arc::new(CorpusStore::open_fresh(&tmp_path).unwrap());
    let prev = idx.swap_corpus_store(staged).expect("prior store returned");
    assert_eq!(prev.path(), live_path.as_path());
    drop(prev);

    // Commit a batch — it must land in the *staging* file, not the live one.
    idx.index_files_batch(&[("src/new.rs".into(), "fn brand_new() {}".into())])
        .await
        .unwrap();

    // Take the staging store back out so its Arc can be dropped before a
    // rename (mirrors `commit_force_corpus_swap`).
    let staged_back = idx.take_corpus_store().expect("staging store taken");
    assert_eq!(staged_back.path(), tmp_path.as_path());
    assert!(!idx.has_corpus_store());
    assert!(
        staged_back.chunk_count().unwrap() >= 1,
        "batch did not commit to the staged corpus"
    );
    // Drop the staging handle so the live file can be re-opened below.
    drop(staged_back);

    // The original live file must be untouched — it never saw the new batch.
    let live = CorpusStore::open(&live_path).unwrap();
    assert_eq!(
        live.chunk_count().unwrap(),
        0,
        "live corpus was mutated while a staging corpus was swapped in"
    );
}

// ---------------------------------------------------------------------------
// Issue #313 / #2984 (Phase 0) — warm-boot must honor `skip_kg`.
// ---------------------------------------------------------------------------

/// A `skip_kg=true` index must never load the symbol graph at warm-boot,
/// even when a fully-populated graph was already persisted to the redb
/// corpus by an earlier (non-skip_kg) session.
///
/// Why: this is the exact latent bug from #2984 — `IndexHandle::skip_kg`
/// never reached `CodeIndexer`, so `load_or_rebuild_symbol_graph` ran
/// unconditionally at warm-boot and happily loaded (or rebuilt) the graph
/// regardless of the flag, defeating the whole point of setting it.
/// What: builds a graph with real edges via `index_files_batch` (a function
/// calling another function), confirms it built + persisted, drops the
/// indexer, then warm-boots a fresh `CodeIndexer` with `skip_kg = true`
/// against the same redb corpus and asserts the restored graph is empty
/// while the chunk corpus itself (lexical/BM25) is fully restored — proving
/// the skip is scoped to the graph only, per the locked "soft-off, stores
/// untouched" design (the on-disk KG tables in redb are left alone; only the
/// in-memory load is skipped).
/// Test: this IS the test.
#[tokio::test]
async fn skip_kg_true_warm_boot_never_loads_persisted_graph() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    // Phase 1: build + persist a real symbol graph (caller → callee edge).
    {
        let idx = make_indexer_with_corpus(&redb_path);
        idx.index_files_batch(&[
            ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
            ("src/callee.rs".into(), "fn callee() {}".into()),
        ])
        .await
        .expect("index batch");
        let graph = idx.snapshot_symbol_graph().await;
        assert!(
            graph.node_count() > 0,
            "sanity: normal indexing must build a non-empty symbol graph"
        );
    } // indexer (and corpus Arc) dropped — simulates shutdown.

    // Phase 2: warm-boot a fresh indexer with skip_kg=true against the same
    // redb corpus that DOES have a persisted graph.
    let mut restored = make_indexer_with_corpus(&redb_path);
    restored.skip_kg = true;
    let chunk_count = restored
        .load_chunks_from_redb()
        .await
        .expect("warm-boot from redb");
    assert!(
        chunk_count >= 2,
        "skip_kg must not affect chunk/lexical restoration"
    );

    let graph = restored.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_count(),
        0,
        "skip_kg=true must skip loading the persisted symbol graph at warm-boot"
    );
    assert_eq!(
        graph.edge_count(),
        0,
        "skip_kg=true must skip loading persisted graph edges at warm-boot"
    );
}

/// Counterpart of the test above: `skip_kg=false` (the default) must keep
/// loading the persisted graph exactly as before — this fix must not
/// regress the normal warm-boot path.
///
/// Why: proves the guard added to `load_or_rebuild_symbol_graph` is a
/// targeted early-return, not an accidental behavior change for the common
/// case.
/// What: same two-phase setup as the test above, but the warm-boot indexer
/// leaves `skip_kg` at its default `false` and asserts the restored graph
/// is non-empty.
/// Test: this IS the test.
#[tokio::test]
async fn skip_kg_false_warm_boot_still_loads_persisted_graph() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    {
        let idx = make_indexer_with_corpus(&redb_path);
        idx.index_files_batch(&[
            ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
            ("src/callee.rs".into(), "fn callee() {}".into()),
        ])
        .await
        .expect("index batch");
    }

    let restored = make_indexer_with_corpus(&redb_path);
    assert!(!restored.skip_kg, "sanity: skip_kg defaults to false");
    restored
        .load_chunks_from_redb()
        .await
        .expect("warm-boot from redb");

    let graph = restored.snapshot_symbol_graph().await;
    assert!(
        graph.node_count() > 0,
        "skip_kg=false must still load the persisted symbol graph at warm-boot"
    );
}

/// `skip_kg=true` must skip the graph *rebuild* fallback too, not just the
/// persisted-graph load — i.e. the guard must sit before the `corpus.is_none()`
/// branch inside `load_or_rebuild_symbol_graph`, which otherwise falls back
/// to a full from-chunks rebuild (the more expensive of the two paths this
/// bug was about).
///
/// Why: a naive fix might only guard the `Some(corpus)` branch (skip the
/// redb read) while leaving the `None` branch's `rebuild_symbol_graph` call
/// unconditional — still paying the full O(chunks) rebuild cost for
/// corpus-less (legacy/test) indexers.
/// What: plants a chunk directly into the in-memory `chunks` map (bypassing
/// `add_chunk`, which itself triggers its own unconditional rebuild) on a
/// `skip_kg=true` indexer with NO corpus wired, calls
/// `load_or_rebuild_symbol_graph` directly, and asserts the graph stays
/// empty — proving the rebuild branch never ran (it would have picked up
/// the planted chunk otherwise).
/// Test: this IS the test.
#[tokio::test]
async fn skip_kg_true_skips_rebuild_fallback_when_no_corpus_wired() {
    let mut idx = make_indexer();
    idx.skip_kg = true;
    assert!(!idx.has_corpus_store(), "precondition: no corpus wired");

    idx.chunks.write().await.insert(
        "a:1".to_string(),
        raw_with_kind(
            "a:1",
            "src/a.rs",
            "fn a() { b(); }",
            crate::core::chunker::ChunkType::Function,
            Some("a"),
        ),
    );

    idx.load_or_rebuild_symbol_graph().await;

    let graph = idx.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_count(),
        0,
        "skip_kg=true must skip the from-chunks rebuild fallback too, \
         not just the persisted-graph load"
    );
}

// ----- Issue #75 — line numbers, grep fallback, archive downranking ---------

#[test]
fn test_compute_match_reason_fallback_label() {
    // Why: the `(false,false,false)` arm used to return the bare "fallback"
    // string. Issue #75 renamed it to `"fallback:ripgrep"` so grep-fallback
    // hits are clearly labelled in MCP / HTTP output.
    // The producer is now typed (`-> MatchReason`); render with `as_str()` to
    // pin the byte-identical wire labels (issue #2695).
    assert_eq!(
        compute_match_reason(false, false, false).as_str(),
        "fallback:ripgrep"
    );
    assert_eq!(compute_match_reason(true, false, false).as_str(), "vector");
    assert_eq!(compute_match_reason(false, true, false).as_str(), "bm25");
    assert_eq!(compute_match_reason(true, true, false).as_str(), "hybrid");
    assert_eq!(
        compute_match_reason(false, false, true).as_str(),
        "hybrid+kg"
    );
}

#[tokio::test]
async fn test_grep_fallback_returns_substring_hits() {
    // Why: when both primary lanes return nothing, an exact-substring scan
    // over the in-memory corpus should still surface relevant chunks. The
    // hits must carry a score equal to GREP_FALLBACK_SCORE so they sink
    // below any real hit.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn alpha_qwerty_unique() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn beta() {}"))
        .await
        .unwrap();
    let hits = idx.grep_fallback_search("alpha_qwerty_unique", 5).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "a");
    // The score must be tiny — well below any real BM25 / vector hit.
    assert!(hits[0].1 < 0.01, "fallback score should be sub-0.01");
}

#[tokio::test]
async fn test_grep_fallback_treats_query_as_literal() {
    // Why: user input must never be treated as a regex. A query containing
    // regex metacharacters should match literally (or not at all) — never
    // explode into a partial substring match driven by the metachar.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn foo() {} // literal: a.b.c"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn aXbYc() {}"))
        .await
        .unwrap();
    // `.` is a regex metachar. With `regex::escape` it should only match the
    // literal "a.b.c" in chunk `a` — not the wildcard-style "aXbYc" in `b`.
    let hits = idx.grep_fallback_search("a.b.c", 5).await;
    let ids: Vec<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"), "literal match in a missing: {ids:?}");
    assert!(
        !ids.contains(&"b"),
        "wildcard-style match leaked through regex escape"
    );
}

#[test]
fn test_merge_grep_lane_appends_new_ids() {
    // Why: merge_grep_lane must add brand-new ids to the fused list without
    // dropping any of the existing fused entries, and the resulting order
    // must be sorted by score descending.
    use super::search::merge_grep_lane;
    let fused = vec![("a".to_string(), 0.05), ("b".to_string(), 0.04)];
    let grep_lane = vec![("c".to_string(), 0.001)];
    let out = merge_grep_lane(fused, &grep_lane, 0.5, 10);
    let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
    assert!(ids.contains(&"c"));
    // The previously-top entry must still be ranked at index 0.
    assert_eq!(out[0].0, "a");
}

#[tokio::test]
async fn test_archive_downrank_demotes_deprecated_chunks() {
    // Why: chunks whose file path matches an archive keyword (here: "legacy")
    // should be demoted below comparable clean-path chunks via the post-MMR
    // archive pass, and their `archive_reason` field should be populated.
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw(
        "old",
        "src/legacy/auth_old.rs",
        "fn authenticate_user_xyz_old() {}",
    ))
    .await
    .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    // Both should appear — the live one must rank above the archived one,
    // and the archived one must carry `archive_reason`.
    let pos_live = results.iter().position(|c| c.id == "live");
    let pos_old = results.iter().position(|c| c.id == "old");
    assert!(pos_live.is_some(), "live chunk missing from results");
    assert!(pos_old.is_some(), "archived chunk missing from results");
    assert!(
        pos_live.unwrap() < pos_old.unwrap(),
        "live chunk should outrank archived chunk: live={pos_live:?} old={pos_old:?}"
    );
    let old_chunk = results.iter().find(|c| c.id == "old").unwrap();
    assert!(
        old_chunk.archive_reason.is_some(),
        "archived chunk missing archive_reason: {:?}",
        old_chunk
    );
    let reason = old_chunk.archive_reason.as_deref().unwrap();
    assert!(
        reason.starts_with("path:"),
        "expected path-prefix reason, got {reason}"
    );
}

/// Issue #74: `exclude_archived: true` drops archived chunks from the
/// result set entirely instead of downranking them, and the configurable
/// path detection covers the requested directory conventions.
///
/// Why: archive downranking (issue #75) keeps legacy code in the results
/// (sunk in ranking) which is the right default for exploratory queries.
/// Code-navigation callers want archived code gone outright. This test
/// pins the opt-in hard filter and verifies it fires for each of the
/// `_archive/`, `archive/`, `_deprecated/`, `old/`, `.archive/` path
/// conventions named in the issue.
/// What: indexes one live `.rs` chunk plus several archived chunks (one
/// per path convention), runs the same query with `exclude_archived: true`,
/// and asserts only the live chunk survives.
/// Test: this test.
#[tokio::test]
async fn test_exclude_archived_drops_archive_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    // One archived chunk per path convention the issue enumerates. Each
    // contains the query token so it would otherwise rank in the result set.
    for (id, path) in [
        ("a1", "src/_archive/auth.rs"),
        ("a2", "src/archive/auth.rs"),
        ("a3", "src/_deprecated/auth.rs"),
        ("a4", "src/old/auth.rs"),
        ("a5", "src/.archive/auth.rs"),
    ] {
        idx.add_chunk(raw(id, path, "fn authenticate_user_xyz_old() {}"))
            .await
            .unwrap();
    }

    // Baseline: without the flag, archived chunks are present (downranked).
    let downranked = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        downranked.iter().any(|c| c.id.starts_with('a')),
        "pre-condition: archived chunks should be present (downranked) without the flag"
    );

    // With `exclude_archived`, every archived chunk must be gone.
    let filtered = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            exclude_archived: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        filtered.iter().all(|c| c.id == "live"),
        "exclude_archived must drop every archived chunk; got {:?}",
        filtered.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
    assert!(
        filtered.iter().any(|c| c.id == "live"),
        "the live chunk must still be returned"
    );
}

#[tokio::test]
async fn test_archive_downrank_skips_clean_chunks() {
    // Why: a chunk with no archive signals must not receive an
    // `archive_reason`, and its score must be unchanged by the downrank pass.
    let idx = make_indexer();
    idx.add_chunk(raw("clean", "src/main.rs", "fn run_main() {}"))
        .await
        .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "run_main".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let chunk = results.iter().find(|c| c.id == "clean").unwrap();
    assert!(chunk.archive_reason.is_none());
}

#[tokio::test]
async fn test_search_result_preserves_line_numbers() {
    // Why: issue #75 requires every search result to carry start_line and
    // end_line. They are already on RawChunk; this guards against a future
    // regression where the materializer drops them.
    let idx = make_indexer();
    let mut chunk = raw("a", "src/a.rs", "fn alpha_qwerty_unique() {}");
    chunk.start_line = 42;
    chunk.end_line = 50;
    idx.add_chunk(chunk).await.unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "alpha_qwerty_unique".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].start_line, 42);
    assert_eq!(results[0].end_line, 50);
}

// ---- Issue #77 final design: mode-based hard file-type filter ---------

/// Build a mixed corpus across the three file-type buckets so each mode
/// test can assert which slice of the index is admitted.
///
/// Why: the mode-filter contract is about which file types are returned,
/// not about which is ranked highest. Seeding one chunk per bucket with
/// the same query-matching content lets each test verify inclusion /
/// exclusion in isolation.
/// What: registers a source (`.rs`), a prose doc (`.md`), a named doc
/// (`LICENSE` with no extension), a config file (`.toml`), and a data
/// file (`.json`) — all containing the literal token "alpha_qwerty" so
/// every chunk matches the same query.
/// Test: used by every `test_mode_filter_*` test below.
async fn seed_mode_filter_corpus(idx: &CodeIndexer) {
    idx.add_chunk(raw(
        "src:1",
        "src/lib.rs",
        "fn alpha_qwerty() -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "doc:1",
        "docs/intro.md",
        "# alpha_qwerty\nDocumentation about alpha_qwerty.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "named:1",
        "LICENSE",
        "MIT licence text mentioning alpha_qwerty.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "cfg:1",
        "Cargo.toml",
        "[package]\nname = \"alpha_qwerty\"",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "data:1",
        "fixtures/alpha.json",
        "{\"name\": \"alpha_qwerty\"}",
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn test_mode_filter_code_returns_only_source() {
    // Why: code mode (the default) must return strictly source-code
    // extensions for genuinely code-shaped intents. Prose docs, named
    // docs, configs, and data files must be dropped from results
    // entirely — not merely demoted.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    // Issue #2203: query is `where is alpha_qwerty`, classified `Usage`
    // by the `usage_re` pattern (`where is`) — a code-shaped intent NOT
    // in the Code→All upgrade set, so the hard file-type filter still
    // applies. (`Unknown`, e.g. bare `alpha`, is now upgraded/down-ranked
    // instead — see `indexer::tests_unknown_intent`.)
    let q = SearchQuery {
        text: "where is alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let lib_abs = abs("src/lib.rs");
    let license_abs = abs("LICENSE");
    assert!(
        files.contains(&lib_abs.as_str()),
        "code mode must include source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "code mode must exclude .md: {files:?}"
    );
    assert!(
        !files.contains(&license_abs.as_str()),
        "code mode must exclude named docs: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "code mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "code mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_text_returns_only_prose_and_named_docs() {
    // Why: text mode must return only prose extensions and path-based
    // named docs (README*, LICENSE*, CHANGELOG*, …). Source, config,
    // and data files must be excluded.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Text,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let license_abs = abs("LICENSE");
    assert!(
        files.iter().any(|f| f.ends_with(".md")),
        "text mode must include prose: {files:?}"
    );
    assert!(
        files.contains(&license_abs.as_str()),
        "text mode must include named docs without extension: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "text mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "text mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "text mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_data_returns_only_structured_data() {
    // Why: data mode must return only structured-data / config / schema
    // files. Source and prose must be excluded.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Data,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    assert!(
        files.iter().any(|f| f.ends_with(".toml")),
        "data mode must include config: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.ends_with(".json")),
        "data mode must include data files: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "data mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "data mode must exclude prose: {files:?}"
    );
    assert!(
        !files.contains(&abs("LICENSE").as_str()),
        "data mode must exclude named docs: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_all_returns_everything() {
    // Why: `all` mode is the escape hatch — no file-type filter applies.
    // Every seeded chunk must appear in results.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::All,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<String> = results.iter().map(|c| c.file.clone()).collect();
    for expected_rel in &[
        "src/lib.rs",
        "docs/intro.md",
        "LICENSE",
        "Cargo.toml",
        "fixtures/alpha.json",
    ] {
        let expected = abs(expected_rel);
        assert!(
            files.contains(&expected),
            "all mode must include {expected}: {files:?}"
        );
    }
}

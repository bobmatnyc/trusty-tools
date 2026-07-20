//! Path/repo filter tests for issue #3401.
//!
//! Why: the whole point of #3401 is that a `path_prefix` / `repos` filter
//! must be applied during candidate selection — BEFORE `top_k`
//! truncation — in every retrieval lane, so a chunk that genuinely matches
//! the scoped repo/path is never lost just because it ranks poorly against
//! the *unscoped* query. A test that only proves "filtering happens" would
//! pass even for a naive post-hoc filter (`results.retain(...)` after
//! `search()` returns); the tests here specifically construct a candidate
//! that ranks below the global `top_k` cutoff and assert it is STILL
//! returned once the query is scoped to its repo/path — that is the
//! regression a post-filter implementation could not pass.
//!
//! Every chunk id here is set equal to its file path (as production's
//! `chunker::walk::make_chunk_id` guarantees a chunk id always begins with
//! its literal file path) — the vector lane's HNSW admission predicate
//! (`vector_search_scoped`) tests the chunk id, so a fixture with an
//! unrelated short id (as several OTHER tests in this suite use for
//! brevity) would not exercise that code path realistically.
//! What: `test_path_prefix_filter_survives_top_k_truncation` (the core
//! proof), `test_repos_filter_matches_path_segment`, and two composition
//! tests with `exclude_archived` and the branch-boost fields.
//! Test: this module.

use super::*;

/// The core proof for issue #3401.
///
/// Why: constructs a query where the target chunk (`vendor/acme/target.rs`)
/// shares no BM25 tokens with the query text at all (so it can only be
/// found via the vector lane) and is embedded to a much lower cosine
/// similarity than 20 filler chunks under a different path. With
/// `top_k = 3` (`HNSW_OVERSAMPLE = 4` ⇒ an unfiltered internal candidate
/// pool of only ~12), the target is unreachable by raw similarity alone —
/// it ranks dead last among 21 chunks. Scoping the query to
/// `path_prefix: "vendor/"` must still return it: `vector_search_scoped`
/// pushes the predicate into HNSW traversal (`VectorStore::search_filtered`)
/// rather than filtering an already-truncated candidate set.
/// What: (1) asserts the unfiltered baseline never returns the target
/// (precondition — proves it really does rank below the cutoff); (2)
/// asserts the `path_prefix`-scoped query returns it.
/// Test: this test.
#[tokio::test]
async fn test_path_prefix_filter_survives_top_k_truncation() {
    let idx = make_indexer();

    // 20 filler chunks: identical content, heavy token overlap with the
    // query, so every one of them outranks the target on both BM25 and
    // vector similarity. More fillers than HNSW_OVERSAMPLE * top_k (4*3=12)
    // so the target is unreachable within the internal oversample window,
    // not just beyond the final top_k.
    for i in 0..20 {
        let file = format!("src/filler{i}.rs");
        idx.add_chunk(raw(
            &file,
            &file,
            "zephyr_wombat_flux_token handler zephyr_wombat_flux_token",
        ))
        .await
        .unwrap();
    }
    // The target: zero shared BM25 tokens with the query (findable only via
    // the vector lane), under a distinct path prefix.
    let target_file = "vendor/acme/target.rs";
    idx.add_chunk(raw(
        target_file,
        target_file,
        "completely unrelated prose about something else entirely",
    ))
    .await
    .unwrap();

    let base_query = SearchQuery {
        text: "zephyr_wombat_flux_token".to_string(),
        top_k: 3,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };

    // Precondition: unfiltered, the target must NOT be in the results — it
    // genuinely ranks outside the top_k window, not merely outside some
    // arbitrary post-hoc filter.
    let baseline = idx.search(&base_query).await.unwrap();
    assert!(
        baseline.iter().all(|c| c.id != target_file),
        "precondition failed: target must rank below top_k=3 unfiltered; got {:?}",
        baseline.iter().map(|c| &c.id).collect::<Vec<_>>()
    );

    // Scoped: path_prefix must recover the target despite its last-place
    // global rank. This is the property a post-hoc filter cannot satisfy.
    let scoped = idx
        .search(&SearchQuery {
            path_prefix: Some("vendor/".to_string()),
            ..base_query.clone()
        })
        .await
        .unwrap();
    assert!(
        scoped.iter().any(|c| c.id == target_file),
        "path_prefix filter must recover a match ranked below the global \
         top_k cutoff; got {:?}",
        scoped.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    let vendor_prefix = abs("vendor");
    assert!(
        scoped.iter().all(|c| c.file.starts_with(&vendor_prefix)),
        "no filler chunk (outside vendor/) may leak through the filter: {:?}",
        scoped.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
}

/// `repos` filter: same pre-truncation guarantee, keyed on a path segment
/// rather than a prefix.
#[tokio::test]
async fn test_repos_filter_matches_path_segment() {
    let idx = make_indexer();
    for i in 0..20 {
        let file = format!("repos/main-monorepo/src/filler{i}.rs");
        idx.add_chunk(raw(
            &file,
            &file,
            "kilo_bravo_ranger_signal handler kilo_bravo_ranger_signal",
        ))
        .await
        .unwrap();
    }
    let target_file = "repos/other-service/src/target.rs";
    idx.add_chunk(raw(
        target_file,
        target_file,
        "completely unrelated prose about something else entirely",
    ))
    .await
    .unwrap();

    let base_query = SearchQuery {
        text: "kilo_bravo_ranger_signal".to_string(),
        top_k: 3,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let baseline = idx.search(&base_query).await.unwrap();
    assert!(
        baseline.iter().all(|c| c.id != target_file),
        "precondition failed: target must rank below top_k=3 unfiltered"
    );

    let scoped = idx
        .search(&SearchQuery {
            repos: vec!["other-service".to_string()],
            ..base_query
        })
        .await
        .unwrap();
    assert!(
        scoped.iter().any(|c| c.id == target_file),
        "repos filter must recover a match ranked below the global top_k \
         cutoff; got {:?}",
        scoped.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// Composition: `path_prefix` + `exclude_archived` — both must apply
/// (independent hard filters), neither silently disables the other.
#[tokio::test]
async fn test_path_prefix_composes_with_exclude_archived() {
    let idx = make_indexer();
    let live = "vendor/acme/live.rs";
    let archived = "vendor/acme/_archive/old.rs";
    let other_repo = "vendor/other/live.rs";
    idx.add_chunk(raw(live, live, "fn ranger_kilo_delta_signal_live() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw(
        archived,
        archived,
        "fn ranger_kilo_delta_signal_old() {}",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        other_repo,
        other_repo,
        "fn ranger_kilo_delta_signal_other() {}",
    ))
    .await
    .unwrap();

    let results = idx
        .search(&SearchQuery {
            text: "ranger_kilo_delta_signal".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            path_prefix: Some("vendor/acme/".to_string()),
            exclude_archived: true,
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(
        results.iter().any(|c| c.id == live),
        "the live, in-scope chunk must be returned; got {:?}",
        results.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert!(
        results.iter().all(|c| c.id != archived),
        "exclude_archived must still drop archived chunks inside the scoped path"
    );
    assert!(
        results.iter().all(|c| c.id != other_repo),
        "path_prefix must still exclude chunks outside the scoped path"
    );
}

/// Composition: `path_prefix` + branch boost — the filter and the boost are
/// independent passes; a chunk can be both in-scope and branch-boosted.
#[tokio::test]
async fn test_path_prefix_composes_with_branch_boost() {
    let idx = make_indexer();
    let on_branch = "vendor/acme/on_branch.rs";
    let off_branch = "vendor/acme/off_branch.rs";
    let other_repo_on_branch = "vendor/other/on_branch.rs";
    idx.add_chunk(raw(
        on_branch,
        on_branch,
        "fn tango_uniform_signal_alpha() {}",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        off_branch,
        off_branch,
        "fn tango_uniform_signal_alpha() {}",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        other_repo_on_branch,
        other_repo_on_branch,
        "fn tango_uniform_signal_alpha() {}",
    ))
    .await
    .unwrap();

    let results = idx
        .search(&SearchQuery {
            text: "tango_uniform_signal_alpha".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            path_prefix: Some("vendor/acme/".to_string()),
            branch_files: Some(vec![on_branch.to_string()]),
            branch_boost: 3.0,
            ..Default::default()
        })
        .await
        .unwrap();

    // Path filter: only vendor/acme/* chunks are present.
    assert!(
        results.iter().all(|c| c.id != other_repo_on_branch),
        "path_prefix must exclude the other repo's chunk even though it's \
         also on-branch; got {:?}",
        results.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert_eq!(
        results.len(),
        2,
        "both in-scope chunks must be present: {:?}",
        results.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    // Branch boost still applies within the scoped set: on_branch outranks
    // off_branch despite identical content.
    let on_branch_pos = results.iter().position(|c| c.id == on_branch).unwrap();
    let off_branch_pos = results.iter().position(|c| c.id == off_branch).unwrap();
    assert!(
        on_branch_pos < off_branch_pos,
        "branch-boosted chunk must outrank the identical off-branch chunk \
         within the path-scoped result set"
    );
    assert!(results[on_branch_pos].on_branch);
    assert!(!results[off_branch_pos].on_branch);
}

/// `SearchQuery` must reject an unknown field rather than silently ignoring
/// it (issue #3401) — a silently-dropped filter field returns too much data,
/// which is the exact correctness trap the issue's reporter demonstrated
/// (21 candidate field names all returned HTTP 200 with an identical result
/// set before this fix).
#[tokio::test]
async fn test_search_query_rejects_unknown_field() {
    let with_typo = serde_json::json!({
        "text": "foo",
        "path_prefx": "src/", // typo: missing the trailing 'i'
    });
    let err = serde_json::from_value::<SearchQuery>(with_typo)
        .expect_err("a misspelled filter field must be rejected, not silently ignored");
    assert!(
        err.to_string().contains("path_prefx"),
        "error should name the offending field: {err}"
    );

    // Sanity: the correctly-spelled field still deserializes fine.
    let correct = serde_json::json!({
        "text": "foo",
        "path_prefix": "src/",
    });
    let q: SearchQuery =
        serde_json::from_value(correct).expect("correctly-spelled field must deserialize");
    assert_eq!(q.path_prefix.as_deref(), Some("src/"));
}

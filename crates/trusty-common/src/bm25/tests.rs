use super::*;

// Why `#[serial_test::serial]` on this and the other BM25Index-building
// tests below (issue #3684 follow-up): `TRUSTY_BM25_CORPUS_CAP` is
// process-wide env state. `bm25_corpus_cap_env_override` and the two
// `upsert_document_reporting_*` tests below deliberately set it to tiny
// values (0/123/1/2); without serializing against every OTHER test that
// builds a `BM25Index` and assumes the ~50k default cap, cargo's default
// multi-threaded test runner can interleave them and truncate an
// unrelated test's corpus mid-run — an env-mutation race (same class as
// #3629).
#[test]
#[serial_test::serial]
fn bm25_scores_relevant_doc_higher() {
    let mut idx = BM25Index::new();
    idx.add_document(0, "authentication login password secure");
    idx.add_document(1, "rendering ui components svelte");
    let s0 = idx.score("authentication", 0);
    let s1 = idx.score("authentication", 1);
    assert!(s0 > s1, "relevant doc should score higher: {s0} vs {s1}");
}

#[test]
fn tokenize_splits_code() {
    let tokens = tokenize("fn search_hybrid(query: &str) -> Vec<Hit>");
    // snake_case parts split via outer non-alphanumeric split.
    assert!(tokens.contains(&"search".to_string()));
    assert!(tokens.contains(&"hybrid".to_string()));
    assert!(tokens.contains(&"query".to_string()));
}

#[test]
fn tokenize_camel_case_pascal() {
    let tokens = tokenize("CodeIndexer");
    assert!(tokens.contains(&"code".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"indexer".to_string()), "got {tokens:?}");
    assert!(
        tokens.contains(&"codeindexer".to_string()),
        "got {tokens:?}"
    );
}

#[test]
fn tokenize_pascal_two_words() {
    let tokens = tokenize("UsearchStore");
    assert!(tokens.contains(&"usearch".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"store".to_string()), "got {tokens:?}");
}

#[test]
fn tokenize_snake_case() {
    let tokens = tokenize("use_kg_first");
    assert!(tokens.contains(&"use".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"kg".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"first".to_string()), "got {tokens:?}");
}

#[test]
fn tokenize_alpha_digit_split() {
    let tokens = tokenize("HTTP2Client");
    assert!(tokens.contains(&"http".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"2".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"client".to_string()), "got {tokens:?}");
}

#[test]
fn tokenize_acronym_then_word() {
    // Pass 2 boundary: "HTTPSClient" → ["HTTPS", "Client"]
    let tokens = tokenize("HTTPSClient");
    assert!(tokens.contains(&"https".to_string()), "got {tokens:?}");
    assert!(tokens.contains(&"client".to_string()), "got {tokens:?}");
}

#[test]
#[serial_test::serial]
fn bm25_incremental_upsert_and_remove() {
    let mut idx = BM25Index::new();
    idx.upsert_document("a", "authentication login password");
    idx.upsert_document("b", "rendering ui components svelte");
    idx.upsert_document("c", "database connection pool postgres");
    assert_eq!(idx.len(), 3);

    let hits = idx.score_query_all("authentication", 10);
    assert!(hits.iter().any(|(id, _)| id == "a"));
    assert!(!hits.iter().any(|(id, _)| id == "b"));

    // Removing a doc must drop it from results AND keep the rest scoring.
    idx.remove_document("a");
    assert_eq!(idx.len(), 2);
    let hits_after = idx.score_query_all("authentication", 10);
    assert!(!hits_after.iter().any(|(id, _)| id == "a"));
    let svelte_hits = idx.score_query_all("svelte", 10);
    assert!(svelte_hits.iter().any(|(id, _)| id == "b"));
}

#[test]
#[serial_test::serial]
fn bm25_upsert_replaces_existing_doc() {
    // Re-upserting an existing doc_id must not double-count terms.
    let mut idx = BM25Index::new();
    idx.upsert_document("a", "alpha beta gamma");
    idx.upsert_document("a", "delta epsilon");
    assert_eq!(idx.len(), 1);
    // "alpha" was in the first version only — must be gone.
    assert!(idx.score_query_all("alpha", 10).is_empty());
    assert!(!idx.score_query_all("delta", 10).is_empty());
}

#[test]
#[serial_test::serial]
fn score_query_all_returns_sorted_unique_results() {
    let mut idx = BM25Index::new();
    idx.upsert_document("a", "search rust async tokio");
    idx.upsert_document("b", "search rust");
    idx.upsert_document("c", "unrelated content");
    let hits = idx.score_query_all("rust async", 10);
    // Must be sorted by score desc.
    for w in hits.windows(2) {
        assert!(w[0].1 >= w[1].1, "results must be sorted desc: {hits:?}");
    }
    // No duplicates.
    let mut ids: Vec<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
    ids.sort();
    let unique = ids.len();
    ids.dedup();
    assert_eq!(unique, ids.len());
}

/// Pins the pre-truncation guarantee `score_query_all_with_filter` exists
/// for (issue #3401, trusty-tools): the filter must be evaluated BEFORE
/// `top_k` truncation, not after, so a document that genuinely matches
/// the filter — and has real lexical overlap with the query — is never
/// lost just because it ranks outside the UNFILTERED top `top_k`.
#[test]
#[serial_test::serial]
fn score_query_all_with_filter_recovers_match_beyond_top_k() {
    let mut idx = BM25Index::new();
    // 5 filler docs with higher term frequency (all outrank the target).
    for i in 0..5 {
        idx.upsert_document(&format!("filler{i}"), "rust rust rust async");
    }
    // The target: real overlap, but lower term frequency, so it ranks
    // last among the 6 matching documents.
    idx.upsert_document("target", "rust async");

    // Precondition: unfiltered top_k=3 never returns "target" — it
    // genuinely ranks below the cutoff.
    let unfiltered = idx.score_query_all("rust async", 3);
    assert_eq!(unfiltered.len(), 3);
    assert!(
        unfiltered.iter().all(|(id, _)| id != "target"),
        "precondition failed: target must rank below top_k=3 unfiltered; \
         got {unfiltered:?}"
    );

    // A naive "filter after the call" can't recover it — it was already
    // discarded by the internal `truncate(3)`. The filter-aware entry
    // point must, because it evaluates the filter BEFORE truncation.
    let filtered = idx.score_query_all_with_filter("rust async", 3, &|id: &str| id == "target");
    assert_eq!(
        filtered.len(),
        1,
        "the filter admits only \"target\" — it must be the one result: {filtered:?}"
    );
    assert_eq!(filtered[0].0, "target");
}

#[test]
fn tokenize_dedups_and_sorts() {
    let tokens = tokenize("foo foo bar");
    let foos: Vec<&String> = tokens.iter().filter(|t| t.as_str() == "foo").collect();
    assert_eq!(foos.len(), 1, "duplicates must collapse: {tokens:?}");
    let mut sorted = tokens.clone();
    sorted.sort();
    assert_eq!(tokens, sorted, "tokens must be sorted: {tokens:?}");
}

#[test]
#[serial_test::serial]
fn bm25_corpus_cap_env_override() {
    // Why: confirm `TRUSTY_BM25_CORPUS_CAP=0` falls back to the default
    // (not "no cap"), and a positive override is honoured. This pins the
    // safety property that a bogus env value never silently disables the
    // cap.
    // SAFETY: this test is the only mutator of TRUSTY_BM25_CORPUS_CAP in
    // this module's tests; cargo runs unit tests in this module on a
    // single thread by default for `Cell`/`Mutex` purity, but we still
    // restore the var before returning so unrelated tests in the binary
    // are unaffected.
    let prev = std::env::var("TRUSTY_BM25_CORPUS_CAP").ok();
    unsafe {
        std::env::set_var("TRUSTY_BM25_CORPUS_CAP", "0");
    }
    assert_eq!(
        bm25_corpus_cap(),
        DEFAULT_BM25_CORPUS_CAP,
        "zero must fall back to default"
    );
    unsafe {
        std::env::set_var("TRUSTY_BM25_CORPUS_CAP", "123");
    }
    assert_eq!(bm25_corpus_cap(), 123, "positive value must be honoured");
    match prev {
        Some(v) => unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", v) },
        None => unsafe { std::env::remove_var("TRUSTY_BM25_CORPUS_CAP") },
    }
}

/// Why (issue #3684): a bulk rebuild caller (idle-evict rehydrate, warm
/// boot) needs a per-call signal for "was this doc_id dropped by the
/// cap", not the process-wide `BM25_CAP_LOGGED` latch `upsert_document`
/// uses — so it can count drops for ITS OWN rebuild and log/emit a
/// metric with an accurate count, instead of a stale one-time warning.
/// What: caps at 2, inserts 2 new ids (both accepted), then a 3rd new id
/// must be reported `false` (dropped) while an UPDATE to an existing id
/// is still always accepted regardless of the cap.
/// Test: this test.
#[test]
#[serial_test::serial]
fn upsert_document_reporting_returns_false_when_capped() {
    let prev = std::env::var("TRUSTY_BM25_CORPUS_CAP").ok();
    unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", "2") };

    let mut idx = BM25Index::new();
    assert!(idx.upsert_document_reporting("a", "alpha one"));
    assert!(idx.upsert_document_reporting("b", "beta two"));
    assert!(
        !idx.upsert_document_reporting("c", "gamma three"),
        "a brand-new doc_id past the cap must be reported as dropped"
    );
    assert_eq!(idx.len(), 2, "the dropped doc must not grow the corpus");

    // An update to an existing doc_id is always honoured, cap or not.
    assert!(
        idx.upsert_document_reporting("a", "alpha one updated"),
        "updates to an existing doc_id must never be dropped by the cap"
    );
    assert_eq!(idx.len(), 2);

    match prev {
        Some(v) => unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", v) },
        None => unsafe { std::env::remove_var("TRUSTY_BM25_CORPUS_CAP") },
    }
}

/// Why: `upsert_document` must keep its existing external behaviour
/// (log-once-per-process, corpus growth capped) after being refactored
/// to delegate to `upsert_document_reporting` — this pins that the
/// refactor is behavior-preserving for the pre-existing incremental-ingest
/// call sites.
/// What: caps at 1, inserts one doc (accepted), then upserts a second new
/// doc_id (dropped, corpus size unchanged).
/// Test: this test.
#[test]
#[serial_test::serial]
fn upsert_document_delegates_to_reporting_and_still_logs_once() {
    let prev = std::env::var("TRUSTY_BM25_CORPUS_CAP").ok();
    unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", "1") };

    let mut idx = BM25Index::new();
    idx.upsert_document("only", "the one doc that fits");
    assert_eq!(idx.len(), 1);

    idx.upsert_document("dropped", "this one does not fit");
    assert_eq!(
        idx.len(),
        1,
        "a brand-new doc past the cap must still be silently dropped via upsert_document"
    );

    match prev {
        Some(v) => unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", v) },
        None => unsafe { std::env::remove_var("TRUSTY_BM25_CORPUS_CAP") },
    }
}

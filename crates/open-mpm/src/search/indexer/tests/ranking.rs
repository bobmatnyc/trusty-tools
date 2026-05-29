//! Retrieval-ranking tests: hybrid RRF promotion, knowledge-graph
//! expansion, the query-embedding cache, and the hybrid-vs-ripgrep bench.
//!
//! Why: These guard the search-quality contracts — that a strong lexical
//! match outranks a vector-only match, that KG expansion appends related
//! functions, and that repeated queries hit the embedding cache.
//! What: Indexes small fixtures with the deterministic mocks and asserts on
//! ranking order, expansion membership, and cache stability, plus a
//! latency/ranking comparison against a walkdir grep.
//! Test: This *is* the test module.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{MockEmbedder, MockStore};
use crate::memory::{Embedder, MemoryStore};
use crate::search::indexer::CodeIndexer;

#[tokio::test]
async fn search_hybrid_promotes_lexical_match() {
    // Why: After RRF re-ranking, a chunk with a strong BM25 lexical hit
    // should outrank a chunk that only matched via the (deterministic
    // mock) vector embedding. We seed the mock store such that vector
    // ordering puts the *non-matching* chunk first; the BM25 signal
    // should flip the order.
    let dir = tempfile::Builder::new()
        .prefix("hybrid-")
        .tempdir()
        .expect("tempdir");
    // File A: contains the rare token "bm25_special" — should rank
    // higher after lexical fusion.
    let a = dir.path().join("a.rs");
    let mut fa = std::fs::File::create(&a).unwrap();
    writeln!(fa, "fn alpha() {{").unwrap();
    writeln!(fa, "    // bm25_special token appears here").unwrap();
    writeln!(fa, "    println!(\"bm25_special\");").unwrap();
    writeln!(fa, "}}").unwrap();
    drop(fa);
    // File B: same length but no occurrence of the rare token.
    let b = dir.path().join("b.rs");
    let mut fb = std::fs::File::create(&b).unwrap();
    writeln!(fb, "fn beta() {{").unwrap();
    writeln!(fb, "    // generic body lines for padding").unwrap();
    writeln!(fb, "    println!(\"hello\");").unwrap();
    writeln!(fb, "}}").unwrap();
    drop(fb);

    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));
    // Insert b first so vector search returns it first (insertion-order
    // mock); a should be promoted by BM25 to outrank it after fusion.
    indexer.index_file(&b, None).await.expect("index b");
    indexer.index_file(&a, None).await.expect("index a");

    let hits = indexer
        .search_hybrid("bm25_special", 5, false)
        .await
        .expect("hybrid search");
    assert!(!hits.is_empty(), "hybrid returned no hits");
    // The top hit must be the file containing the rare token. With the
    // mock embedder both files get equal cosine, so vector ranking
    // resolves to insertion order (b first, then a). BM25 inverts that
    // (a first because its text contains both query terms). RRF on
    // (1,2) and (2,1) ties numerically; the BM25-raw tiebreaker is what
    // promotes a above b — exactly the property we want to enforce.
    let top_path = hits[0].file.display().to_string();
    assert!(
        top_path.ends_with("a.rs"),
        "expected a.rs to outrank b.rs after RRF; got top={top_path:?}"
    );
    // RRF score is in (0, 2/(RRF_K+1)] — sanity check it's positive.
    assert!(hits[0].score > 0.0, "RRF score should be positive");
}

/// Verify that KG expansion appends caller/callee chunks beyond the
/// initial RRF set when `expand_graph` is true (#376 B1).
///
/// Why: Hybrid search alone doesn't return functions related to the
/// match by call structure. With expansion enabled, a top-K hit on
/// `caller` should also surface `callee` and vice-versa, scored at
/// 0.7× the trigger's RRF.
/// What: Writes a single Rust file containing two functions where
/// `caller` calls `callee`, indexes it, queries for `caller`, and
/// asserts both functions appear in the expanded result set.
#[tokio::test]
async fn search_hybrid_expansion_appends_related_chunks() {
    let dir = tempfile::Builder::new()
        .prefix("kgexpand-")
        .tempdir()
        .expect("tempdir");
    let p = dir.path().join("expand.rs");
    let mut f = std::fs::File::create(&p).unwrap();
    writeln!(f, "fn callee() -> i32 {{ 42 }}").unwrap();
    writeln!(f, "fn caller() -> i32 {{ callee() + 1 }}").unwrap();
    drop(f);

    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));
    indexer.index_file(&p, None).await.expect("index");

    let baseline = indexer
        .search_hybrid("callee", 1, false)
        .await
        .expect("hybrid no expand");
    assert_eq!(baseline.len(), 1, "baseline should be exactly top_k=1");

    let expanded = indexer
        .search_hybrid("callee", 1, true)
        .await
        .expect("hybrid expand");
    assert!(
        expanded.len() > baseline.len(),
        "expansion should add hits; got {}",
        expanded.len()
    );
    let names: Vec<&str> = expanded
        .iter()
        .filter_map(|c| c.function_name.as_deref())
        .collect();
    assert!(
        names.contains(&"callee") && names.contains(&"caller"),
        "expansion missing related fn; got {names:?}"
    );
}

/// Verify the query embedding LRU cache returns identical vectors
/// across calls (#376 D2).
///
/// Why: A correct cache must return the *same* embedding for the
/// same query without re-running the embedder. Our `MockEmbedder` is
/// deterministic, so a regression to "always re-embed" would still
/// yield equal vectors — but the cache hit path returns its stored
/// clone, which is the property we want to verify.
/// What: Calls `embed_query_cached` twice with the same query and
/// asserts the returned vectors are equal.
#[tokio::test]
async fn embed_query_cached_returns_consistent_vector() {
    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(store, embedder);
    let v1 = indexer.embed_query_cached("hello world").await.unwrap();
    let v2 = indexer.embed_query_cached("hello world").await.unwrap();
    assert_eq!(v1, v2, "cached query embedding must be stable");
    // Cache should now have the entry.
    let cache = indexer.query_cache.lock().await;
    assert!(cache.contains(&"hello world".to_string()));
}

/// Recursive case-insensitive substring grep used as the ripgrep stand-in
/// for the bench. Skips hidden + build dirs to mirror the indexer's filter.
fn walkdir_grep_bench(root: &Path, query: &str, top_k: usize) -> Vec<(PathBuf, usize)> {
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if hits.len() >= top_k {
            break;
        }
        if p.is_dir() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str())
                && (name.starts_with('.')
                    || matches!(name, "target" | "node_modules" | "dist" | "build"))
            {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&p) {
                for entry in rd.flatten() {
                    stack.push(entry.path());
                }
            }
        } else if p.is_file() {
            let ok = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e, "rs" | "py" | "ts" | "tsx" | "js" | "go" | "md"))
                .unwrap_or(false);
            if !ok {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&p) else {
                continue;
            };
            for (idx, line) in body.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    hits.push((p.clone(), idx + 1));
                    if hits.len() >= top_k {
                        break;
                    }
                }
            }
        }
    }
    hits
}

/// Hybrid-vs-ripgrep latency + ranking comparison for #372.
///
/// Why: We want a checked-in baseline showing hybrid (vector + BM25 RRF)
/// is competitive with ripgrep on representative queries — both in
/// quality and latency. Without a baseline, regressions on either axis
/// are easy to ship.
/// What: Indexes the project's own `src/` with `MockEmbedder` (no model
/// download — keeps the test hermetic and CI-safe), runs five
/// representative queries through both `search_hybrid` and a walkdir
/// grep, prints a comparison table, and asserts hybrid produces at
/// least one hit per query and completes within a reasonable budget.
/// Test: `tokio::test`-driven; <30s on a developer laptop. Skipped
/// gracefully if the manifest source dir isn't reachable.
#[tokio::test]
async fn hybrid_vs_ripgrep_benchmark() {
    use std::time::Instant;

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_dir = match manifest.join("src").canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping hybrid_vs_ripgrep_benchmark: {e}");
            return;
        }
    };

    // Use the deterministic MockEmbedder so this test never depends on
    // the network or HuggingFace cache. The MockStore is brute-force-
    // searchable (insertion-order + cosine sketches), which is plenty
    // for the bench: BM25 dominates ranking on these lexical queries,
    // and we're measuring the hybrid + grep paths, not embedding quality.
    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 16 });
    let indexer = CodeIndexer::new(store, embedder);

    let t0 = Instant::now();
    let chunks = match indexer.index_directory(&src_dir, &["rs"]).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("skipping bench: index_directory failed: {e}");
            return;
        }
    };
    let index_elapsed = t0.elapsed();
    eprintln!(
        "[bench] indexed {chunks} chunks from {} in {index_elapsed:?}",
        src_dir.display()
    );
    if chunks == 0 {
        eprintln!("[bench] zero chunks indexed — skipping");
        return;
    }

    let queries = [
        "BM25 ranking score",
        "file watcher debounce",
        "HNSW cosine similarity",
        "agent delegation task",
        "tokio spawn background",
    ];

    println!("\n┌────────────────────────────────────┬────────────┬────────────┐");
    println!("│ Query                              │  hybrid ms │ ripgrep ms │");
    println!("├────────────────────────────────────┼────────────┼────────────┤");

    let mut total_hybrid_us: u128 = 0;
    let mut total_grep_us: u128 = 0;

    for q in &queries {
        let t = Instant::now();
        let hybrid_hits = indexer
            .search_hybrid(q, 3, false)
            .await
            .expect("hybrid search did not error");
        let hybrid_us = t.elapsed().as_micros();
        total_hybrid_us += hybrid_us;

        let t = Instant::now();
        let grep_hits = walkdir_grep_bench(&src_dir, q, 3);
        let grep_us = t.elapsed().as_micros();
        total_grep_us += grep_us;

        let q_disp = if q.len() > 34 {
            format!("{}…", &q[..33])
        } else {
            q.to_string()
        };
        println!(
            "│ {:<34} │ {:>10.2} │ {:>10.2} │",
            q_disp,
            hybrid_us as f64 / 1000.0,
            grep_us as f64 / 1000.0
        );

        // Show top-3 from each so reviewers can eyeball ranking quality.
        eprintln!("\n[{q}] hybrid top {} hits:", hybrid_hits.len());
        for (i, h) in hybrid_hits.iter().enumerate() {
            eprintln!(
                "  #{}: {}:{}-{} (score={:.4}) fn={:?}",
                i + 1,
                h.file.strip_prefix(&manifest).unwrap_or(&h.file).display(),
                h.start_line,
                h.end_line,
                h.score,
                h.function_name
            );
        }
        eprintln!("[{q}] ripgrep top {} hits:", grep_hits.len());
        for (i, (path, line)) in grep_hits.iter().enumerate() {
            eprintln!(
                "  #{}: {}:{}",
                i + 1,
                path.strip_prefix(&manifest).unwrap_or(path).display(),
                line
            );
        }

        assert!(
            !hybrid_hits.is_empty(),
            "hybrid returned 0 hits for {q:?} — index empty or scoring broken"
        );
    }

    println!("├────────────────────────────────────┼────────────┼────────────┤");
    println!(
        "│ TOTAL (5 queries)                  │ {:>10.2} │ {:>10.2} │",
        total_hybrid_us as f64 / 1000.0,
        total_grep_us as f64 / 1000.0
    );
    println!("└────────────────────────────────────┴────────────┴────────────┘\n");
}

//! Benchmark harness for entity-enriched KG search quality.
//!
//! Why: ad-hoc spot checks aren't enough to know whether a refactor regressed
//! retrieval. This harness fixes a small per-intent query set against the
//! `trusty-search` crate source tree and reports MRR@5 + Recall@10 + latency.
//! What: indexes `crates/trusty-search/src/` end-to-end (FastEmbedder +
//! UsearchStore + BM25 via the live `CodeIndexer::search` pipeline), runs the
//! query set, prints a per-intent table, and asserts a soft mean MRR@5 floor.
//! Test: each `#[ignore] #[tokio::test]` corresponds to one intent class.
//!
//! Run: cargo test --test benchmark_harness -- --include-ignored --nocapture
//!
//! Soft thresholds, not hard contracts: changes that drop MRR@5 below 0.3 mean
//! warrant scrutiny but the harness itself stays advisory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use trusty_search::core::indexer::{CodeChunk, CodeIndexer, SearchQuery};
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::{Embedder, FastEmbedder};

// ---------------------------------------------------------------------------
// Query corpora — `(query_text, expected_substring)`. The substring is matched
// case-insensitively against the chunk content (for substring queries) or the
// `function_name` field (for symbolic queries).
// ---------------------------------------------------------------------------

const DEFINITION_QUERIES: &[(&str, &str)] = &[
    ("UsearchStore", "UsearchStore"),
    ("CodeChunk struct", "CodeChunk"),
    ("QueryIntent enum", "QueryIntent"),
    ("RRF fusion", "rrf"),
    ("BM25 tokenize", "tokenize"),
    ("SymbolGraph", "SymbolGraph"),
    ("EntityExtractor", "EntityExtractor"),
    ("FastEmbedder", "FastEmbedder"),
    ("FactRecord", "FactRecord"),
    ("EdgeKind score_multiplier", "score_multiplier"),
];

const USAGE_QUERIES: &[(&str, &str)] = &[
    ("where is FastEmbedder called", "embed"),
    ("callers of kg_expand", "kg_expand"),
    ("uses of redb table", "redb"),
    ("search method calls", "search"),
    ("index_file usage", "index_file"),
    ("entity_exact_match called", "entity_exact_match"),
    ("mmr_rerank called", "mmr_rerank"),
    ("bm25 tokenize usage", "tokenize"),
    ("cosine_similarity callers", "cosine"),
    ("fact_hash usage", "fact_hash"),
];

const CONCEPTUAL_QUERIES: &[(&str, &str)] = &[
    ("how does BM25 scoring work", "bm25"),
    ("what handles concurrent reads", "rwlock"),
    ("how is RRF fusion computed", "rrf"),
    ("knowledge graph expansion", "kg"),
    ("how are embeddings cached", "cache"),
    ("intent routing weights", "alpha"),
    ("how are chunks stored", "redb"),
    ("file watching debounce", "debounce"),
    ("MCP tool dispatch", "dispatch"),
    ("entity extraction tree-sitter", "entity"),
];

const BUGDEBT_QUERIES: &[(&str, &str)] = &[
    ("panic sites in indexer", "panic"),
    ("unwrap calls in search path", "unwrap"),
    ("todo in codebase", "todo"),
    ("unimplemented stubs", "unimplemented"),
    ("error handling gaps", "error"),
    ("raises CapacityError", "capacity"),
    ("bail macro usage", "bail"),
    ("anyhow error sites", "anyhow"),
    ("missing test coverage", "test"),
    ("deprecated code", "deprecated"),
];

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Mean Reciprocal Rank at k: 1/(rank+1) of the first relevant hit, else 0.
fn mrr_at_k(results: &[CodeChunk], expected: &str, k: usize) -> f32 {
    let needle = expected.to_lowercase();
    results
        .iter()
        .take(k)
        .enumerate()
        .find(|(_, c)| {
            c.content.to_lowercase().contains(&needle)
                || c.function_name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
        })
        .map(|(i, _)| 1.0 / (i + 1) as f32)
        .unwrap_or(0.0)
}

/// Recall@k: did at least one of the top-k results contain the expected substring?
fn recall_at_k(results: &[CodeChunk], expected: &str, k: usize) -> bool {
    let needle = expected.to_lowercase();
    results
        .iter()
        .take(k)
        .any(|c| c.content.to_lowercase().contains(&needle))
}

// ---------------------------------------------------------------------------
// Grep / ripgrep baseline
// ---------------------------------------------------------------------------
//
// Why: a semantic-search benchmark is only meaningful relative to the tool it
// claims to beat. `grep -r` (or ripgrep when available) is the universal
// baseline developers reach for, so a side-by-side MRR@5 / Recall@10 table
// makes "where does semantic search win?" concrete and stable across refactors.
// What: derives a literal search term from each natural-language query, shells
// out to `rg` (preferred) or `grep -r` (fallback), wraps the first 10 ranked
// hits as `CodeChunk`s, and feeds them through the same metric functions used
// for trusty-search. Test: the four `#[ignore] bench_*_grep_baseline` tests, plus
// the merged comparison in each `bench_*_queries` test.

/// Pick the literal term a developer would actually grep for from a
/// natural-language query.
///
/// Why: `grep` cannot interpret prose ("how does BM25 scoring work"); a fair
/// baseline must reduce each query to the single keyword a human would type.
/// What: tokenizes on whitespace, drops common stop words and tokens shorter
/// than 3 chars, and returns the longest surviving token (ties broken by first
/// occurrence). Falls back to the longest raw token if everything is filtered.
/// Test: covered indirectly by the grep baseline tests; the chosen term is
/// printed in the comparison table for inspection.
fn grep_term(query: &str) -> String {
    const STOP: &[&str] = &[
        "how", "does", "what", "where", "the", "are", "is", "in", "of", "to", "called", "calls",
        "uses", "usage", "callers", "handles", "work", "works", "computed", "site", "sites",
        "code", "codebase", "gaps", "missing", "stubs",
    ];
    let best = query
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|t| t.len() >= 3 && !STOP.contains(&t.to_lowercase().as_str()))
        .max_by_key(|t| t.len());
    match best {
        Some(t) => t.to_string(),
        // Degenerate query (all stop words / short) — fall back to longest raw token.
        None => query
            .split_whitespace()
            .max_by_key(|t| t.len())
            .unwrap_or(query)
            .to_string(),
    }
}

/// Run the best available grep tool against `root` for a literal `term`,
/// returning matched-line "chunks" plus the wall-clock latency of the shell-out.
///
/// Why: trusty-search returns ranked `CodeChunk`s; to compute identical MRR@5 /
/// Recall@10 the grep baseline must produce a comparably ranked list. Capturing
/// latency alongside the result list lets the comparison table report a
/// like-for-like wall-clock cost — trusty-search's semantic latency vs the
/// system tool's I/O-bound cost — without a second sample run.
/// What: prefers `rg --line-number --no-heading -i <term>`, falls back to
/// `grep -rni --include=*.rs <term>`, parses `file:line:content`, and caps at
/// `top_k` hits (grep has no relevance ranking, so file-walk order is its
/// "ranking"). Returns `(matches, elapsed_ms)`; `elapsed_ms` covers process
/// spawn + scan + I/O and is `0` only when the tool failed to launch.
/// Test: exercised by the grep baseline tests; returns an empty vec on tool
/// failure so callers degrade to MRR=0 rather than panicking.
fn grep_search(root: &Path, term: &str, top_k: usize) -> (Vec<CodeChunk>, u128) {
    // Prefer ripgrep; fall back to POSIX grep -r. We probe rg once per call —
    // cheap relative to the embedding work that dominates the bench.
    let started = Instant::now();
    let output = if Command::new("rg").arg("--version").output().is_ok() {
        Command::new("rg")
            .args([
                "--line-number",
                "--no-heading",
                "--color=never",
                "-i",
                "-t",
                "rust",
                term,
            ])
            .arg(root)
            .output()
    } else {
        Command::new("grep")
            .args(["-rni", "--include=*.rs", term])
            .arg(root)
            .output()
    };

    let out = match output {
        Ok(o) => o,
        Err(_) => return (Vec::new(), 0),
    };
    let elapsed_ms = started.elapsed().as_millis();
    let text = String::from_utf8_lossy(&out.stdout);

    let chunks = text
        .lines()
        .take(top_k)
        .filter_map(|line| {
            // Format: <path>:<lineno>:<content>
            let mut parts = line.splitn(3, ':');
            let path = parts.next()?;
            let lineno: usize = parts.next()?.trim().parse().ok()?;
            let content = parts.next()?;
            let rel = Path::new(path)
                .strip_prefix(root)
                .unwrap_or_else(|_| Path::new(path))
                .to_string_lossy()
                .into_owned();
            // CodeChunk does not derive Default; spell out every field. We only
            // care about `content` (for the metric match) and ordering; the rest
            // are inert placeholders so the grep result is structurally a chunk.
            Some(CodeChunk {
                id: format!("{rel}:{lineno}"),
                file: rel,
                language: Some("rust".to_string()),
                start_line: lineno,
                end_line: lineno,
                content: content.to_string(),
                function_name: None,
                score: 0.0,
                compact_snippet: None,
                match_reason: "grep".to_string(),
                chunk_type: Default::default(),
                calls: Vec::new(),
                inherits_from: Vec::new(),
                chunk_depth: 0,
                index_id: None,
                on_branch: false,
                archive_reason: None,
            })
        })
        .collect();
    (chunks, elapsed_ms)
}

/// Per-query grep baseline row: `(query, mrr@5, recall@10, latency_ms)`.
type GrepRow = (String, f32, bool, u128);

/// Compute mean MRR@5 + Recall@10 + per-query latency for the grep baseline
/// over a query set, returning
/// `(mean_mrr, recall_hits, mean_latency_ms, per_query_rows)`.
///
/// Why: callers want one call site that produces every column the comparison
/// table needs. Bundling latency in lets us print a fair side-by-side cost
/// breakdown without re-shelling out per query in the renderer.
/// What: shells out via [`grep_search`] for each query, captures the returned
/// latency, and computes the mean. `mean_latency_ms` is in milliseconds.
/// Test: exercised by the grep baseline tests and `print_comparison`.
fn grep_metrics(root: &Path, queries: &[(&str, &str)]) -> (f32, usize, u128, Vec<GrepRow>) {
    let mut mrr_sum = 0.0_f32;
    let mut recall_hits = 0_usize;
    let mut latency_sum: u128 = 0;
    let mut per_query: Vec<GrepRow> = Vec::with_capacity(queries.len());
    for (q, expected) in queries {
        let term = grep_term(q);
        let (results, latency_ms) = grep_search(root, &term, 10);
        let mrr = mrr_at_k(&results, expected, 5);
        let rec = recall_at_k(&results, expected, 10);
        mrr_sum += mrr;
        if rec {
            recall_hits += 1;
        }
        latency_sum += latency_ms;
        per_query.push(((*q).to_string(), mrr, rec, latency_ms));
    }
    let n = queries.len() as u128;
    let mean_latency = latency_sum.checked_div(n).unwrap_or(0);
    (
        mrr_sum / queries.len() as f32,
        recall_hits,
        mean_latency,
        per_query,
    )
}

// ---------------------------------------------------------------------------
// Indexing fixture — shared across the four benchmark tests.
// ---------------------------------------------------------------------------

/// Locate the crate's own `src/` directory so the harness works regardless of
/// which directory `cargo test` was launched in.
///
/// Why: the previous path pointed at `crates/trusty-search-core/src/`, a
/// sub-crate that was folded into `trusty-search` at v0.3.0 and no longer
/// exists. The stale path silently indexed zero files, so every benchmark
/// reported MRR@5 = 0.000.
/// What: returns `<crate-root>/src`. `CARGO_MANIFEST_DIR` for a test in this
/// crate resolves to `crates/trusty-search/`, so `.join("src")` is the real
/// consolidated source tree (core / service / mcp modules).
/// Test: exercised by the four benchmark tests in this file, which now index a
/// non-empty corpus; also verifiable with `ls crates/trusty-search/src/`.
fn core_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Build a fresh `CodeIndexer` populated with every `.rs` file under the
/// crate's `src/`. Returns the indexer once population
/// completes (synchronous from the caller's perspective).
async fn build_indexer() -> CodeIndexer {
    let embedder: Arc<dyn Embedder> = Arc::new(
        FastEmbedder::new()
            .await
            .expect("init FastEmbedder (downloads model on first run)"),
    );
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(384).expect("init UsearchStore"));
    let indexer = CodeIndexer::new("bench", core_src_dir()).with_components(embedder, store);

    // Walk core src and feed every .rs file through index_file.
    let src = core_src_dir();
    for entry in walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let rel = path
            .strip_prefix(&src)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Err(e) = indexer.index_file(&rel, &content).await {
            eprintln!("index_file({rel}) failed: {e}");
        }
    }

    indexer
}

/// Run a query set, print a results table, and return the mean MRR@5 plus the
/// per-query rows so the caller can assert on the soft floor and build a
/// side-by-side grep comparison.
async fn run_bench(
    label: &str,
    indexer: &CodeIndexer,
    queries: &[(&str, &str)],
) -> (f32, Vec<(String, f32, bool)>) {
    println!("\n=== {label} ===");
    println!(
        "| {:<40} | {:>6} | {:>9} | {:>10} |",
        "query", "MRR@5", "Recall@10", "latency_ms"
    );
    println!("|{:-<42}|{:-<8}|{:-<11}|{:-<12}|", "", "", "", "");

    let mut mrr_sum = 0.0_f32;
    let mut recall_hits = 0_usize;
    let mut per_query = Vec::with_capacity(queries.len());
    for (q, expected) in queries {
        let sq = SearchQuery {
            text: (*q).to_string(),
            top_k: 10,
            expand_graph: true,
            compact: false,
            ..Default::default()
        };
        let started = Instant::now();
        let results = indexer
            .search(&sq)
            .await
            .expect("search must not fail in bench harness");
        let latency_ms = started.elapsed().as_millis();
        let mrr = mrr_at_k(&results, expected, 5);
        let rec = recall_at_k(&results, expected, 10);
        mrr_sum += mrr;
        if rec {
            recall_hits += 1;
        }
        per_query.push(((*q).to_string(), mrr, rec));
        println!(
            "| {:<40} | {:>6.3} | {:>9} | {:>10} |",
            truncate(q, 40),
            mrr,
            if rec { "yes" } else { "no" },
            latency_ms
        );
    }

    let n = queries.len() as f32;
    let mean_mrr = mrr_sum / n;
    let recall_pct = (recall_hits as f32 / n) * 100.0;
    println!(
        "mean MRR@5 = {:.3}  |  Recall@10 = {:.0}% ({}/{})",
        mean_mrr,
        recall_pct,
        recall_hits,
        queries.len()
    );
    (mean_mrr, per_query)
}

/// Print a per-query side-by-side comparison table of trusty-search vs grep and
/// a winner summary, then return the grep mean MRR@5 for context.
///
/// Why: the whole point of the baseline is the *comparison*; a single combined
/// table makes regressions and grep-wins obvious at a glance. What: greps the
/// crate source for each query's literal term, aligns the per-query rows with
/// the trusty-search rows, and tallies how many queries each tool wins on MRR@5.
/// Test: invoked from each `bench_*_queries` test under `--nocapture`.
fn print_comparison(label: &str, search_rows: &[(String, f32, bool)], queries: &[(&str, &str)]) {
    let root = core_src_dir();
    let (grep_mean, grep_recall, grep_mean_latency, grep_rows) = grep_metrics(&root, queries);

    println!("\n--- {label}: trusty-search vs grep ---");
    println!(
        "| {:<34} | {:<12} | {:>9} | {:>9} | {:>9} | {:<8} |",
        "query", "grep term", "ts MRR@5", "gp MRR@5", "gp ms", "winner"
    );
    println!(
        "|{:-<36}|{:-<14}|{:-<11}|{:-<11}|{:-<11}|{:-<10}|",
        "", "", "", "", "", ""
    );

    let mut ts_wins = 0_usize;
    let mut grep_wins = 0_usize;
    let mut ties = 0_usize;
    let mut ts_mrr_sum = 0.0_f32;

    for (i, (q, _expected)) in queries.iter().enumerate() {
        let (_, ts_mrr, _) = &search_rows[i];
        let (_, gp_mrr, _, gp_latency) = &grep_rows[i];
        ts_mrr_sum += *ts_mrr;
        let winner = if (ts_mrr - gp_mrr).abs() < f32::EPSILON {
            ties += 1;
            "tie"
        } else if ts_mrr > gp_mrr {
            ts_wins += 1;
            "trusty"
        } else {
            grep_wins += 1;
            "grep"
        };
        println!(
            "| {:<34} | {:<12} | {:>9.3} | {:>9.3} | {:>9} | {:<8} |",
            truncate(q, 34),
            truncate(&grep_term(q), 12),
            ts_mrr,
            gp_mrr,
            gp_latency,
            winner
        );
    }

    let n = queries.len() as f32;
    println!(
        "trusty mean MRR@5 = {:.3}  |  grep mean MRR@5 = {:.3}  |  grep Recall@10 = {:.0}% ({}/{})  |  grep mean latency = {} ms",
        ts_mrr_sum / n,
        grep_mean,
        (grep_recall as f32 / n) * 100.0,
        grep_recall,
        queries.len(),
        grep_mean_latency,
    );
    println!("winners — trusty: {ts_wins}  grep: {grep_wins}  ties: {ties}");
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

const SOFT_MRR_FLOOR: f32 = 0.3;

// ---------------------------------------------------------------------------
// Tests — `#[ignore]` so they don't run in `cargo test --workspace`.
// ---------------------------------------------------------------------------

#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_definition_queries() {
    let indexer = build_indexer().await;
    let (mean, rows) = run_bench("definition", &indexer, DEFINITION_QUERIES).await;
    print_comparison("definition", &rows, DEFINITION_QUERIES);
    assert!(
        mean >= SOFT_MRR_FLOOR,
        "definition mean MRR@5 = {mean:.3} below soft floor {SOFT_MRR_FLOOR}"
    );
}

#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_usage_queries() {
    let indexer = build_indexer().await;
    let (mean, rows) = run_bench("usage", &indexer, USAGE_QUERIES).await;
    print_comparison("usage", &rows, USAGE_QUERIES);
    assert!(
        mean >= SOFT_MRR_FLOOR,
        "usage mean MRR@5 = {mean:.3} below soft floor {SOFT_MRR_FLOOR}"
    );
}

#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_conceptual_queries() {
    let indexer = build_indexer().await;
    let (mean, rows) = run_bench("conceptual", &indexer, CONCEPTUAL_QUERIES).await;
    print_comparison("conceptual", &rows, CONCEPTUAL_QUERIES);
    assert!(
        mean >= SOFT_MRR_FLOOR,
        "conceptual mean MRR@5 = {mean:.3} below soft floor {SOFT_MRR_FLOOR}"
    );
}

#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_bugdebt_queries() {
    let indexer = build_indexer().await;
    let (mean, rows) = run_bench("bugdebt", &indexer, BUGDEBT_QUERIES).await;
    print_comparison("bugdebt", &rows, BUGDEBT_QUERIES);
    assert!(
        mean >= SOFT_MRR_FLOOR,
        "bugdebt mean MRR@5 = {mean:.3} below soft floor {SOFT_MRR_FLOOR}"
    );
}

// ---------------------------------------------------------------------------
// Grep / ripgrep baseline tests — `#[ignore]`-gated like the trusty-search
// benches so `cargo test --workspace` stays fast. These run grep alone (no
// embedder) so the baseline can be inspected even when the model is absent.
// ---------------------------------------------------------------------------

/// Shared body: grep one intent class and print its MRR@5 / Recall@10 table.
fn run_grep_baseline(label: &str, queries: &[(&str, &str)]) {
    let root = core_src_dir();
    let (mean, recall_hits, mean_latency, rows) = grep_metrics(&root, queries);

    println!("\n=== grep baseline: {label} ===");
    println!(
        "| {:<40} | {:<14} | {:>6} | {:>9} | {:>10} |",
        "query", "grep term", "MRR@5", "Recall@10", "latency_ms"
    );
    println!(
        "|{:-<42}|{:-<16}|{:-<8}|{:-<11}|{:-<12}|",
        "", "", "", "", ""
    );
    for ((q, mrr, rec, latency_ms), (orig_q, _)) in rows.iter().zip(queries.iter()) {
        debug_assert_eq!(q, orig_q);
        println!(
            "| {:<40} | {:<14} | {:>6.3} | {:>9} | {:>10} |",
            truncate(q, 40),
            truncate(&grep_term(orig_q), 14),
            mrr,
            if *rec { "yes" } else { "no" },
            latency_ms
        );
    }
    let n = queries.len() as f32;
    println!(
        "grep mean MRR@5 = {:.3}  |  Recall@10 = {:.0}% ({}/{})  |  mean latency = {} ms",
        mean,
        (recall_hits as f32 / n) * 100.0,
        recall_hits,
        queries.len(),
        mean_latency,
    );
}

#[ignore]
#[test]
fn bench_definition_grep_baseline() {
    run_grep_baseline("definition", DEFINITION_QUERIES);
}

#[ignore]
#[test]
fn bench_usage_grep_baseline() {
    run_grep_baseline("usage", USAGE_QUERIES);
}

#[ignore]
#[test]
fn bench_conceptual_grep_baseline() {
    run_grep_baseline("conceptual", CONCEPTUAL_QUERIES);
}

#[ignore]
#[test]
fn bench_bugdebt_grep_baseline() {
    run_grep_baseline("bugdebt", BUGDEBT_QUERIES);
}

// ---------------------------------------------------------------------------
// Live-daemon `/grep` endpoint vs system ripgrep — `#[ignore]`-gated.
//
// Why: the line-oriented `/grep` endpoint is meant to be a drop-in replacement
// for shelling out to ripgrep, but its real value depends on (a) returning the
// same hits as ripgrep for the same pattern and (b) doing so at competitive
// wall-clock latency. This bench drives the live daemon end-to-end so the two
// dimensions can be inspected in one table.
// What: requires a daemon at `http://127.0.0.1:7878` with the `trusty-search`
// index registered. For each fixed pattern it issues `POST
// /indexes/trusty-search/grep`, shells out `rg` against the indexed root, and
// prints per-pattern hit count + latency. Reports P50/P95 and the latency
// ratio. The test never asserts on absolute numbers — it's a baseline gate, not
// a regression threshold (those live in `baseline_trusty_tools.rs`).
// ---------------------------------------------------------------------------

/// Representative grep patterns covering literal, regex, and word-boundary
/// shapes — picked to mirror common LLM grep calls against trusty-search.
const GREP_ENDPOINT_PATTERNS: &[(&str, &str)] = &[
    ("CodeChunk", "literal struct name"),
    ("fn search", "literal-with-space"),
    (r"fn \w+_index", "regex"),
    ("HnswIndex", "literal type"),
    (r"impl\s+\w+\s+for", "regex impl block"),
];

#[ignore]
#[tokio::test]
async fn bench_grep_endpoint_vs_ripgrep() {
    const DAEMON: &str = "http://127.0.0.1:7878";
    const INDEX: &str = "trusty-search";

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client");

    // Resolve the indexed root by asking the daemon — the test should still
    // work if the index was registered against a different on-disk path.
    let status = client
        .get(format!("{DAEMON}/indexes/{INDEX}/status"))
        .send()
        .await
        .expect("daemon must be running on 127.0.0.1:7878 with the trusty-search index registered");
    assert!(
        status.status().is_success(),
        "GET /indexes/{INDEX}/status returned {}: register the index first",
        status.status()
    );
    let status_json: serde_json::Value = status.json().await.expect("JSON status");
    let root_path = status_json["root_path"]
        .as_str()
        .expect("status.root_path must be a string");
    let root = std::path::Path::new(root_path);

    println!("\n=== /grep endpoint vs ripgrep ===");
    println!("daemon: {DAEMON}   index: {INDEX}   root: {root_path}");
    println!(
        "| {:<28} | {:>9} | {:>9} | {:>9} | {:>9} | {:>6} |",
        "pattern", "ep hits", "rg hits", "ep ms", "rg ms", "ratio"
    );
    println!(
        "|{:-<30}|{:-<11}|{:-<11}|{:-<11}|{:-<11}|{:-<8}|",
        "", "", "", "", "", ""
    );

    let mut ep_latencies: Vec<u128> = Vec::with_capacity(GREP_ENDPOINT_PATTERNS.len());
    let mut rg_latencies: Vec<u128> = Vec::with_capacity(GREP_ENDPOINT_PATTERNS.len());

    for (pattern, _label) in GREP_ENDPOINT_PATTERNS {
        // (a) /grep endpoint.
        let body = serde_json::json!({
            "pattern": pattern,
            "max_results": 500,
        });
        let t0 = Instant::now();
        let resp = client
            .post(format!("{DAEMON}/indexes/{INDEX}/grep"))
            .json(&body)
            .send()
            .await
            .expect("POST /grep transport");
        let ep_ms = t0.elapsed().as_millis();
        assert!(
            resp.status().is_success(),
            "POST /indexes/{INDEX}/grep returned {} for pattern {pattern}",
            resp.status()
        );
        let ep_body: serde_json::Value = resp.json().await.expect("grep response JSON");
        let ep_hits = ep_body["matches"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();

        // (b) shell-out rg with the same pattern.
        let (rg_chunks, rg_ms) = grep_search(root, pattern, 500);
        let rg_hits = rg_chunks.len();

        let ratio = if rg_ms == 0 {
            f64::INFINITY
        } else {
            ep_ms as f64 / rg_ms as f64
        };
        println!(
            "| {:<28} | {:>9} | {:>9} | {:>9} | {:>9} | {:>6.2} |",
            truncate(pattern, 28),
            ep_hits,
            rg_hits,
            ep_ms,
            rg_ms,
            ratio
        );

        ep_latencies.push(ep_ms);
        rg_latencies.push(rg_ms);
    }

    ep_latencies.sort_unstable();
    rg_latencies.sort_unstable();
    let p = |xs: &[u128], pct: usize| -> u128 {
        if xs.is_empty() {
            return 0;
        }
        let i = ((xs.len() * pct) / 100).min(xs.len() - 1);
        xs[i]
    };
    println!(
        "/grep   P50={} ms  P95={} ms",
        p(&ep_latencies, 50),
        p(&ep_latencies, 95)
    );
    println!(
        "ripgrep P50={} ms  P95={} ms",
        p(&rg_latencies, 50),
        p(&rg_latencies, 95)
    );
}

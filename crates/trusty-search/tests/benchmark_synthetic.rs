//! Synthetic non-circular benchmark harness for trusty-search hybrid retrieval.
//!
//! Why: benchmarks run against `trusty-tools` itself suffer from BM25 circular
//! bias (issue #123) because the benchmark query strings appear verbatim in
//! the indexed test files (e.g. `core/classifier.rs` assert literals). This
//! harness drives the daemon against a fully synthetic corpus
//! (`tests/benchmark_corpus/synthetic/`) where every symbol name has been
//! verified to appear nowhere else in the repository — so the BM25 lane
//! cannot lift a query just by seeing its own benchmark text.
//!
//! What: reads `GROUND_TRUTH.json`, registers a fresh `synthetic-benchmark`
//! index pointing at the corpus, drives a reindex, polls until every stage
//! is `Ready`, then runs each ground-truth query in three modes (lexical
//! only / full hybrid / KG-leading), records Hit@1 and Hit@5, prints a
//! per-(mode × query-category) comparison table, and deletes the index.
//!
//! `mode_hint` support (v0.2.0): each query in GROUND_TRUTH.json carries a
//! `mode_hint` field (`"code"` / `"text"` / `"data"`). The harness forwards
//! the hint as the `mode` parameter in the search request body so the daemon
//! can route text/data queries away from the code-semantic pipeline.
//!
//! Test: gated `#[ignore]` so it does not run during default `cargo test`.
//! Run with:
//!   cargo test --test benchmark_synthetic -- --include-ignored --nocapture
//!
//! Prerequisites:
//!   - trusty-search daemon running at `http://127.0.0.1:7878`
//!   - `OPENROUTER_API_KEY` not required (no chat calls)
//!
//! This harness intentionally does NOT spin up its own daemon. It uses the
//! already-running daemon the developer started for everyday work, consistent
//! with the pattern in `baseline_trusty_tools.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

// ── Constants ───────────────────────────────────────────────────────────────

const DAEMON_URL: &str = "http://127.0.0.1:7878";
const INDEX_NAME: &str = "synthetic-benchmark";

/// Maximum time we will wait for the reindex to bring every stage to `Ready`.
const REINDEX_TIMEOUT: Duration = Duration::from_secs(180);

/// Interval between status polls during reindex wait.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

// ── Types ───────────────────────────────────────────────────────────────────

/// One ground-truth entry loaded from GROUND_TRUTH.json.
///
/// Why: a typed struct avoids indexing into raw JSON throughout the harness.
/// What: holds all GROUND_TRUTH.json fields for one query.
/// Test: load_ground_truth() panics if any required field is missing.
#[derive(Debug, Clone)]
#[allow(dead_code)] // category + mode_hint are used for the breakdown table; others retained for future assertions.
struct GroundTruthQuery {
    id: String,
    text: String,
    category: String,
    /// Forwarded as `mode` in the search request body.
    /// Values: "code", "text", "data" (from GROUND_TRUTH.json `mode_hint` field).
    mode_hint: String,
    ground_truth_files: Vec<String>,
}

/// The three retrieval modes exercised per query.
///
/// Why: running all three against every query reveals where each mode wins or
/// loses and validates that hybrid/KG-leading is worth the overhead.
/// What: enum with a label method for table output.
/// Test: iterated in the main test body.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// `?stage=lexical` — BM25 only.
    Lexical,
    /// No stage parameter — full hybrid (BM25 + vector + KG expansion + RRF).
    Hybrid,
    /// `expand_graph=true, use_kg_first=true` — KG-leading retrieval.
    KgLeading,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Lexical => "lexical",
            Mode::Hybrid => "hybrid",
            Mode::KgLeading => "kg-leading",
        }
    }
}

/// Result for one (query, mode) pair.
///
/// Why: collecting all results into a Vec lets the summary tables iterate
/// over any slice (by mode, by category, aggregate) without re-querying.
/// What: hit booleans, latencies, and top file list for one search call.
/// Test: populated inside run_query(); assertions in the main test body.
#[derive(Debug, Clone)]
#[allow(dead_code)] // query_text retained for diagnostic prints during failures.
struct QueryResult {
    query_id: String,
    query_text: String,
    query_category: String,
    mode: Mode,
    top_files: Vec<String>,
    hit_at_1: bool,
    hit_at_5: bool,
    intent: String,
    match_reason: String,
    server_latency_ms: Option<u64>,
    client_latency_ms: u128,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Build a reqwest client with the timeouts already tuned by
/// `baseline_trusty_tools.rs`.
///
/// Why: we share the same daemon as everyday work; generous but finite
/// timeouts prevent the test hanging on a hung daemon.
/// What: returns a Client with 2 s connect and 30 s request timeouts.
/// Test: any transport error will panic via .expect() in callers.
fn make_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client construction is infallible")
}

/// Absolute path of the synthetic corpus root.
///
/// Why: the corpus root changes depending on where cargo runs the test.
/// What: derives the path from CARGO_MANIFEST_DIR at compile time.
/// Test: corpus_root().join("GROUND_TRUTH.json") must exist; load_ground_truth panics otherwise.
fn corpus_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("tests")
        .join("benchmark_corpus")
        .join("synthetic")
}

/// Load and parse `GROUND_TRUTH.json`.
///
/// Why: centralises all JSON field access so callers use typed structs.
/// What: reads the file, parses every query array entry into GroundTruthQuery.
///   Accepts both `expected_mode` (v0.1 key) and `mode_hint` (v0.2 key),
///   preferring `mode_hint` when both are present.
/// Test: panics with a clear message if the file is missing or malformed.
fn load_ground_truth() -> Vec<GroundTruthQuery> {
    let path = corpus_root().join("GROUND_TRUTH.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read GROUND_TRUTH.json at {path:?}: {e}"));
    let raw: Value = serde_json::from_slice(&bytes).expect("GROUND_TRUTH.json must be valid JSON");
    let queries = raw["queries"].as_array().expect("queries array required");
    queries
        .iter()
        .map(|q| {
            // Accept both v0.1 `expected_mode` and v0.2 `mode_hint`; prefer v0.2.
            let mode_hint = q["mode_hint"]
                .as_str()
                .or_else(|| q["expected_mode"].as_str())
                .unwrap_or("code")
                .to_string();
            GroundTruthQuery {
                id: q["id"].as_str().unwrap().to_string(),
                text: q["text"].as_str().unwrap().to_string(),
                category: q["category"].as_str().unwrap().to_string(),
                mode_hint,
                ground_truth_files: q["ground_truth_files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect(),
            }
        })
        .collect()
}

/// Sanity-check that the daemon is up.
///
/// Why: a clear early error is better than confusing transport failures later.
/// What: GETs /health and asserts 200.
/// Test: panics with a human-readable message if the daemon is unreachable.
async fn assert_daemon_healthy(client: &Client) {
    let resp = client
        .get(format!("{DAEMON_URL}/health"))
        .send()
        .await
        .expect("daemon must be reachable at 127.0.0.1:7878 — start it with `trusty-search start`");
    assert_eq!(resp.status().as_u16(), 200, "GET /health returned non-200");
}

/// Create (or re-create) the `synthetic-benchmark` index. Returns immediately
/// after the POST — the caller drives the reindex separately.
///
/// Why: starting from a clean slate prevents stale chunk data from a previous
/// run from inflating Hit@K scores.
/// What: DELETEs any existing index with this name, then POSTs to /indexes.
/// Test: asserts 200 on POST; transport errors panic.
async fn register_index(client: &Client) {
    // Delete first if it exists, to start from a clean slate.
    let _ = client
        .delete(format!(
            "{DAEMON_URL}/indexes/{INDEX_NAME}?delete_data=true"
        ))
        .send()
        .await;

    let root = corpus_root();
    let body = json!({
        "id": INDEX_NAME,
        "root_path": root.to_string_lossy(),
    });
    let resp = client
        .post(format!("{DAEMON_URL}/indexes"))
        .json(&body)
        .send()
        .await
        .expect("POST /indexes transport failure");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "POST /indexes returned non-200: body = {:?}",
        resp.text().await.ok()
    );
}

/// Trigger a full force-reindex and wait for every stage to reach `Ready`.
///
/// Why: queries against a partially-indexed corpus produce misleading Hit@K.
/// What: POSTs /reindex with force=true, then polls /status until lexical +
///   semantic + graph stages all report status="ready".
/// Test: panics with last-known status on REINDEX_TIMEOUT.
async fn reindex_and_wait(client: &Client) {
    let root = corpus_root();
    let body = json!({
        "root_path": root.to_string_lossy(),
        "force": true,
    });
    let resp = client
        .post(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/reindex"))
        .json(&body)
        .send()
        .await
        .expect("POST /reindex transport failure");
    assert_eq!(resp.status().as_u16(), 200, "POST /reindex non-200");

    // Poll status until every stage is Ready.
    let start = Instant::now();
    loop {
        if start.elapsed() > REINDEX_TIMEOUT {
            // Print last status for diagnostics.
            let status = fetch_status(client).await;
            panic!(
                "synthetic-benchmark reindex did not reach Ready within {:?}\nlast status: {status}",
                REINDEX_TIMEOUT
            );
        }

        let status = fetch_status(client).await;
        let stages_ready = status["stages"].is_object()
            && ["lexical", "semantic", "graph"].iter().all(|stage| {
                status["stages"][stage]["status"]
                    .as_str()
                    .map(|s| s == "ready")
                    .unwrap_or(false)
            });
        if stages_ready {
            let chunks = status["chunk_count"].as_u64().unwrap_or(0);
            println!(
                "    reindex complete: chunks={chunks}, elapsed={:.1}s",
                start.elapsed().as_secs_f64()
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Fetch `/indexes/{INDEX_NAME}/status`.
///
/// Why: status polling and diagnostic footers both need this.
/// What: GET /indexes/:id/status, returns parsed JSON Value.
/// Test: transport or parse failures panic.
async fn fetch_status(client: &Client) -> Value {
    let resp = client
        .get(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/status"))
        .send()
        .await
        .expect("GET /status transport failure");
    resp.json().await.expect("status JSON parse failure")
}

/// Delete the synthetic-benchmark index. Best-effort.
///
/// Why: cleanup ensures the developer's daemon doesn't accumulate stale
/// synthetic indexes between runs.
/// What: DELETE /indexes/:id; prints the response status regardless.
/// Test: failures are printed, not panicked (cleanup is best-effort).
async fn cleanup_index(client: &Client) {
    let resp = client
        .delete(format!(
            "{DAEMON_URL}/indexes/{INDEX_NAME}?delete_data=true"
        ))
        .send()
        .await;
    match resp {
        Ok(r) => {
            println!(
                "  cleanup DELETE /indexes/{INDEX_NAME} → {}",
                r.status().as_u16()
            );
        }
        Err(e) => println!("  cleanup DELETE failed: {e}"),
    }
}

/// Run one query in one retrieval mode and record the result.
///
/// Why: all three modes must run the same search path so results are
/// comparable; only the JSON body fields differ.
/// What: POSTs to /indexes/:id/search with mode-appropriate parameters
///   plus the query's `mode_hint` forwarded as `mode`. Records Hit@1 and
///   Hit@5 against the ground_truth_files list.
/// Test: transport failures panic; JSON parse failures panic with status code.
async fn run_query(client: &Client, query: &GroundTruthQuery, mode: Mode) -> QueryResult {
    let mut body = json!({
        "text": query.text,
        "top_k": 10,
        "compact": false,
        // Forward the per-query mode hint so text/data queries are routed
        // to the correct pipeline stage (not the code-semantic lane).
        "mode": query.mode_hint,
    });
    match mode {
        Mode::Lexical => {
            body["stage"] = json!("lexical");
        }
        Mode::Hybrid => {
            // No stage override — full hybrid is the daemon default.
        }
        Mode::KgLeading => {
            body["expand_graph"] = json!(true);
            body["use_kg_first"] = json!(true);
        }
    }

    let t0 = Instant::now();
    let resp = client
        .post(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/search"))
        .json(&body)
        .send()
        .await
        .expect("POST /search transport failure");
    let client_latency_ms = t0.elapsed().as_millis();

    let status = resp.status().as_u16();
    let json_body: Value = resp
        .json()
        .await
        .unwrap_or_else(|e| panic!("search response JSON parse failure ({status}): {e}"));

    let results = json_body["results"].as_array().cloned().unwrap_or_default();
    let top_files: Vec<String> = results
        .iter()
        .filter_map(|r| r["file"].as_str().map(|s| s.to_string()))
        .collect();

    // The daemon may report `file` as either a path relative to the index
    // root or an absolute path; normalise to relative so we can compare
    // against ground_truth_files.
    let normalised: Vec<String> = top_files.iter().map(|f| normalise_path(f)).collect();

    let hit_at_1 = normalised
        .first()
        .map(|f| any_match(f, &query.ground_truth_files))
        .unwrap_or(false);
    let hit_at_5 = normalised
        .iter()
        .take(5)
        .any(|f| any_match(f, &query.ground_truth_files));

    let intent = json_body["intent"].as_str().unwrap_or("?").to_string();
    let match_reason = results
        .first()
        .and_then(|r| r["match_reason"].as_str())
        .unwrap_or("-")
        .to_string();
    let server_latency_ms = json_body["latency_ms"].as_u64();

    QueryResult {
        query_id: query.id.clone(),
        query_text: query.text.clone(),
        query_category: query.category.clone(),
        mode,
        top_files: normalised,
        hit_at_1,
        hit_at_5,
        intent,
        match_reason,
        server_latency_ms,
        client_latency_ms,
    }
}

/// Normalise an absolute or repo-relative path to the relative-to-corpus form
/// used by ground_truth_files.
///
/// Why: the daemon may return absolute paths or relative paths depending on
/// index registration; normalising removes the ambiguity.
/// What: strips the corpus root prefix when found; strips leading "./".
/// Test: `any_match` relies on this to compare against ground_truth_files entries.
fn normalise_path(file: &str) -> String {
    // Strip leading "./".
    let trimmed = file.trim_start_matches("./");
    // If the daemon reported an absolute path, strip everything up to and
    // including the corpus root segment.
    if let Some(idx) = trimmed.find("benchmark_corpus/synthetic/") {
        let after = &trimmed[idx + "benchmark_corpus/synthetic/".len()..];
        return after.to_string();
    }
    trimmed.to_string()
}

/// Returns true if `result_file` matches any entry in `ground_truth_files`.
///
/// Why: the daemon may return paths with different root prefixes; an
/// ends_with check is more robust than an exact-equals check.
/// What: checks equality, path suffix, or slash-prefixed suffix.
/// Test: normalise_path + any_match together tested by the main test body.
fn any_match(result_file: &str, ground_truth_files: &[String]) -> bool {
    ground_truth_files.iter().any(|truth| {
        result_file == truth
            || result_file.ends_with(truth)
            || result_file.ends_with(&format!("/{truth}"))
    })
}

/// Print a markdown-formatted per-query results table.
///
/// Why: per-query detail helps diagnose which specific queries drive mode
/// differences that the aggregate table obscures.
/// What: one row per (query, mode) with Hit@1, Hit@5, latencies, intent, category.
/// Test: visual inspection of harness output.
fn print_per_query_table(results: &[QueryResult]) {
    println!("\n## Per-query results\n");
    println!(
        "| {:<4} | {:<10} | {:<10} | {:>4} | {:>4} | {:>8} | {:>9} | {:<14} | {:<12} |",
        "ID", "Mode", "Cat", "H@1", "H@5", "srv ms", "client ms", "Intent", "MatchReason"
    );
    println!(
        "|{:-<6}|{:-<12}|{:-<12}|{:-<6}|{:-<6}|{:-<10}|{:-<11}|{:-<16}|{:-<14}|",
        "", "", "", "", "", "", "", "", ""
    );
    for r in results {
        let h1 = if r.hit_at_1 { "Y" } else { "-" };
        let h5 = if r.hit_at_5 { "Y" } else { "-" };
        let srv = r
            .server_latency_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "| {:<4} | {:<10} | {:<10} | {:>4} | {:>4} | {:>8} | {:>9} | {:<14} | {:<12} |",
            r.query_id,
            r.mode.label(),
            r.query_category,
            h1,
            h5,
            srv,
            r.client_latency_ms,
            r.intent,
            r.match_reason
        );
    }
}

/// Print the per-(mode × query-category) Hit@K breakdown table.
///
/// Why: this is the primary analytical artifact — it shows whether hybrid/KG
/// modes outperform lexical specifically on conceptual queries (where BM25
/// has no literal match advantage).
/// What: rows = modes; columns = definition / conceptual / usage / text_data /
///   aggregate. Prints Hit@1 and Hit@5 for each cell.
/// Test: visual inspection; CI baseline doc records the numbers.
fn print_category_breakdown_table(results: &[QueryResult]) {
    let categories = ["definition", "conceptual", "usage", "text", "data"];
    let header_cats = ["Def", "Concept", "Usage", "Text", "Data", "All"];

    println!("\n## Per-(mode × query-category) Hit@K breakdown\n");
    println!(
        "| {:<10} | {:^15} | {:^15} | {:^15} | {:^15} | {:^15} | {:^15} |",
        "Mode",
        "Def Hit@1/5",
        "Concept Hit@1/5",
        "Usage Hit@1/5",
        "Text Hit@1/5",
        "Data Hit@1/5",
        "Aggregate Hit@1/5",
    );
    println!(
        "|{:-<12}|{:-<17}|{:-<17}|{:-<17}|{:-<17}|{:-<17}|{:-<17}|",
        "", "", "", "", "", "", ""
    );

    for mode in [Mode::Lexical, Mode::Hybrid, Mode::KgLeading] {
        let mode_results: Vec<&QueryResult> = results
            .iter()
            .filter(|r| std::mem::discriminant(&r.mode) == std::mem::discriminant(&mode))
            .collect();

        let mut cells: Vec<String> = Vec::new();

        // Per-category cells.
        for cat in categories {
            let subset: Vec<&&QueryResult> = mode_results
                .iter()
                .filter(|r| r.query_category == cat)
                .collect();
            if subset.is_empty() {
                cells.push(format!("{:^15}", "n/a"));
            } else {
                let n = subset.len();
                let h1 = subset.iter().filter(|r| r.hit_at_1).count();
                let h5 = subset.iter().filter(|r| r.hit_at_5).count();
                cells.push(format!("{:^15}", format!("{h1}/{n} | {h5}/{n}")));
            }
        }

        // Aggregate cell.
        let n = mode_results.len();
        let h1 = mode_results.iter().filter(|r| r.hit_at_1).count();
        let h5 = mode_results.iter().filter(|r| r.hit_at_5).count();
        cells.push(format!("{:^15}", format!("{h1}/{n} | {h5}/{n}")));

        println!(
            "| {:<10} | {} | {} | {} | {} | {} | {} |",
            mode.label(),
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            cells[4],
            cells[5],
        );
    }

    // Print a separator and the category-column headers for clarity.
    println!("\n  Columns: {}", header_cats.join(" / "));
    println!("  Format per cell: Hit@1/total | Hit@5/total\n");
}

/// Print the aggregate Hit@K table comparing modes (identical to v0.1 table).
///
/// Why: aggregate numbers for backward compatibility with v0.1 baseline doc.
/// What: one row per mode with overall Hit@1%, Hit@5%, p50 latencies.
/// Test: visual inspection.
fn print_aggregate_table(results: &[QueryResult]) {
    println!("\n## Aggregate by mode\n");
    println!(
        "| {:<10} | {:>10} | {:>10} | {:>13} | {:>13} |",
        "Mode", "Hit@1", "Hit@5", "p50 client ms", "p50 server ms"
    );
    println!(
        "|{:-<12}|{:-<12}|{:-<12}|{:-<15}|{:-<15}|",
        "", "", "", "", ""
    );
    for mode in [Mode::Lexical, Mode::Hybrid, Mode::KgLeading] {
        let subset: Vec<&QueryResult> = results
            .iter()
            .filter(|r| std::mem::discriminant(&r.mode) == std::mem::discriminant(&mode))
            .collect();
        if subset.is_empty() {
            continue;
        }
        let total = subset.len() as f64;
        let h1_count = subset.iter().filter(|r| r.hit_at_1).count();
        let h5_count = subset.iter().filter(|r| r.hit_at_5).count();
        let h1_pct = 100.0 * h1_count as f64 / total;
        let h5_pct = 100.0 * h5_count as f64 / total;

        let mut client_latencies: Vec<u128> = subset.iter().map(|r| r.client_latency_ms).collect();
        client_latencies.sort_unstable();
        let p50_client = percentile(&client_latencies, 50);

        let mut server_latencies: Vec<u64> =
            subset.iter().filter_map(|r| r.server_latency_ms).collect();
        server_latencies.sort_unstable();
        let p50_server = if server_latencies.is_empty() {
            0
        } else {
            server_latencies[server_latencies.len() / 2]
        };

        println!(
            "| {:<10} | {:>4}/{:<2} {:>3.0}% | {:>4}/{:<2} {:>3.0}% | {:>13} | {:>13} |",
            mode.label(),
            h1_count,
            subset.len(),
            h1_pct,
            h5_count,
            subset.len(),
            h5_pct,
            p50_client,
            p50_server
        );
    }
}

/// Compute the p-th percentile of a sorted slice.
///
/// Why: latency reporting wants p50 without pulling in a stats crate.
/// What: index arithmetic into a sorted slice; returns 0 on empty.
/// Test: used by print_aggregate_table; correctness is visually checked.
fn percentile(sorted: &[u128], p: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() * p) / 100).min(sorted.len() - 1);
    sorted[idx]
}

// ── The actual test ─────────────────────────────────────────────────────────

/// Index the synthetic corpus, run every ground-truth query in three modes,
/// print the comparison tables, and clean up.
///
/// Why: this is the FIRST measurement of trusty-search hybrid retrieval that
/// is provably free of BM25 circular bias (issue #123). The numbers it prints
/// land in docs/regression-testing/synthetic-corpus-baseline-*.md.
///   v0.2.0 adds: mode_hint forwarding and per-(mode × category) table.
/// What: the steps documented in the file-level comment.
/// Test: this IS the test.
#[tokio::test]
#[ignore]
async fn benchmark_synthetic_corpus_all_modes() {
    let client = make_client();
    assert_daemon_healthy(&client).await;
    println!("\n=== synthetic-benchmark corpus, three-mode evaluation (v0.2.0) ===");
    println!("corpus root: {}", corpus_root().display());

    let queries = load_ground_truth();
    println!("loaded {} ground-truth queries", queries.len());

    println!("\nregistering index '{INDEX_NAME}'...");
    register_index(&client).await;

    println!("triggering force-reindex and waiting for Ready...");
    reindex_and_wait(&client).await;

    // Capture chunk count from status for the diagnostics footer.
    let status = fetch_status(&client).await;
    let chunk_count = status["chunk_count"].as_u64().unwrap_or(0);
    let stages: Value = status["stages"].clone();

    let mut all_results: Vec<QueryResult> = Vec::with_capacity(queries.len() * 3);
    for mode in [Mode::Lexical, Mode::Hybrid, Mode::KgLeading] {
        println!("\n--- mode = {} ---", mode.label());
        for q in &queries {
            let result = run_query(&client, q, mode).await;
            println!(
                "  {} [{}] (mode_hint={}, cat={}): H@1={} H@5={} top1={}",
                q.id,
                q.text,
                q.mode_hint,
                q.category,
                if result.hit_at_1 { "Y" } else { "-" },
                if result.hit_at_5 { "Y" } else { "-" },
                result
                    .top_files
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "<none>".into())
            );
            all_results.push(result);
        }
    }

    print_per_query_table(&all_results);
    print_category_breakdown_table(&all_results);
    print_aggregate_table(&all_results);

    println!("\n## Diagnostics");
    println!("- chunk_count: {chunk_count}");
    println!("- stages: {stages}");

    // Sanity asserts so the test fails loudly if the daemon misbehaved
    // (e.g. zero results across all queries).
    let total_hits = all_results.iter().filter(|r| r.hit_at_5).count();
    assert!(
        total_hits > 0,
        "every single query missed at H@5 — daemon may be misconfigured"
    );

    cleanup_index(&client).await;
}

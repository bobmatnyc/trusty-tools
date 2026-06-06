//! Organic-corpus benchmark harness for trusty-search v0.10.0 per-lane MCP tools.
//!
//! Why: the synthetic corpus (`benchmark_synthetic.rs`, #123 v2) was too small
//! at 47 files / 298 chunks to differentiate `search_kg` from `search_semantic`
//! — KG-leading mode collapsed to the hybrid baseline because there were too
//! few inter-file call edges. `crates/open-mpm/` is the natural next probe:
//! ~282 source files (~115k lines) of organic Rust code authored before these
//! benchmark queries existed, so it answers: does the KG signal pull its
//! weight on a real Rust workspace?
//!
//! What: reads `benchmark_open_mpm_ground_truth.json`, registers a fresh
//! `open-mpm-benchmark` index pointing at `crates/open-mpm/`, drives a
//! force-reindex while sampling daemon RSS every 15 s (bails at > 12 GB),
//! polls until every stage is `ready`, runs each query through the four
//! per-lane MCP tool equivalents (`search_lexical`, `search_semantic`,
//! `search_kg`, `search_all`), computes Hit@K per (tool × type), and
//! deletes the index. KG-seed queries use a two-stage pattern: stage-1
//! `search_lexical` to find the seed chunk_id, stage-2 `search_kg` with
//! `seed_chunk_id` and `expand_graph=true` to traverse the symbol graph.
//!
//! Per-tool HTTP mapping (matches `crates/trusty-search/src/mcp/tools.rs`
//! `run_lane_search` → `/indexes/:id/search` body shape):
//!   - `search_lexical`  → `stage="lexical"`, `expand_graph=false`
//!   - `search_semantic` → `stage="semantic"`, `expand_graph=false`
//!   - `search_kg`       → `stage="graph"`, `expand_graph=true`
//!   - `search_all`      → no `stage` field, `expand_graph=false`
//!
//! Test: gated `#[ignore]` so it does not run during default `cargo test`.
//! Run with:
//!   cargo test --test benchmark_open_mpm -- --include-ignored --nocapture
//!
//! Prerequisites:
//!   - trusty-search daemon running at `http://127.0.0.1:7878` (v0.10.0+)
//!   - `crates/open-mpm/` source tree present (in-tree workspace member)
//!
//! Like `benchmark_synthetic.rs`, this harness does NOT spin up its own
//! daemon — it uses the developer's already-running instance and cleans up
//! after itself.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

// ── Constants ───────────────────────────────────────────────────────────────

const DAEMON_URL: &str = "http://127.0.0.1:7878";

/// Why: do NOT clash with the existing `open-mpm` index in the daemon's
/// registry (which points at the old `/Users/masa/Projects/open-mpm` path).
/// What: a benchmark-only index name; cleanup deletes it after the run.
const INDEX_NAME: &str = "open-mpm-benchmark";

/// Why: open-mpm at ~282 files is bigger than the synthetic corpus
/// (47 files / 298 chunks). Expect ~3–5 min wall-clock for reindex on a
/// warm CoreML-accelerated daemon; allow generous headroom for cold runs.
/// What: hard ceiling on reindex wait time.
const REINDEX_TIMEOUT: Duration = Duration::from_secs(900);

/// Interval between status polls during reindex wait.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Interval between RSS samples logged to stdout during reindex.
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_secs(15);

/// Why: empirical measurement during open-mpm reindex showed the CoreML
/// pipeline peaks at 17–20 GB during the embedding phase (much higher
/// than the 5–10 GB observed on the synthetic corpus). The bail threshold
/// is set to 28 GB — well below the daemon's 32 GB ceiling but above the
/// observed natural peak, so we bail on runaway growth rather than
/// expected behaviour.
/// What: harness bails before the daemon's own ceiling kicks in.
const RSS_BAIL_MB: u64 = 28_672;

// ── Types ───────────────────────────────────────────────────────────────────

/// One ground-truth entry loaded from the JSON file.
///
/// Why: typed access keeps the harness body readable; `kg_seed_query` is
/// optional and only populated for `type = "kg_seed"` rows.
/// What: holds every field the harness needs from one query row.
/// Test: `load_ground_truth` panics if required fields are missing.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `description` is retained for diagnostic prints.
struct GroundTruthQuery {
    id: String,
    text: String,
    /// definition / conceptual / kg_seed / negative
    query_type: String,
    /// code / text / data — forwarded as `mode` to the daemon.
    mode_hint: String,
    /// Files (relative to the open-mpm crate root) considered correct.
    /// Empty for `negative` queries.
    ground_truth_files: Vec<String>,
    /// Stage-1 lexical seed text. Only present for `kg_seed` queries.
    kg_seed_query: Option<String>,
    description: String,
}

/// Which per-lane MCP tool we are emulating over HTTP.
///
/// Why: v0.10.0 exposes four canonical tools (`search_lexical`,
/// `search_semantic`, `search_kg`, `search_all`). The harness drives the
/// daemon via the same JSON body shape that `mcp::tools::run_lane_search`
/// constructs internally.
/// What: a unit-like enum with serde-stage + label helpers.
/// Test: per-tool routing exercised in the main test body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Lexical,
    Semantic,
    Kg,
    All,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Tool::Lexical => "search_lexical",
            Tool::Semantic => "search_semantic",
            Tool::Kg => "search_kg",
            Tool::All => "search_all",
        }
    }

    /// Value sent in the search request body's `stage` field, or `None`
    /// to omit the field entirely (adaptive routing).
    fn stage_value(self) -> Option<&'static str> {
        match self {
            Tool::Lexical => Some("lexical"),
            Tool::Semantic => Some("semantic"),
            Tool::Kg => Some("graph"),
            Tool::All => None,
        }
    }

    /// Whether to set `expand_graph: true` in the request body.
    /// Mirrors `SearchLane::expand_graph_default` in the production code.
    fn expand_graph(self) -> bool {
        matches!(self, Tool::Kg)
    }
}

const ALL_TOOLS: &[Tool] = &[Tool::Lexical, Tool::Semantic, Tool::Kg, Tool::All];

/// Result for one (query, tool) pair.
///
/// Why: collecting every result into a Vec lets the summary tables iterate
/// over any slice (by tool, by type, aggregate) without re-querying.
/// What: hit booleans + latencies + diagnostic fields.
/// Test: populated inside `run_query`; assertions in the main test body.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `query_text` and `match_reason` retained for failure prints.
struct QueryResult {
    query_id: String,
    query_text: String,
    query_type: String,
    tool: Tool,
    top_files: Vec<String>,
    hit_at_1: bool,
    hit_at_5: bool,
    intent: String,
    match_reason: String,
    server_latency_ms: Option<u64>,
    client_latency_ms: u128,
    /// For kg_seed queries, the chunk_id resolved in stage-1. None for
    /// non-KG queries or when the seed lookup yielded nothing.
    kg_seed_chunk_id: Option<String>,
}

// ── Path helpers ────────────────────────────────────────────────────────────

/// Absolute path of the trusty-agents crate root.
///
/// Why: the daemon needs an absolute path; deriving it from
/// `CARGO_MANIFEST_DIR` keeps the harness portable.
/// What: trusty-search's manifest dir + `../trusty-agents`.
/// Test: `ground_truth_path()` and `open_mpm_root()` resolve to existing
/// directories — `register_index` panics on a non-existent root.
fn open_mpm_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("trusty-search manifest dir must have a parent (crates/)")
        .join("trusty-agents")
}

/// Absolute path of the ground-truth JSON file.
fn ground_truth_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("tests")
        .join("benchmark_open_mpm_ground_truth.json")
}

// ── Ground-truth loader ─────────────────────────────────────────────────────

/// Load and parse the open-mpm ground-truth file.
///
/// Why: centralises JSON field access so the rest of the harness is typed.
/// What: reads the JSON, parses every `queries[]` entry, returns the list.
/// Test: panics with a clear message if the file is missing or malformed.
fn load_ground_truth() -> Vec<GroundTruthQuery> {
    let path = ground_truth_path();
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read ground-truth file at {path:?}: {e}"));
    let raw: Value = serde_json::from_slice(&bytes).expect("ground-truth file must be valid JSON");
    let queries = raw["queries"].as_array().expect("queries array required");
    queries
        .iter()
        .map(|q| GroundTruthQuery {
            id: q["id"].as_str().expect("id required").to_string(),
            text: q["text"].as_str().expect("text required").to_string(),
            query_type: q["type"].as_str().expect("type required").to_string(),
            mode_hint: q["mode_hint"].as_str().unwrap_or("code").to_string(),
            ground_truth_files: q["ground_truth_files"]
                .as_array()
                .expect("ground_truth_files required")
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            kg_seed_query: q["kg_seed_query"].as_str().map(str::to_owned),
            description: q["description"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

/// Build a reqwest client with timeouts tuned for a long-running benchmark.
///
/// Why: KG expansion can be slow on the first call; the synthetic harness
/// uses a 30 s ceiling, but indexing an organic corpus may need more.
/// What: 5 s connect, 60 s request.
/// Test: any transport error panics via `.expect()` in callers.
fn make_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("reqwest client construction is infallible")
}

/// Sanity-check that the daemon is up and report its version.
///
/// Why: a clear early error is much better than a confusing transport
/// failure 5 minutes into the reindex.
/// What: GETs /health, asserts 200, prints `version` + `rss_mb`.
/// Test: panics with a human-readable message if the daemon is unreachable.
async fn assert_daemon_healthy(client: &Client) -> Value {
    let resp = client
        .get(format!("{DAEMON_URL}/health"))
        .send()
        .await
        .expect("daemon must be reachable at 127.0.0.1:7878 — start it with `trusty-search start`");
    assert_eq!(resp.status().as_u16(), 200, "GET /health returned non-200");
    let body: Value = resp.json().await.expect("health JSON parse failure");
    println!(
        "    daemon healthy: version={}, indexes={}, rss_mb={}",
        body["version"].as_str().unwrap_or("?"),
        body["indexes"].as_u64().unwrap_or(0),
        body["rss_mb"].as_u64().unwrap_or(0),
    );
    body
}

/// Fetch /health and return current RSS (MB) for memory-watching.
///
/// Why: the harness samples RSS every 15 s during reindex and bails if it
/// exceeds `RSS_BAIL_MB`.
/// What: GET /health, returns `rss_mb` (0 on parse failure).
/// Test: transport errors panic; callers consume the u64.
async fn fetch_rss_mb(client: &Client) -> u64 {
    match client.get(format!("{DAEMON_URL}/health")).send().await {
        Ok(r) => match r.json::<Value>().await {
            Ok(v) => v["rss_mb"].as_u64().unwrap_or(0),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

/// Register (or re-register) the open-mpm-benchmark index.
///
/// Why: starting from a clean slate prevents stale chunk data from a
/// previous run from inflating Hit@K scores.
/// What: DELETEs any existing index with this name, then POSTs to /indexes.
/// Test: asserts 200 on POST; transport errors panic.
async fn register_index(client: &Client) {
    let _ = client
        .delete(format!("{DAEMON_URL}/indexes/{INDEX_NAME}"))
        .send()
        .await;

    let root = open_mpm_root();
    assert!(
        root.is_dir(),
        "trusty-agents crate root does not exist at {root:?} — wrong workspace layout?"
    );
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

/// Trigger a full force-reindex and wait for every stage to reach `ready`,
/// sampling daemon RSS during the wait.
///
/// Why: queries against a partially-indexed corpus produce misleading
/// Hit@K. On open-mpm we expect ~3–5 min wall-clock; we also need to bail
/// gracefully if memory pressure is unexpected.
/// What: POSTs /reindex with force=true, then polls /status until the
/// three stages (lexical, semantic, graph) all report `status="ready"`.
/// Every `RSS_SAMPLE_INTERVAL` it prints the RSS trajectory; bails if RSS
/// exceeds `RSS_BAIL_MB`. Returns the final chunk_count and elapsed time.
///
/// Test: panics with last-known status on `REINDEX_TIMEOUT` or memory bail.
async fn reindex_and_wait(client: &Client) -> (u64, Duration, u64) {
    let root = open_mpm_root();
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

    let start = Instant::now();
    let mut last_rss_sample = Instant::now() - RSS_SAMPLE_INTERVAL;
    let mut peak_rss_mb: u64 = 0;
    println!(
        "    reindex started; sampling RSS every {}s...",
        RSS_SAMPLE_INTERVAL.as_secs()
    );

    loop {
        if start.elapsed() > REINDEX_TIMEOUT {
            let status = fetch_status(client).await;
            panic!(
                "open-mpm-benchmark reindex did not reach Ready within {:?}\nlast status: {status}",
                REINDEX_TIMEOUT
            );
        }

        // RSS sample.
        if last_rss_sample.elapsed() >= RSS_SAMPLE_INTERVAL {
            let rss = fetch_rss_mb(client).await;
            if rss > peak_rss_mb {
                peak_rss_mb = rss;
            }
            println!(
                "      t+{:>4.0}s  rss={:>5} MB  peak={:>5} MB",
                start.elapsed().as_secs_f64(),
                rss,
                peak_rss_mb
            );
            if rss > RSS_BAIL_MB {
                panic!(
                    "daemon RSS exceeded {RSS_BAIL_MB} MB during open-mpm reindex \
                     (rss={rss}, peak={peak_rss_mb}); bailing to avoid the daemon's own ceiling"
                );
            }
            last_rss_sample = Instant::now();
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
            let elapsed = start.elapsed();
            // Take one final RSS sample so peak captures any late-stage burst.
            let final_rss = fetch_rss_mb(client).await;
            if final_rss > peak_rss_mb {
                peak_rss_mb = final_rss;
            }
            println!(
                "    reindex complete: chunks={chunks}, elapsed={:.1}s, peak_rss={peak_rss_mb} MB",
                elapsed.as_secs_f64()
            );
            return (chunks, elapsed, peak_rss_mb);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Fetch /indexes/:id/status.
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

/// Delete the benchmark index. Best-effort.
///
/// Why: cleanup keeps the developer's daemon registry tidy between runs.
/// What: DELETE /indexes/:id; prints the response status regardless.
/// Test: failures print, not panic (cleanup is best-effort).
async fn cleanup_index(client: &Client) {
    let resp = client
        .delete(format!("{DAEMON_URL}/indexes/{INDEX_NAME}"))
        .send()
        .await;
    match resp {
        Ok(r) => println!(
            "  cleanup DELETE /indexes/{INDEX_NAME} → {}",
            r.status().as_u16()
        ),
        Err(e) => println!("  cleanup DELETE failed: {e}"),
    }
}

// ── Query execution ─────────────────────────────────────────────────────────

/// Run one query in one tool mode and record the result.
///
/// Why: the four per-lane tools must run the same code path so results are
/// directly comparable; only the request body's `stage` / `expand_graph`
/// differ. For `kg_seed` queries with `Tool::Kg`, this performs the
/// two-stage pattern: stage-1 lexical to seed the chunk_id, stage-2
/// search_kg with the seed.
/// What: builds the request body matching the production
///   `mcp::tools::run_lane_search` shape; POSTs to
///   /indexes/:id/search; records hits / latencies / top files.
/// Test: transport failures panic; per-tool routing covered by per-query
/// asserts in the main body.
async fn run_query(client: &Client, query: &GroundTruthQuery, tool: Tool) -> QueryResult {
    // KG seed handling: only Tool::Kg on a kg_seed query exercises the two-
    // stage pattern. Other tool/query combinations fall through to the
    // standard single-stage path so we can compare each tool on the same
    // input directly.
    let mut kg_seed_chunk_id: Option<String> = None;
    if tool == Tool::Kg && query.query_type == "kg_seed" {
        if let Some(seed_text) = &query.kg_seed_query {
            kg_seed_chunk_id = resolve_kg_seed(client, seed_text).await;
        }
    }

    let mut body = json!({
        "text": query.text,
        "top_k": 10,
        "compact": false,
        "mode": query.mode_hint,
    });
    if let Some(stage) = tool.stage_value() {
        body["stage"] = json!(stage);
    }
    body["expand_graph"] = json!(tool.expand_graph());

    // For the KG two-stage path, attach the seed so the daemon's graph
    // expansion has an explicit anchor. The daemon also falls back to
    // intent-based expansion when seed_chunk_id is absent, but supplying
    // it makes the test deterministic.
    if let Some(seed) = &kg_seed_chunk_id {
        body["seed_chunk_id"] = json!(seed);
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
        .filter_map(|r| r["file"].as_str().map(normalise_path))
        .collect();

    let hit_at_1 = top_files
        .first()
        .map(|f| any_match(f, &query.ground_truth_files))
        .unwrap_or(false);
    let hit_at_5 = top_files
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
        query_type: query.query_type.clone(),
        tool,
        top_files,
        hit_at_1,
        hit_at_5,
        intent,
        match_reason,
        server_latency_ms,
        client_latency_ms,
        kg_seed_chunk_id,
    }
}

/// Stage-1 helper for `search_kg`: resolve a seed chunk_id by lexical
/// lookup of the seed query string.
///
/// Why: `search_kg` is most useful with an explicit seed. The production
/// MCP tool exposes a `seed_chunk_id` parameter; the harness mirrors
/// that contract by doing the lexical pre-search itself.
/// What: POSTs /search with `stage=lexical` and `top_k=1`, returns the
///   first result's `id` field, or None if nothing was found.
/// Test: implicit — kg_seed queries' KG runs succeed iff the seed
///   resolves.
async fn resolve_kg_seed(client: &Client, seed_text: &str) -> Option<String> {
    let body = json!({
        "text": seed_text,
        "top_k": 1,
        "stage": "lexical",
        "compact": false,
    });
    let resp = client
        .post(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/search"))
        .json(&body)
        .send()
        .await
        .ok()?;
    let json_body: Value = resp.json().await.ok()?;
    let id = json_body["results"]
        .as_array()?
        .first()?
        .get("id")?
        .as_str()?
        .to_string();
    Some(id)
}

/// Normalise an absolute or repo-relative path to the form used by
/// ground_truth_files (paths relative to the open-mpm crate root).
///
/// Why: the daemon may return absolute paths or relative paths depending
/// on index registration; normalising removes the ambiguity.
/// What: strips leading "./" and everything up to and including the
///   "trusty-agents/" segment if it appears.
/// Test: `any_match` relies on this for ground-truth comparison.
fn normalise_path(file: &str) -> String {
    let trimmed = file.trim_start_matches("./");
    // If the daemon reported an absolute path inside crates/trusty-agents/, strip
    // everything up to and including that segment.
    if let Some(idx) = trimmed.find("trusty-agents/") {
        let after = &trimmed[idx + "trusty-agents/".len()..];
        return after.to_string();
    }
    trimmed.to_string()
}

/// Returns true if `result_file` matches any entry in `ground_truth_files`.
///
/// Why: paths may differ in root prefix; an ends_with check is more robust
/// than exact equality.
/// What: equality, path suffix, or slash-prefixed suffix.
/// Test: covered implicitly by the Hit@K assertions in the main body.
fn any_match(result_file: &str, ground_truth_files: &[String]) -> bool {
    ground_truth_files.iter().any(|truth| {
        result_file == truth
            || result_file.ends_with(truth)
            || result_file.ends_with(&format!("/{truth}"))
    })
}

// ── Result tables ───────────────────────────────────────────────────────────

/// Print the headline analytical table — per-tool Hit@K with per-type
/// breakdown plus aggregate H@1, H@5, and p50 server latency.
///
/// Why: this is the deliverable artifact. Each tool gets one row; the
/// columns are the three meaningful query categories plus aggregate.
/// What: rows = tools, columns = Definition / Conceptual / KG / Aggregate.
///   Negative queries are excluded from H@K aggregates (vacuously true)
///   and reported separately below.
/// Test: visual inspection — numbers land in the baseline doc.
fn print_per_tool_table(results: &[QueryResult]) {
    println!("\n## Per-tool Hit@K with per-type breakdown\n");
    println!(
        "| {:<16} | {:^14} | {:^14} | {:^14} | {:^14} | {:^14} | {:>13} |",
        "Tool", "Def H@1", "Concept H@1", "KG H@1", "Agg H@1", "Agg H@5", "p50 srv ms",
    );
    println!(
        "|{:-<18}|{:-<16}|{:-<16}|{:-<16}|{:-<16}|{:-<16}|{:-<15}|",
        "", "", "", "", "", "", ""
    );

    for tool in ALL_TOOLS.iter().copied() {
        let mode_results: Vec<&QueryResult> = results
            .iter()
            .filter(|r| r.tool == tool && r.query_type != "negative")
            .collect();

        let def_h1 = subset_h1(&mode_results, "definition");
        let con_h1 = subset_h1(&mode_results, "conceptual");
        let kg_h1 = subset_h1(&mode_results, "kg_seed");

        let n = mode_results.len();
        let agg_h1 = mode_results.iter().filter(|r| r.hit_at_1).count();
        let agg_h5 = mode_results.iter().filter(|r| r.hit_at_5).count();

        let mut srv: Vec<u64> = mode_results
            .iter()
            .filter_map(|r| r.server_latency_ms)
            .collect();
        srv.sort_unstable();
        let p50 = if srv.is_empty() {
            0
        } else {
            srv[srv.len() / 2]
        };

        println!(
            "| {:<16} | {:^14} | {:^14} | {:^14} | {:^14} | {:^14} | {:>13} |",
            tool.label(),
            def_h1,
            con_h1,
            kg_h1,
            format!("{agg_h1}/{n}"),
            format!("{agg_h5}/{n}"),
            p50,
        );
    }

    println!("\n  Format per cell: Hit@1/total (negative queries excluded from aggregates)");
}

/// Helper: format `"hit/total"` for a query-type subset of one tool's
/// results.
fn subset_h1(results: &[&QueryResult], query_type: &str) -> String {
    let subset: Vec<&&QueryResult> = results
        .iter()
        .filter(|r| r.query_type == query_type)
        .collect();
    if subset.is_empty() {
        return "n/a".into();
    }
    let n = subset.len();
    let h1 = subset.iter().filter(|r| r.hit_at_1).count();
    format!("{h1}/{n}")
}

/// Print the per-query results table for forensic review.
///
/// Why: per-query detail isolates which specific queries drive the
/// tool-level differences that the aggregate table obscures.
/// What: one row per (query, tool) with H@1, H@5, latencies, top file.
/// Test: visual inspection.
fn print_per_query_table(results: &[QueryResult]) {
    println!("\n## Per-query results\n");
    println!(
        "| {:<4} | {:<16} | {:<11} | {:>4} | {:>4} | {:>8} | {:>9} | {:<14} | {:<14} |",
        "ID", "Tool", "Type", "H@1", "H@5", "srv ms", "client ms", "Intent", "Top-1 file"
    );
    println!(
        "|{:-<6}|{:-<18}|{:-<13}|{:-<6}|{:-<6}|{:-<10}|{:-<11}|{:-<16}|{:-<16}|",
        "", "", "", "", "", "", "", "", ""
    );
    for r in results {
        let h1 = if r.hit_at_1 { "Y" } else { "-" };
        let h5 = if r.hit_at_5 { "Y" } else { "-" };
        let srv = r
            .server_latency_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let top1 = r
            .top_files
            .first()
            .cloned()
            .unwrap_or_else(|| "<none>".into());
        // Truncate the top-1 file display so the table stays readable.
        let top1_short: String = top1.chars().take(40).collect();
        println!(
            "| {:<4} | {:<16} | {:<11} | {:>4} | {:>4} | {:>8} | {:>9} | {:<14} | {:<14} |",
            r.query_id,
            r.tool.label(),
            r.query_type,
            h1,
            h5,
            srv,
            r.client_latency_ms,
            r.intent,
            top1_short,
        );
    }
}

/// Print the negative-case footer.
///
/// Why: negative queries shouldn't count as misses in the aggregate, but
/// we still want to see what each tool returned — a false-positive top-1
/// match is a useful signal.
/// What: one block per negative query showing the top-1 each tool surfaced.
/// Test: visual inspection.
fn print_negative_footer(results: &[QueryResult]) {
    let neg_ids: Vec<String> = results
        .iter()
        .filter(|r| r.query_type == "negative")
        .map(|r| r.query_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if neg_ids.is_empty() {
        return;
    }
    println!("\n## Negative-case diagnostics (these should return zero/empty gracefully)\n");
    for id in neg_ids {
        let rows: Vec<&QueryResult> = results
            .iter()
            .filter(|r| r.query_id == id && r.query_type == "negative")
            .collect();
        if let Some(first) = rows.first() {
            println!(
                "  {} \"{}\":",
                first.query_id,
                &first.query_text[..first.query_text.len().min(60)]
            );
        }
        for r in rows {
            let top1 = r
                .top_files
                .first()
                .cloned()
                .unwrap_or_else(|| "<empty>".into());
            println!(
                "    {:<16}  top1={}  intent={}",
                r.tool.label(),
                top1,
                r.intent,
            );
        }
    }
}

// ── The test ────────────────────────────────────────────────────────────────

/// Index `crates/open-mpm/`, run every ground-truth query through the four
/// v0.10.0 per-lane MCP tools, print the analytical tables, clean up.
///
/// Why: this is the first organic-corpus measurement of trusty-search's
/// per-lane tools (#5). It answers the question the synthetic corpus
/// couldn't: does `search_kg` differ meaningfully from `search_semantic`
/// on a real Rust workspace?
/// What: the steps documented in the file-level comment.
/// Test: this IS the test.
#[tokio::test]
#[ignore]
async fn benchmark_open_mpm_per_lane_tools() {
    let client = make_client();
    let health = assert_daemon_healthy(&client).await;
    let daemon_version = health["version"].as_str().unwrap_or("?").to_string();

    println!("\n=== open-mpm-benchmark, per-lane MCP tool evaluation (v0.10.0) ===");
    println!("daemon version: {daemon_version}");
    println!("corpus root: {}", open_mpm_root().display());

    let queries = load_ground_truth();
    println!("loaded {} ground-truth queries", queries.len());

    println!("\nregistering index '{INDEX_NAME}'...");
    register_index(&client).await;

    println!("triggering force-reindex and waiting for Ready...");
    let (chunk_count, reindex_elapsed, peak_rss_mb) = reindex_and_wait(&client).await;

    let status = fetch_status(&client).await;
    let stages: Value = status["stages"].clone();
    let search_capabilities: Vec<String> = status["search_capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let mut all_results: Vec<QueryResult> = Vec::with_capacity(queries.len() * ALL_TOOLS.len());
    for tool in ALL_TOOLS.iter().copied() {
        println!("\n--- tool = {} ---", tool.label());
        for q in &queries {
            let result = run_query(&client, q, tool).await;
            let seed_label = result
                .kg_seed_chunk_id
                .as_deref()
                .map(|s| format!(" seed={s}"))
                .unwrap_or_default();
            println!(
                "  {} [{}] type={} mode={}: H@1={} H@5={} top1={}{}",
                q.id,
                if q.text.len() > 50 {
                    format!("{}…", &q.text[..50])
                } else {
                    q.text.clone()
                },
                q.query_type,
                q.mode_hint,
                if result.hit_at_1 { "Y" } else { "-" },
                if result.hit_at_5 { "Y" } else { "-" },
                result
                    .top_files
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "<none>".into()),
                seed_label,
            );
            all_results.push(result);
        }
    }

    print_per_tool_table(&all_results);
    print_per_query_table(&all_results);
    print_negative_footer(&all_results);

    println!("\n## Diagnostics");
    println!("- daemon version: {daemon_version}");
    println!("- chunk_count: {chunk_count}");
    println!("- reindex_elapsed_s: {:.1}", reindex_elapsed.as_secs_f64());
    println!("- peak_rss_mb: {peak_rss_mb}");
    println!("- search_capabilities: {search_capabilities:?}");
    println!("- stages: {stages}");

    // Sanity assert: the harness should not be silently broken. At least
    // one positive query must hit at H@5 on at least one tool. Negative
    // queries are excluded.
    let total_hits = all_results
        .iter()
        .filter(|r| r.query_type != "negative" && r.hit_at_5)
        .count();
    assert!(
        total_hits > 0,
        "every non-negative query missed at H@5 across every tool — daemon may be misconfigured"
    );

    cleanup_index(&client).await;
}

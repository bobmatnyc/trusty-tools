//! KG-targeted benchmark harness for trusty-search v0.10.0 per-lane MCP
//! tools — the #145 retrieval-lift determination.
//!
//! Why: the existing `benchmark_open_mpm.rs` harness (#5) ran 20 mixed-intent
//! queries against open-mpm and found `search_kg` ≡ `search_semantic` on
//! every Hit@K metric. Only 3/20 queries were KG-relevant, so the negative
//! finding may simply reflect query-set undersampling rather than a genuine
//! absence of KG signal. This harness authors 18 queries SPECIFICALLY DESIGNED
//! to exercise the symbol graph — every query asks "who calls X", "what
//! implements T", or "what is the call-neighborhood of Y", structurally
//! requiring a graph walk. If `search_kg` still doesn't lift Hit@K here, the
//! KG signal is dead-code retrieval ornament and Stage 3's reindex cost
//! buys provenance + `get_call_chain` but nothing for top-K retrieval.
//!
//! What: reads `benchmark_open_mpm_kg_ground_truth.json` (18 queries across
//! 4 classes: kg_callers / kg_traversal / kg_impl_of / kg_neighborhood),
//! reuses the `open-mpm-benchmark` index from the prior #5 run if it
//! persists (otherwise creates + reindexes), runs each query through the
//! four per-lane MCP tool equivalents using the two-stage seed pattern
//! (stage-1 lexical seeds the chunk_id, stage-2 fires the chosen lane),
//! and reports per-class Hit@K. The critical comparison is
//! `search_kg.hit_at_1 - search_semantic.hit_at_1` on the KG-targeted
//! subset. If ≥ 10 pp on average across the four classes, KG signal earns
//! its keep; if under 5 pp, deprecate Stage 3's ranking signal.
//!
//! Per-tool HTTP mapping (matches `mcp::tools::run_lane_search`):
//!   - `search_lexical`  → `stage="lexical"`, `expand_graph=false`
//!   - `search_semantic` → `stage="semantic"`, `expand_graph=false`
//!   - `search_kg`       → `stage="graph"`, `expand_graph=true`
//!   - `search_all`      → no `stage` field, `expand_graph=false`
//!
//! Test: gated `#[ignore]` so it does not run during default `cargo test`.
//! Run with:
//!   cargo test --test benchmark_open_mpm_kg -- --include-ignored --nocapture
//!
//! Prerequisites:
//!   - trusty-search daemon running at `http://127.0.0.1:7878` (v0.10.0+)
//!   - `crates/open-mpm/` source tree present (in-tree workspace member)
//!
//! Like `benchmark_open_mpm.rs`, this harness does NOT spin up its own
//! daemon — it uses the developer's already-running instance. Unlike that
//! harness, it does NOT delete the index at the end (so subsequent #145
//! re-runs are fast); a follow-up cleanup task may drop it explicitly.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

// ── Constants ───────────────────────────────────────────────────────────────

const DAEMON_URL: &str = "http://127.0.0.1:7878";

/// Why: same name as `benchmark_open_mpm.rs` so we can REUSE its index if
/// it persists — the harness checks /status first and skips reindex when
/// every stage is `ready`. open-mpm reindex takes ~2.5 min and 22 GB peak
/// RSS, so reuse is a big win.
const INDEX_NAME: &str = "open-mpm-benchmark";

const REINDEX_TIMEOUT: Duration = Duration::from_secs(900);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_secs(15);
const RSS_BAIL_MB: u64 = 28_672;

// ── Types ───────────────────────────────────────────────────────────────────

/// One ground-truth entry. All KG-targeted queries are positive (no
/// `negative` class) — the goal is to measure lift, not robustness.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `description` retained for forensic prints.
struct GroundTruthQuery {
    id: String,
    /// kg_callers / kg_traversal / kg_impl_of / kg_neighborhood
    class: String,
    text: String,
    /// Stage-1 lexical seed text used for every (query, tool) pair so the
    /// graph traversal in stage-2 has a deterministic anchor.
    seed_query: String,
    /// Expected stage-1 landing file (informational — not enforced).
    seed_target_file: Option<String>,
    mode_hint: String,
    /// Files (relative to the open-mpm crate root) considered correct.
    ground_truth_files: Vec<String>,
    description: String,
}

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

    fn stage_value(self) -> Option<&'static str> {
        match self {
            Tool::Lexical => Some("lexical"),
            Tool::Semantic => Some("semantic"),
            Tool::Kg => Some("graph"),
            Tool::All => None,
        }
    }

    fn expand_graph(self) -> bool {
        matches!(self, Tool::Kg)
    }
}

const ALL_TOOLS: &[Tool] = &[Tool::Lexical, Tool::Semantic, Tool::Kg, Tool::All];

/// All four KG-targeted classes — used for the per-class table.
const ALL_CLASSES: &[&str] = &[
    "kg_callers",
    "kg_traversal",
    "kg_impl_of",
    "kg_neighborhood",
];

#[derive(Debug, Clone)]
#[allow(dead_code)] // query_text + match_reason used for forensic prints.
struct QueryResult {
    query_id: String,
    query_text: String,
    class: String,
    tool: Tool,
    top_files: Vec<String>,
    hit_at_1: bool,
    hit_at_5: bool,
    /// Number of distinct ground-truth files that appear in top-5 (for
    /// neighborhood / impl-of queries this is a stronger signal than H@5).
    distinct_truth_hits_at_5: usize,
    intent: String,
    match_reason: String,
    server_latency_ms: Option<u64>,
    client_latency_ms: u128,
    /// Stage-1-resolved chunk_id, for the audit log.
    kg_seed_chunk_id: Option<String>,
    /// File of the stage-1 seed chunk (so we can see WHERE the seed landed).
    kg_seed_file: Option<String>,
}

// ── Path helpers ────────────────────────────────────────────────────────────

fn open_mpm_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("trusty-search manifest dir must have a parent (crates/)")
        .join("trusty-agents")
}

fn ground_truth_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("tests")
        .join("benchmark_open_mpm_kg_ground_truth.json")
}

// ── Ground-truth loader ─────────────────────────────────────────────────────

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
            class: q["class"].as_str().expect("class required").to_string(),
            text: q["text"].as_str().expect("text required").to_string(),
            seed_query: q["seed_query"]
                .as_str()
                .expect("seed_query required for every KG-targeted query")
                .to_string(),
            seed_target_file: q["seed_target_file"].as_str().map(str::to_owned),
            mode_hint: q["mode_hint"].as_str().unwrap_or("code").to_string(),
            ground_truth_files: q["ground_truth_files"]
                .as_array()
                .expect("ground_truth_files required")
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            description: q["description"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

fn make_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("reqwest client construction is infallible")
}

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

async fn fetch_rss_mb(client: &Client) -> u64 {
    match client.get(format!("{DAEMON_URL}/health")).send().await {
        Ok(r) => match r.json::<Value>().await {
            Ok(v) => v["rss_mb"].as_u64().unwrap_or(0),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

/// Probe whether the `open-mpm-benchmark` index exists AND has every stage
/// ready. Returns `Some((chunk_count, root_matches))` when reuse is safe;
/// `None` when reindex is needed.
///
/// Why: open-mpm reindex takes ~2.5 min + 22 GB peak RSS. If the prior #5
/// run left the index in `ready` state pointing at the same workspace path,
/// reuse it.
/// What: GETs /indexes/:id/status; checks `stages.{lexical,semantic,graph}`
/// all `ready` and `root_path` matches our open_mpm_root().
/// Test: implicit — main() prints the reuse decision so the operator sees
/// which path was taken.
async fn probe_existing_index(client: &Client) -> Option<u64> {
    let resp = client
        .get(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/status"))
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let stages_ready = body["stages"].is_object()
        && ["lexical", "semantic", "graph"].iter().all(|stage| {
            body["stages"][stage]["status"]
                .as_str()
                .map(|s| s == "ready")
                .unwrap_or(false)
        });
    if !stages_ready {
        return None;
    }
    // Verify the registered root matches our workspace path. If a previous
    // run registered the index at a different path (e.g. an older sibling
    // checkout), we must not reuse it — the file paths would not match the
    // ground-truth set.
    let registered = body["root_path"].as_str()?;
    let expected = open_mpm_root();
    let expected_str = expected.to_string_lossy();
    // Permit equality or suffix match (the daemon may canonicalise).
    if registered != expected_str && !registered.ends_with("crates/trusty-agents") {
        println!(
            "    existing index root '{registered}' does not match expected '{expected_str}' — \
             will recreate"
        );
        return None;
    }
    let chunks = body["chunk_count"].as_u64().unwrap_or(0);
    Some(chunks)
}

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

async fn fetch_status(client: &Client) -> Value {
    let resp = client
        .get(format!("{DAEMON_URL}/indexes/{INDEX_NAME}/status"))
        .send()
        .await
        .expect("GET /status transport failure");
    resp.json().await.expect("status JSON parse failure")
}

// ── Query execution ─────────────────────────────────────────────────────────

/// Stage-1 helper: resolve a seed chunk_id by lexical lookup of the seed
/// query string. Returns the (chunk_id, file) pair so the harness can log
/// where the seed landed — useful for diagnosing wrong-seed KG drift.
async fn resolve_kg_seed(client: &Client, seed_text: &str) -> Option<(String, String)> {
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
    let first = json_body["results"].as_array()?.first()?;
    let id = first.get("id")?.as_str()?.to_string();
    let file = first
        .get("file")
        .and_then(|v| v.as_str())
        .map(normalise_path)
        .unwrap_or_default();
    Some((id, file))
}

async fn run_query(client: &Client, query: &GroundTruthQuery, tool: Tool) -> QueryResult {
    // Every KG-targeted query gets a stage-1 seed lookup. The seed_chunk_id
    // is currently silently ignored by the daemon's SearchQuery struct, but
    // we still resolve it for forensic logging (to see WHERE stage-1 landed).
    // This matches the contract of `benchmark_open_mpm.rs` and keeps the
    // two-stage pattern explicit and auditable.
    let seed = resolve_kg_seed(client, &query.seed_query).await;
    let (kg_seed_chunk_id, kg_seed_file) = match seed.clone() {
        Some((id, file)) => (Some(id), Some(file)),
        None => (None, None),
    };

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
    if let Some(seed_id) = &kg_seed_chunk_id {
        // Daemon currently ignores this field. Sent for future compat and
        // forensic-log clarity — the request payload should record the
        // seed even when it isn't yet honoured server-side.
        body["seed_chunk_id"] = json!(seed_id);
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
    let distinct_truth_hits_at_5 = {
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for f in top_files.iter().take(5) {
            if let Some(truth) = query
                .ground_truth_files
                .iter()
                .find(|t| f == *t || f.ends_with(*t) || f.ends_with(&format!("/{t}")))
            {
                seen.insert(truth.as_str());
            }
        }
        seen.len()
    };

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
        class: query.class.clone(),
        tool,
        top_files,
        hit_at_1,
        hit_at_5,
        distinct_truth_hits_at_5,
        intent,
        match_reason,
        server_latency_ms,
        client_latency_ms,
        kg_seed_chunk_id,
        kg_seed_file,
    }
}

fn normalise_path(file: &str) -> String {
    let trimmed = file.trim_start_matches("./");
    if let Some(idx) = trimmed.find("trusty-agents/") {
        let after = &trimmed[idx + "trusty-agents/".len()..];
        return after.to_string();
    }
    trimmed.to_string()
}

fn any_match(result_file: &str, ground_truth_files: &[String]) -> bool {
    ground_truth_files.iter().any(|truth| {
        result_file == truth
            || result_file.ends_with(truth)
            || result_file.ends_with(&format!("/{truth}"))
    })
}

// ── Result tables ───────────────────────────────────────────────────────────

/// Per-class × per-tool Hit@1 / Hit@5 table. This is THE answer to #145.
///
/// Why: the #145 question is "does search_kg lift Hit@K on KG-targeted
/// queries". This table puts all four tools on each of the four KG classes
/// side-by-side; the search_kg row minus the search_semantic row across
/// the four class columns IS the verdict.
fn print_per_class_table(results: &[QueryResult]) {
    println!("\n## Per-class Hit@1 / Hit@5 by tool (the #145 verdict table)\n");
    print!("| {:<16} ", "Tool");
    for class in ALL_CLASSES {
        print!("| {:<22} ", format!("{class} (H@1 / H@5)"));
    }
    println!("| {:<22} |", "Aggregate (H@1 / H@5)");
    print!("|{:-<18}", "");
    for _ in ALL_CLASSES {
        print!("|{:-<24}", "");
    }
    println!("|{:-<24}|", "");

    for tool in ALL_TOOLS.iter().copied() {
        let tool_results: Vec<&QueryResult> = results.iter().filter(|r| r.tool == tool).collect();
        print!("| {:<16} ", tool.label());
        for class in ALL_CLASSES {
            let subset: Vec<&&QueryResult> =
                tool_results.iter().filter(|r| r.class == *class).collect();
            let n = subset.len();
            let h1 = subset.iter().filter(|r| r.hit_at_1).count();
            let h5 = subset.iter().filter(|r| r.hit_at_5).count();
            let cell = if n == 0 {
                "n/a".to_string()
            } else {
                format!("{h1}/{n} / {h5}/{n}")
            };
            print!("| {cell:<22} ");
        }
        let n = tool_results.len();
        let h1 = tool_results.iter().filter(|r| r.hit_at_1).count();
        let h5 = tool_results.iter().filter(|r| r.hit_at_5).count();
        println!("| {:<22} |", format!("{h1}/{n} / {h5}/{n}"));
    }

    println!("\n  Cell format: Hit@1/N / Hit@5/N.");
}

/// Print the headline delta: search_kg minus search_semantic on each
/// class, plus the aggregate delta.
///
/// Why: the determination boils down to a single number — the average
/// Hit@1 lift of search_kg over search_semantic on the KG-targeted set.
/// This table makes it impossible to miss.
fn print_delta_table(results: &[QueryResult]) {
    println!("\n## Headline: search_kg minus search_semantic (the #145 decision number)\n");
    println!(
        "| {:<16} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10} |",
        "Class", "kg H@1", "sem H@1", "Δ H@1", "kg H@5", "sem H@5", "Δ H@5"
    );
    println!(
        "|{:-<18}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|",
        "", "", "", "", "", "", ""
    );
    let mut sum_h1_kg = 0i32;
    let mut sum_h1_sem = 0i32;
    let mut sum_h5_kg = 0i32;
    let mut sum_h5_sem = 0i32;
    let mut total_n = 0i32;
    for class in ALL_CLASSES {
        let kg_subset: Vec<&QueryResult> = results
            .iter()
            .filter(|r| r.tool == Tool::Kg && r.class == *class)
            .collect();
        let sem_subset: Vec<&QueryResult> = results
            .iter()
            .filter(|r| r.tool == Tool::Semantic && r.class == *class)
            .collect();
        let n = kg_subset.len() as i32;
        let kg_h1 = kg_subset.iter().filter(|r| r.hit_at_1).count() as i32;
        let sem_h1 = sem_subset.iter().filter(|r| r.hit_at_1).count() as i32;
        let kg_h5 = kg_subset.iter().filter(|r| r.hit_at_5).count() as i32;
        let sem_h5 = sem_subset.iter().filter(|r| r.hit_at_5).count() as i32;
        let d1 = kg_h1 - sem_h1;
        let d5 = kg_h5 - sem_h5;
        println!(
            "| {:<16} | {:>10} | {:>10} | {:>+10} | {:>10} | {:>10} | {:>+10} |",
            class,
            format!("{kg_h1}/{n}"),
            format!("{sem_h1}/{n}"),
            d1,
            format!("{kg_h5}/{n}"),
            format!("{sem_h5}/{n}"),
            d5,
        );
        sum_h1_kg += kg_h1;
        sum_h1_sem += sem_h1;
        sum_h5_kg += kg_h5;
        sum_h5_sem += sem_h5;
        total_n += n;
    }
    let agg_d1 = sum_h1_kg - sum_h1_sem;
    let agg_d5 = sum_h5_kg - sum_h5_sem;
    println!(
        "|{:-<18}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|{:-<12}|",
        "", "", "", "", "", "", ""
    );
    println!(
        "| {:<16} | {:>10} | {:>10} | {:>+10} | {:>10} | {:>10} | {:>+10} |",
        "AGGREGATE",
        format!("{sum_h1_kg}/{total_n}"),
        format!("{sum_h1_sem}/{total_n}"),
        agg_d1,
        format!("{sum_h5_kg}/{total_n}"),
        format!("{sum_h5_sem}/{total_n}"),
        agg_d5,
    );
    if total_n > 0 {
        let pp_h1 = (agg_d1 as f64) / (total_n as f64) * 100.0;
        let pp_h5 = (agg_d5 as f64) / (total_n as f64) * 100.0;
        println!("\n  Aggregate Hit@1 lift: {pp_h1:+.1} pp   Aggregate Hit@5 lift: {pp_h5:+.1} pp");
        println!(
            "  Decision thresholds: ≥ +10 pp Hit@1 → KEEP   < +5 pp Hit@1 → DEPRECATE / PROVENANCE-ONLY"
        );
    }
}

/// Per-query result detail table for forensic review.
fn print_per_query_table(results: &[QueryResult]) {
    println!("\n## Per-query results (sorted by query id)\n");
    println!(
        "| {:<5} | {:<16} | {:<16} | {:>3} | {:>3} | {:>5} | {:>5} | {:<16} | {:<30} | {:<30} |",
        "ID",
        "Tool",
        "Class",
        "H@1",
        "H@5",
        "n_gt",
        "srv",
        "Intent",
        "Seed file (stage-1)",
        "Top-1 file (stage-2)"
    );
    println!(
        "|{:-<7}|{:-<18}|{:-<18}|{:-<5}|{:-<5}|{:-<7}|{:-<7}|{:-<18}|{:-<32}|{:-<32}|",
        "", "", "", "", "", "", "", "", "", ""
    );
    let mut ordered = results.to_vec();
    ordered.sort_by(|a, b| {
        a.query_id
            .cmp(&b.query_id)
            .then_with(|| a.tool.label().cmp(b.tool.label()))
    });
    for r in &ordered {
        let h1 = if r.hit_at_1 { "Y" } else { "-" };
        let h5 = if r.hit_at_5 { "Y" } else { "-" };
        let srv = r
            .server_latency_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let seed_file = r
            .kg_seed_file
            .clone()
            .unwrap_or_else(|| "<none>".into())
            .chars()
            .take(30)
            .collect::<String>();
        let top1 = r
            .top_files
            .first()
            .cloned()
            .unwrap_or_else(|| "<none>".into())
            .chars()
            .take(30)
            .collect::<String>();
        println!(
            "| {:<5} | {:<16} | {:<16} | {:>3} | {:>3} | {:>5} | {:>5} | {:<16} | {:<30} | {:<30} |",
            r.query_id,
            r.tool.label(),
            r.class,
            h1,
            h5,
            r.distinct_truth_hits_at_5,
            srv,
            r.intent,
            seed_file,
            top1,
        );
    }
    println!(
        "\n  n_gt = number of DISTINCT ground-truth files found in top-5 (stronger signal for impl-of + neighborhood)"
    );
}

// ── The test ────────────────────────────────────────────────────────────────

/// The #145 decision harness. Runs every KG-targeted query through every
/// per-lane tool and prints (a) the per-class verdict table, (b) the
/// search_kg − search_semantic delta table, and (c) the per-query forensic
/// detail.
#[tokio::test]
#[ignore]
async fn benchmark_open_mpm_kg_per_lane_tools() {
    let client = make_client();
    let health = assert_daemon_healthy(&client).await;
    let daemon_version = health["version"].as_str().unwrap_or("?").to_string();

    println!("\n=== open-mpm KG-targeted retrieval-lift determination (#145) ===");
    println!("daemon version: {daemon_version}");
    println!("corpus root: {}", open_mpm_root().display());

    let queries = load_ground_truth();
    println!(
        "loaded {} KG-targeted ground-truth queries across {} classes",
        queries.len(),
        ALL_CLASSES.len()
    );

    // Reuse the existing `open-mpm-benchmark` index if it's already ready.
    // The prior #5 run leaves it intact (cleanup_index was called there but
    // the in-memory registry may have been re-populated by a daemon restart);
    // when the daemon restarts the index re-loads from redb at startup.
    let (chunk_count, reindex_elapsed, peak_rss_mb) = match probe_existing_index(&client).await {
        Some(chunks) => {
            println!(
                    "    reusing existing '{INDEX_NAME}' index (chunk_count={chunks}, all stages ready)"
                );
            (chunks, Duration::from_secs(0), 0u64)
        }
        None => {
            println!("    no reusable index found — registering and reindexing...");
            register_index(&client).await;
            reindex_and_wait(&client).await
        }
    };

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
                .kg_seed_file
                .as_deref()
                .map(|s| format!(" seed_file={s}"))
                .unwrap_or_default();
            println!(
                "  {} [{}] class={}: H@1={} H@5={} n_gt={} top1={}{}",
                q.id,
                if q.text.len() > 50 {
                    format!("{}…", &q.text[..50])
                } else {
                    q.text.clone()
                },
                q.class,
                if result.hit_at_1 { "Y" } else { "-" },
                if result.hit_at_5 { "Y" } else { "-" },
                result.distinct_truth_hits_at_5,
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

    print_per_class_table(&all_results);
    print_delta_table(&all_results);
    print_per_query_table(&all_results);

    println!("\n## Diagnostics");
    println!("- daemon version: {daemon_version}");
    println!("- chunk_count: {chunk_count}");
    println!("- reindex_elapsed_s: {:.1}", reindex_elapsed.as_secs_f64());
    println!("- peak_rss_mb: {peak_rss_mb}");
    println!("- search_capabilities: {search_capabilities:?}");
    println!("- stages: {stages}");

    // Sanity assert: at least one tool must hit at H@5 on at least one
    // KG-targeted query. Catching a silently-broken harness.
    let total_hits = all_results.iter().filter(|r| r.hit_at_5).count();
    assert!(
        total_hits > 0,
        "every KG-targeted query missed at H@5 across every tool — harness or daemon misconfigured"
    );

    // Intentionally do NOT call cleanup_index — keep the index hot so #145
    // re-runs are fast. The operator can `DELETE /indexes/open-mpm-benchmark`
    // manually when done.
}

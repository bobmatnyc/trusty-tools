//! p95 load test for scoped analysis (#10781 closure condition 1).
//!
//! Why: #10787 added `paths` / `path_prefix` scoping to
//! `analyze.complexity_hotspots`, `analyze.smells` and `analyze.quality`. Its
//! acceptance test 1 (p95 under 10 s for a 20-file scope on an index of at
//! least 300K chunks, in-scope results only, a bounded page-fetch count) was
//! checked for shape only. The page retry policy allows about 121 s on one
//! slow scoped page, so only a measurement shows whether the target holds.
//!
//! What: `scoped_review_p95_under_target` runs N scoped review rounds against
//! a trusty-search daemon the operator names. One round sends the three
//! methods at once, in process through `build_router`, each with the same
//! 20-file `paths` scope, and is timed until all three answer. The analyzer
//! reaches trusty-search through a counting proxy socket that records every
//! `search.chunks.list` page and the rows it returned. The run fails when
//! p95 is 10 s or more, when any hotspot or smell lies outside the scope, when
//! a response does not echo the scope, when any page was unscoped, or when a
//! round's scoped pages exceed `page_fetch_bound`. It prints p50, p95 and max
//! per round and per method.
//!
//! N defaults to 40 and is set with `TRUSTY_ANALYZE_LOAD_CALLS` (at least 20).
//! Nearest-rank p95 over 40 samples is the 38th, so two slow rounds, such as
//! one page retried to its bound, are absorbed and a third fails the run. Over
//! 20 samples p95 is the 19th, and a single outlier decides it. 40 rounds at
//! the 10 s target take under 7 minutes. Two warm-up rounds run first and are
//! not counted, so a cold corpus on the daemon does not skew the sample.
//!
//! How to run. Never point it at the live daemon or the live `main` index
//! without the owner's or the Architect's approval.
//!
//! 1. Start an isolated daemon with `scripts/sandbox_search_daemon.sh --dir
//!    <DIR>`. It serves `<DIR>/data/trusty-search.sock` and prints that path.
//! 2. Build an index of at least 300,000 chunks in that sandbox. The test
//!    reads `search.index.status` and refuses a smaller index.
//! 3. Write 20 indexed file paths, one per line, to a scope file.
//! 4. Run, with all three variables set:
//!    `TRUSTY_ANALYZE_LOAD_SEARCH_SOCKET=<DIR>/data/trusty-search.sock
//!    TRUSTY_ANALYZE_LOAD_INDEX=<index id>
//!    TRUSTY_ANALYZE_LOAD_SCOPE_FILE=<scope file>
//!    cargo test -p trusty-analyze --test scoped_p95_bench -- --ignored
//!    --nocapture scoped_review_p95_under_target`
//!
//! The test refuses to run unless the socket, the index and the scope file are
//! set; it has no default target. The socket is a Unix socket path, because
//! trusty-search serves no HTTP listener since #9214. A target that is the
//! live daemon's default socket, or the index `main`, also needs
//! `TRUSTY_ANALYZE_LOAD_LIVE_APPROVAL` set to the approval reference.
//!
//! Test: `scoped_review_p95_under_target`,
//! `target_guard_refuses_unset_and_unapproved_live_targets`,
//! `percentile_uses_nearest_rank_over_synthetic_timings`,
//! `p95_verdict_absorbs_two_slow_rounds_in_forty_and_fails_on_three`,
//! `page_fetch_bound_counts_retries_and_refuses_an_unscoped_page`,
//! `results_outside_the_scope_are_reported`,
//! `a_round_through_the_counting_proxy_counts_every_scoped_page`.

#[path = "support/fake_search.rs"]
mod fake_search;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use trusty_analyze::core::{FactStore, PageRetryPolicy, ScipOverlayStore, TrustySearchClient};
use trusty_analyze::service::{build_router, AnalyzerAppState};
use trusty_common::search_rpc;
use trusty_common::uds::server::RpcRouter;

const ENV_SOCKET: &str = "TRUSTY_ANALYZE_LOAD_SEARCH_SOCKET";
const ENV_INDEX: &str = "TRUSTY_ANALYZE_LOAD_INDEX";
const ENV_SCOPE_FILE: &str = "TRUSTY_ANALYZE_LOAD_SCOPE_FILE";
const ENV_CALLS: &str = "TRUSTY_ANALYZE_LOAD_CALLS";
const ENV_LIVE_APPROVAL: &str = "TRUSTY_ANALYZE_LOAD_LIVE_APPROVAL";

/// The live index; reading it needs an approval reference.
const LIVE_INDEX: &str = "main";
/// Brief §4 test 1: a 20-file scope.
const SCOPE_FILES: usize = 20;
/// Brief §4 test 1: an index of at least 300K chunks.
const MIN_INDEX_CHUNKS: u64 = 300_000;
const DEFAULT_CALLS: usize = 40;
const MIN_CALLS: usize = 20;
const WARMUP_ROUNDS: usize = 2;
const P95_TARGET: Duration = Duration::from_secs(10);
/// Bound on one round, so a stuck walk fails the run instead of hanging it.
const ROUND_TIMEOUT: Duration = Duration::from_secs(300);
/// The three RPCs a PR review sends.
const METHODS: [&str; 3] = [
    "analyze.complexity_hotspots",
    "analyze.smells",
    "analyze.quality",
];
/// `top_n` / `limit` high enough that every scoped result is returned.
const RESULT_CAP: usize = 100_000;
const SEARCH_CHUNKS_LIST: &str = "search.chunks.list";

// ─── target guard ────────────────────────────────────────────────────────────

/// What the operator pointed the load test at.
#[derive(Debug, PartialEq)]
struct LoadTarget {
    socket: PathBuf,
    index_id: String,
    scope_file: PathBuf,
    calls: usize,
    live_approval: Option<String>,
}

/// Read the target from `var`, refusing an unset or unapproved live one.
///
/// Why: the Architect forbids reading the live index without approval, and a
/// default target would make a bare `--ignored` run do exactly that.
/// What: socket, index and scope file must all be set and non-blank, and the
/// socket must be absolute. The index `main`, or a socket `is_live_socket`
/// accepts, also needs a non-blank approval reference. `calls` defaults to
/// 40 and must be at least 20.
/// Test: `target_guard_refuses_unset_and_unapproved_live_targets`.
fn resolve_target(
    var: impl Fn(&str) -> Option<String>,
    is_live_socket: impl Fn(&Path) -> bool,
) -> Result<LoadTarget, String> {
    let get = |name: &str| {
        var(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let (Some(socket), Some(index_id), Some(scope_file)) =
        (get(ENV_SOCKET), get(ENV_INDEX), get(ENV_SCOPE_FILE))
    else {
        let missing: Vec<&str> = [ENV_SOCKET, ENV_INDEX, ENV_SCOPE_FILE]
            .into_iter()
            .filter(|name| get(name).is_none())
            .collect();
        return Err(format!(
            "refusing to run: set {} explicitly; the load test has no default target",
            missing.join(", ")
        ));
    };
    let socket = PathBuf::from(socket);
    if !socket.is_absolute() {
        return Err(format!(
            "refusing to run: {ENV_SOCKET} must be an absolute socket path, got {}",
            socket.display()
        ));
    }
    let calls = parse_calls(get(ENV_CALLS).as_deref())?;
    let live_approval = get(ENV_LIVE_APPROVAL);
    let live = index_id == LIVE_INDEX || is_live_socket(&socket);
    if live && live_approval.is_none() {
        return Err(format!(
            "refusing to run: the target ({}, index {index_id:?}) is the live trusty-search \
             daemon or the live `{LIVE_INDEX}` index; it may be read only with the owner's or \
             the Architect's approval, named in {ENV_LIVE_APPROVAL}. Use a sandbox from \
             scripts/sandbox_search_daemon.sh instead",
            socket.display()
        ));
    }
    Ok(LoadTarget {
        socket,
        index_id,
        scope_file: PathBuf::from(scope_file),
        calls,
        live_approval,
    })
}

/// N from `TRUSTY_ANALYZE_LOAD_CALLS`: 40 when unset, at least 20.
fn parse_calls(raw: Option<&str>) -> Result<usize, String> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_CALLS);
    };
    let calls: usize = raw
        .parse()
        .map_err(|_| format!("{ENV_CALLS} must be a whole number, got {raw:?}"))?;
    if calls < MIN_CALLS {
        return Err(format!(
            "{ENV_CALLS}={calls} is below {MIN_CALLS}; p95 over fewer samples is the max"
        ));
    }
    Ok(calls)
}

/// True when `socket` is the live daemon's default socket path.
///
/// What: compares against `search_socket_under(None)`, the path the live
/// daemon binds with no `TRUSTY_DATA_DIR`, lexically and canonically. A path
/// that cannot be resolved counts as live, so the guard fails closed.
fn is_default_live_socket(socket: &Path) -> bool {
    let Ok(live) = search_rpc::search_socket_under(None) else {
        return true;
    };
    socket == live
        || matches!(
            (std::fs::canonicalize(socket), std::fs::canonicalize(&live)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The scope file's entries: exactly 20 distinct paths, `#` lines skipped.
///
/// Test: `target_guard_refuses_unset_and_unapproved_live_targets`.
fn parse_scope(text: &str) -> Result<Vec<String>, String> {
    let mut entries: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || entries.iter().any(|e| e == line) {
            continue;
        }
        entries.push(line.to_string());
    }
    if entries.len() != SCOPE_FILES {
        return Err(format!(
            "the scope file names {} distinct paths; the load test needs exactly {SCOPE_FILES}",
            entries.len()
        ));
    }
    Ok(entries)
}

// ─── statistics and assertions ───────────────────────────────────────────────

/// p50, p95 and max of one set of samples.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Summary {
    n: usize,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

/// Nearest-rank percentile: the `ceil(pct * n / 100)`-th smallest sample.
///
/// Test: `percentile_uses_nearest_rank_over_synthetic_timings`.
fn percentile(sorted: &[Duration], pct: usize) -> Option<Duration> {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

impl Summary {
    /// Summarise `samples`; `None` when there are none.
    fn of(samples: &[Duration]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        Some(Self {
            n: sorted.len(),
            p50: percentile(&sorted, 50)?,
            p95: percentile(&sorted, 95)?,
            max: *sorted.last()?,
        })
    }

    fn line(&self, label: &str) -> String {
        format!(
            "{label:<30} n={:<3} p50={:>8.3}s p95={:>8.3}s max={:>8.3}s",
            self.n,
            self.p50.as_secs_f64(),
            self.p95.as_secs_f64(),
            self.max.as_secs_f64()
        )
    }
}

/// The `search.chunks.list` traffic of one round, read off the proxy.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct RoundPages {
    scoped: u64,
    unscoped: u64,
    rows: u64,
}

/// Most scoped page fetches a round may make.
///
/// Why: a scoped walk must never page the corpus; brief §4 test 1 asks for a
/// page-fetch count bounded by the scope.
/// What: a walk over `r` rows needs at most `ceil(r / min_limit) + 1` pages,
/// since the limit never drops below `min_limit` and one trailing page may be
/// empty. Summed over `walks` walks this is at most
/// `ceil(rows / min_limit) + 2 * walks`, and each page may be tried
/// `attempts` times.
/// Test: `page_fetch_bound_counts_retries_and_refuses_an_unscoped_page`.
fn page_fetch_bound(walks: u64, rows: u64, policy: &PageRetryPolicy) -> u64 {
    let min_limit = policy.min_limit.max(1) as u64;
    u64::from(policy.attempts.max(1)) * (rows.div_ceil(min_limit) + 2 * walks)
}

/// The page-count failures of one round, if any.
///
/// Test: `page_fetch_bound_counts_retries_and_refuses_an_unscoped_page`.
fn page_violations(
    round: usize,
    pages: RoundPages,
    walks: u64,
    policy: &PageRetryPolicy,
) -> Vec<String> {
    let mut out = Vec::new();
    if pages.unscoped > 0 {
        out.push(format!(
            "round {round}: {} unscoped search.chunks.list pages; a scoped call paged the corpus",
            pages.unscoped
        ));
    }
    let bound = page_fetch_bound(walks, pages.rows, policy);
    if pages.scoped > bound {
        out.push(format!(
            "round {round}: {} scoped pages for {} rows over {walks} walks exceeds the bound {bound}",
            pages.scoped, pages.rows
        ));
    }
    out
}

/// True when chunk `file` lies under scope entry `entry` at a segment boundary.
///
/// What: `file` equals `entry` or starts with `entry/`; a relative entry also
/// matches an absolute `file` that ends with `/entry` or contains `/entry/`.
/// Test: `results_outside_the_scope_are_reported`.
fn in_entry(file: &str, entry: &str) -> bool {
    let entry = entry.trim_end_matches('/');
    if file == entry || file.starts_with(&format!("{entry}/")) {
        return true;
    }
    !entry.starts_with('/')
        && (file.ends_with(&format!("/{entry}")) || file.contains(&format!("/{entry}/")))
}

/// Every hotspot or smell `file` in `response` that lies in no scope entry.
///
/// Test: `results_outside_the_scope_are_reported`.
fn out_of_scope(response: &Value, scope: &[String]) -> Vec<String> {
    ["hotspots", "chunks"]
        .iter()
        .filter_map(|key| response[*key].as_array())
        .flatten()
        .filter_map(|item| item["file"].as_str())
        .filter(|file| !scope.iter().any(|entry| in_entry(file, entry)))
        .map(str::to_string)
        .collect()
}

/// Why a response does not prove it was scoped to `scope`, if it does not.
///
/// What: needs `scoped: true`, `scope.paths` naming exactly the entries, and
/// at least one entry outside `scope.unmatched`; a scope matching nothing
/// measures nothing.
/// Test: `results_outside_the_scope_are_reported`.
fn scope_echo_problem(method: &str, response: &Value, scope: &[String]) -> Option<String> {
    if response["scoped"] != json!(true) {
        return Some(format!("{method}: response is not marked scoped"));
    }
    let mut echoed: Vec<&str> = response["scope"]["paths"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut expected: Vec<&str> = scope.iter().map(String::as_str).collect();
    echoed.sort_unstable();
    expected.sort_unstable();
    if echoed != expected {
        return Some(format!("{method}: scope echo {echoed:?} != {expected:?}"));
    }
    let unmatched = response["scope"]["unmatched"]
        .as_array()
        .map_or(0, Vec::len);
    if unmatched >= scope.len() {
        return Some(format!(
            "{method}: no scope entry selected a chunk; pick files the index holds"
        ));
    }
    None
}

/// The run's failures: p95 at or over the target, plus every recorded one.
///
/// Test: `p95_verdict_absorbs_two_slow_rounds_in_forty_and_fails_on_three`.
fn verdict(rounds: &Summary, mut failures: Vec<String>) -> Vec<String> {
    if rounds.p95 >= P95_TARGET {
        failures.insert(
            0,
            format!(
                "p95 {:.3}s is not under the {}s target",
                rounds.p95.as_secs_f64(),
                P95_TARGET.as_secs()
            ),
        );
    }
    failures
}

// ─── counting proxy ──────────────────────────────────────────────────────────

/// Running totals of the `search.chunks.list` frames the proxy relayed.
#[derive(Default)]
struct PageCounts {
    scoped: AtomicU64,
    unscoped: AtomicU64,
    rows: AtomicU64,
}

/// A Unix socket that relays each frame to trusty-search and counts pages.
///
/// Why: the analyzer reports no page count, and trusty-search is the operator's
/// daemon; counting at the socket sees every page, retries included.
/// What: one connection carries one newline-framed request and one response,
/// as `search_rpc::call_at` sends them. Pages with a string `path_prefix` are
/// scoped; the rows are the length of `result.chunks`.
/// Test: `a_round_through_the_counting_proxy_counts_every_scoped_page`.
struct CountingProxy {
    _dir: tempfile::TempDir,
    path: PathBuf,
    counts: Arc<PageCounts>,
    task: tokio::task::JoinHandle<()>,
}

impl CountingProxy {
    fn start(upstream: PathBuf) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("proxy tempdir: {e}"))?;
        let path = dir.path().join("proxy.sock");
        let listener = trusty_common::uds::bind_hardened(&path)
            .map_err(|e| format!("bind the counting proxy: {e}"))?;
        let counts = Arc::new(PageCounts::default());
        let seen = Arc::clone(&counts);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    // A broken relay shows up as the client's own error.
                    let _ = relay(stream, &upstream, &seen).await;
                });
            }
        });
        Ok(Self {
            _dir: dir,
            path,
            counts,
            task,
        })
    }

    fn snapshot(&self) -> RoundPages {
        RoundPages {
            scoped: self.counts.scoped.load(Ordering::SeqCst),
            unscoped: self.counts.unscoped.load(Ordering::SeqCst),
            rows: self.counts.rows.load(Ordering::SeqCst),
        }
    }
}

impl Drop for CountingProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay(client: UnixStream, upstream: &Path, counts: &PageCounts) -> std::io::Result<()> {
    let (read, mut write) = client.into_split();
    let mut request = Vec::new();
    BufReader::new(read).read_until(b'\n', &mut request).await?;
    let page = serde_json::from_slice::<Value>(&request)
        .ok()
        .filter(|v| v["method"] == SEARCH_CHUNKS_LIST)
        .map(|v| v["params"]["path_prefix"].is_string());
    match page {
        Some(true) => counts.scoped.fetch_add(1, Ordering::SeqCst),
        Some(false) => counts.unscoped.fetch_add(1, Ordering::SeqCst),
        None => 0,
    };
    let mut up = UnixStream::connect(upstream).await?;
    up.write_all(&request).await?;
    let mut response = Vec::new();
    BufReader::new(&mut up)
        .read_until(b'\n', &mut response)
        .await?;
    if page.is_some() {
        let rows = serde_json::from_slice::<Value>(&response)
            .ok()
            .and_then(|v| v["result"]["chunks"].as_array().map(Vec::len))
            .unwrap_or(0);
        counts.rows.fetch_add(rows as u64, Ordering::SeqCst);
    }
    write.write_all(&response).await
}

// ─── one round ───────────────────────────────────────────────────────────────

/// One review round: per-method latency and response, and the round's time.
struct RoundOutcome {
    total: Duration,
    per_method: Vec<(Duration, Value)>,
}

fn params_for(method: &str, index_id: &str, scope: &[String]) -> Value {
    let mut params = json!({ "index_id": index_id, "paths": scope });
    match method {
        "analyze.complexity_hotspots" => params["top_n"] = json!(RESULT_CAP),
        "analyze.smells" => {
            params["limit"] = json!(RESULT_CAP);
            params["omit_content"] = json!(true);
        }
        _ => {}
    }
    params
}

async fn timed_call(
    router: &RpcRouter,
    method: &str,
    params: Value,
) -> Result<(Duration, Value), String> {
    let frame = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .map_err(|e| format!("encode {method}: {e}"))?;
    let start = Instant::now();
    let response = router.dispatch(&frame).await;
    let elapsed = start.elapsed();
    if let Some(error) = response.error {
        return Err(format!("{method} failed: {} {}", error.code, error.message));
    }
    let result = response
        .result
        .ok_or_else(|| format!("{method}: a success frame without a result"))?;
    Ok((elapsed, result))
}

/// Send the three methods at once and wait for all of them.
async fn run_round(
    router: &RpcRouter,
    index_id: &str,
    scope: &[String],
) -> Result<RoundOutcome, String> {
    let start = Instant::now();
    let calls = METHODS
        .iter()
        .map(|m| timed_call(router, m, params_for(m, index_id, scope)));
    let results = futures::future::join_all(calls).await;
    let total = start.elapsed();
    let per_method = results.into_iter().collect::<Result<Vec<_>, _>>()?;
    Ok(RoundOutcome { total, per_method })
}

/// The scope and page failures of one finished round.
fn round_failures(
    round: usize,
    outcome: &RoundOutcome,
    pages: RoundPages,
    scope: &[String],
    policy: &PageRetryPolicy,
) -> Vec<String> {
    let walks = (METHODS.len() * scope.len()) as u64;
    let mut out = page_violations(round, pages, walks, policy);
    for (method, (_, response)) in METHODS.iter().zip(&outcome.per_method) {
        out.extend(scope_echo_problem(method, response, scope));
        let stray = out_of_scope(response, scope);
        if !stray.is_empty() {
            out.push(format!(
                "round {round}: {method} returned {} out-of-scope results, e.g. {:?}",
                stray.len(),
                &stray[..stray.len().min(3)]
            ));
        }
    }
    out
}

fn analyzer_state(search_socket: &Path, dir: &Path) -> AnalyzerAppState {
    let facts = FactStore::open(&dir.join("facts.redb")).expect("open the fact store");
    let overlays =
        ScipOverlayStore::open(&dir.join("scip_overlays.redb")).expect("open the overlay store");
    AnalyzerAppState::new(TrustySearchClient::new(search_socket), facts, overlays)
}

// ─── the load run ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load test: needs an operator-named trusty-search daemon holding a 300K-chunk index"]
async fn scoped_review_p95_under_target() {
    let target = resolve_target(|name| std::env::var(name).ok(), is_default_live_socket)
        .unwrap_or_else(|e| panic!("{e}"));
    let text = std::fs::read_to_string(&target.scope_file)
        .unwrap_or_else(|e| panic!("read {ENV_SCOPE_FILE} {}: {e}", target.scope_file.display()));
    let scope = parse_scope(&text).unwrap_or_else(|e| panic!("{e}"));

    let fingerprint = TrustySearchClient::new(&target.socket)
        .index_fingerprint(&target.index_id)
        .await
        .unwrap_or_else(|e| panic!("search.index.status {}: {e:#}", target.index_id));
    let corpus = fingerprint.chunk_count.unwrap_or(0);
    assert!(
        corpus >= MIN_INDEX_CHUNKS,
        "index {:?} holds {corpus} chunks; the load test needs at least {MIN_INDEX_CHUNKS}",
        target.index_id
    );

    let proxy = CountingProxy::start(target.socket.clone()).unwrap_or_else(|e| panic!("{e}"));
    let store = tempfile::tempdir().expect("tempdir for the analyzer stores");
    let router = build_router(analyzer_state(&proxy.path, store.path()));
    let policy = PageRetryPolicy::default();

    println!(
        "=== scoped p95 load: index {:?} ({corpus} chunks) at {}, {} files, {} rounds{} ===",
        target.index_id,
        target.socket.display(),
        scope.len(),
        target.calls,
        target
            .live_approval
            .as_deref()
            .map_or(String::new(), |a| format!(", live approval {a:?}"))
    );

    let mut failures = Vec::new();
    let mut rounds = Vec::new();
    let mut per_method: Vec<Vec<Duration>> = vec![Vec::new(); METHODS.len()];
    let mut pages_total = RoundPages::default();
    for round in 0..WARMUP_ROUNDS + target.calls {
        let before = proxy.snapshot();
        let outcome =
            tokio::time::timeout(ROUND_TIMEOUT, run_round(&router, &target.index_id, &scope))
                .await
                .unwrap_or_else(|_| panic!("round {round} exceeded {ROUND_TIMEOUT:?}"))
                .unwrap_or_else(|e| panic!("round {round}: {e}"));
        let after = proxy.snapshot();
        let pages = RoundPages {
            scoped: after.scoped - before.scoped,
            unscoped: after.unscoped - before.unscoped,
            rows: after.rows - before.rows,
        };
        failures.extend(round_failures(round, &outcome, pages, &scope, &policy));
        if round < WARMUP_ROUNDS {
            continue;
        }
        rounds.push(outcome.total);
        for (slot, (elapsed, _)) in per_method.iter_mut().zip(&outcome.per_method) {
            slot.push(*elapsed);
        }
        pages_total.scoped += pages.scoped;
        pages_total.unscoped += pages.unscoped;
        pages_total.rows += pages.rows;
    }

    let summary = Summary::of(&rounds).expect("at least one measured round");
    println!("{}", summary.line("review round (3 methods)"));
    for (method, samples) in METHODS.iter().zip(&per_method) {
        if let Some(s) = Summary::of(samples) {
            println!("{}", s.line(method));
        }
    }
    println!(
        "pages over {} rounds: scoped {} (rows {}), unscoped {}",
        target.calls, pages_total.scoped, pages_total.rows, pages_total.unscoped
    );
    let failures = verdict(&summary, failures);
    assert!(
        failures.is_empty(),
        "scoped load test failed:\n{}",
        failures.join("\n")
    );
}

// ─── deterministic tests ─────────────────────────────────────────────────────

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// Why: the guard is what keeps a bare `--ignored` run off the live index.
/// What: unset, relative and unapproved live targets are refused; a sandbox
/// target and an approved live target pass; N and the scope file are checked.
/// Test: this function IS the test.
#[test]
fn target_guard_refuses_unset_and_unapproved_live_targets() {
    let live = Path::new("/home/u/.local/share/trusty-search/trusty-search.sock");
    let is_live = |p: &Path| p == live;
    let with = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    };

    let err = resolve_target(with(&[]), is_live).unwrap_err();
    for name in [ENV_SOCKET, ENV_INDEX, ENV_SCOPE_FILE] {
        assert!(err.contains(name), "{err}");
    }
    let err = resolve_target(
        with(&[
            (ENV_SOCKET, "/s/data/trusty-search.sock"),
            (ENV_INDEX, "big"),
            (ENV_SCOPE_FILE, " "),
        ]),
        is_live,
    )
    .unwrap_err();
    assert!(
        err.contains(ENV_SCOPE_FILE) && !err.contains(ENV_INDEX),
        "{err}"
    );
    let err = resolve_target(
        with(&[
            (ENV_SOCKET, "data/s.sock"),
            (ENV_INDEX, "big"),
            (ENV_SCOPE_FILE, "/s.txt"),
        ]),
        is_live,
    )
    .unwrap_err();
    assert!(err.contains("absolute"), "{err}");

    // The live index and the live socket each need an approval reference.
    for (socket, index) in [
        ("/s/data/trusty-search.sock", "main"),
        (
            "/home/u/.local/share/trusty-search/trusty-search.sock",
            "big",
        ),
    ] {
        let vars = move |name: &str| match name {
            ENV_SOCKET => Some(socket.to_string()),
            ENV_INDEX => Some(index.to_string()),
            ENV_SCOPE_FILE => Some("/s.txt".to_string()),
            _ => None,
        };
        let err = resolve_target(vars, is_live).unwrap_err();
        assert!(err.contains(ENV_LIVE_APPROVAL), "{err}");
        let approved = move |name: &str| match name {
            ENV_LIVE_APPROVAL => Some("architect ruling".to_string()),
            other => vars(other),
        };
        let target = resolve_target(approved, is_live).expect("an approved live target");
        assert_eq!(target.live_approval.as_deref(), Some("architect ruling"));
    }

    let sandbox = resolve_target(
        with(&[
            (ENV_SOCKET, "/s/data/trusty-search.sock"),
            (ENV_INDEX, "big"),
            (ENV_SCOPE_FILE, "/s.txt"),
        ]),
        is_live,
    )
    .expect("a sandbox target needs no approval");
    assert_eq!(
        sandbox,
        LoadTarget {
            socket: PathBuf::from("/s/data/trusty-search.sock"),
            index_id: "big".to_string(),
            scope_file: PathBuf::from("/s.txt"),
            calls: DEFAULT_CALLS,
            live_approval: None,
        }
    );
    assert_eq!(parse_calls(Some("60")), Ok(60));
    assert!(parse_calls(Some("19")).is_err());
    assert!(parse_calls(Some("forty")).is_err());

    let twenty: String = (0..20).map(|i| format!("src/f{i}.rs\n")).collect();
    assert_eq!(
        parse_scope(&format!("# scope\n\n{twenty}src/f0.rs\n")).map(|s| s.len()),
        Ok(20)
    );
    assert!(parse_scope("src/a.rs\n").is_err());
}

/// Test: this function IS the test.
#[test]
fn percentile_uses_nearest_rank_over_synthetic_timings() {
    let forty: Vec<Duration> = (1..=40).rev().map(secs).collect();
    let s = Summary::of(&forty).expect("forty samples");
    assert_eq!(
        (s.n, s.p50, s.p95, s.max),
        (40, secs(20), secs(38), secs(40))
    );
    let twenty: Vec<Duration> = (1..=20).map(secs).collect();
    assert_eq!(Summary::of(&twenty).map(|s| s.p95), Some(secs(19)));
    assert_eq!(
        Summary::of(&[secs(3)]).map(|s| (s.p50, s.p95)),
        Some((secs(3), secs(3)))
    );
    assert_eq!(Summary::of(&[]), None);
}

/// Why: the review notes one slow page can cost about 121 s under the retry
/// policy; N = 40 must absorb two such rounds and fail on a third.
/// Test: this function IS the test.
#[test]
fn p95_verdict_absorbs_two_slow_rounds_in_forty_and_fails_on_three() {
    let run = |slow: usize| {
        let mut samples = vec![secs(2); DEFAULT_CALLS - slow];
        samples.extend(vec![secs(121); slow]);
        verdict(&Summary::of(&samples).expect("samples"), Vec::new())
    };
    assert!(run(2).is_empty(), "{:?}", run(2));
    assert!(run(3)[0].contains("not under"), "{:?}", run(3));
    let at_target = Summary::of(&vec![P95_TARGET; DEFAULT_CALLS]).expect("samples");
    assert_eq!(
        verdict(&at_target, Vec::new()).len(),
        1,
        "p95 must be under, not at, 10 s"
    );
    let fast = Summary::of(&[secs(1)]).expect("one sample");
    assert_eq!(
        verdict(&fast, vec!["stray".into()]),
        vec!["stray".to_string()]
    );
}

/// Test: this function IS the test.
#[test]
fn page_fetch_bound_counts_retries_and_refuses_an_unscoped_page() {
    let policy = PageRetryPolicy {
        call_timeout: secs(30),
        attempts: 4,
        min_limit: 100,
        backoff: Duration::from_millis(250),
    };
    // 60 walks (3 methods x 20 files) over 600 rows: 4 * (6 + 120).
    assert_eq!(page_fetch_bound(60, 600, &policy), 504);
    let one_try = PageRetryPolicy {
        attempts: 0,
        ..policy.clone()
    };
    assert_eq!(page_fetch_bound(60, 600, &one_try), 126);

    let ok = RoundPages {
        scoped: 60,
        unscoped: 0,
        rows: 600,
    };
    assert!(page_violations(0, ok, 60, &policy).is_empty());
    let over = RoundPages { scoped: 505, ..ok };
    assert!(page_violations(1, over, 60, &policy)[0].contains("exceeds the bound 504"));
    let corpus = RoundPages { unscoped: 1, ..ok };
    assert!(page_violations(2, corpus, 60, &policy)[0].contains("unscoped"));
}

/// Test: this function IS the test.
#[test]
fn results_outside_the_scope_are_reported() {
    let scope = vec!["src/a.rs".to_string(), "crates/x/".to_string()];
    assert!(in_entry("src/a.rs", "src/a.rs"));
    assert!(in_entry("/repo/src/a.rs", "src/a.rs"));
    assert!(in_entry("crates/x/lib.rs", "crates/x/"));
    assert!(!in_entry("src/a.rs.bak", "src/a.rs"));
    assert!(!in_entry("/repo/other/src/a.rsx", "src/a.rs"));
    assert!(!in_entry("/elsewhere/a.rs", "/repo/src/a.rs"));

    let response = json!({
        "scoped": true,
        "scope": { "paths": ["crates/x/", "src/a.rs"], "unmatched": ["crates/x/"] },
        "hotspots": [{ "file": "/repo/src/a.rs" }, { "file": "src/b.rs" }],
        "chunks": [{ "file": "crates/xy/lib.rs" }],
    });
    assert_eq!(
        out_of_scope(&response, &scope),
        vec!["src/b.rs", "crates/xy/lib.rs"]
    );
    assert_eq!(scope_echo_problem("m", &response, &scope), None);

    let unscoped = json!({ "scoped": false });
    assert!(scope_echo_problem("m", &unscoped, &scope).is_some());
    let wrong_echo = json!({ "scoped": true, "scope": { "paths": ["src/a.rs"], "unmatched": [] } });
    assert!(scope_echo_problem("m", &wrong_echo, &scope).is_some());
    let nothing = json!({ "scoped": true, "scope": { "paths": ["src/a.rs", "crates/x/"],
        "unmatched": ["src/a.rs", "crates/x/"] } });
    assert!(scope_echo_problem("m", &nothing, &scope)
        .unwrap()
        .contains("no scope entry"));
}

/// Why: the load run cannot be executed here (no live run, T11), so its
/// plumbing (in-process router, proxy, page and row counts, scope checks) is
/// proven against a fake daemon instead.
/// What: a fake answers `search.index.status` and serves one chunk per
/// scoped prefix; one round must make 60 scoped pages, 60 rows, no unscoped
/// page and no failure. A fake that ignores the scope must be caught.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_round_through_the_counting_proxy_counts_every_scoped_page() {
    let search = fake_search::FakeSearchSocket::serve(|method, params| match method {
        "search.index.status" => Ok(json!({ "root_path": "/repo", "chunk_count": 400_000 })),
        SEARCH_CHUNKS_LIST => {
            let prefix = params["path_prefix"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            Ok(json!({
                "chunks": [{
                    "id": format!("{prefix}:1:3"), "file": prefix, "start_line": 1,
                    "end_line": 3, "content": "fn f(x: i32) -> i32 {\n    if x > 0 { x } else { -x }\n}",
                }],
                "total": 1, "next_cursor": null, "path_prefix": prefix,
            }))
        }
        other => Err((-32601, format!("fake has no {other}"))),
    });
    let scope: Vec<String> = (0..SCOPE_FILES).map(|i| format!("src/f{i}.rs")).collect();
    let proxy = CountingProxy::start(search.path().to_path_buf()).expect("proxy");
    let store = tempfile::tempdir().expect("tempdir");
    let router = build_router(analyzer_state(&proxy.path, store.path()));
    let policy = PageRetryPolicy::default();

    let outcome = run_round(&router, "big", &scope).await.expect("round");
    let pages = proxy.snapshot();
    assert_eq!(
        pages,
        RoundPages {
            scoped: 60,
            unscoped: 0,
            rows: 60
        }
    );
    let failures = round_failures(0, &outcome, pages, &scope, &policy);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(outcome.per_method.len(), METHODS.len());
    let hotspots = &outcome.per_method[0].1["hotspots"];
    assert_eq!(
        hotspots.as_array().map(Vec::len),
        Some(SCOPE_FILES),
        "{hotspots}"
    );

    // The same round with a wrong scope list must report the strays.
    let narrower = &scope[..SCOPE_FILES - 1];
    let failures = round_failures(1, &outcome, pages, narrower, &policy);
    assert!(
        failures.iter().any(|f| f.contains("out-of-scope")),
        "{failures:?}"
    );
}

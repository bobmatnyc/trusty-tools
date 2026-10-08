//! Tests for the changed-symbol phase (#9196): every fail-open arm.
//!
//! Why: AC3: an outage omits the section and the review continues, so each
//! downgrade arm needs a test that goes red when the arm is removed (B6 Q8).
//! What: a scripted `SearchClient` answers `call_chain` per entry point and
//! records every call; the phase runs over a real `FilteredDiff`.
//! Test: included as `#[cfg(test)] mod tests` from `symbols_apply.rs`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::{
    integrations::search_client::{
        HealthResponse, IndexInfo, IndexStageReport, IndexStatusResponse, SearchResult,
    },
    models::ContextSourceRecord,
    pipeline::{
        citation_gate::DocCorpus,
        diff_analyzer::DiffAnalyzer,
        optional_context::{
            OptionalContextRequest, files_render::FileSections, ledger::ContextLedger,
            probes::ContextRows,
        },
    },
};

/// One scripted answer.
#[derive(Clone)]
enum Answer {
    /// A report.
    Text(String),
    /// `Api { status, body }`.
    Api(u16, String),
    /// `Transport`.
    Down,
    /// `Unavailable`: what the trait's default body answers.
    Unsupported,
    /// Never answers.
    Hang,
    /// Answers `Text` after this long.
    Slow(Duration, String),
}

/// A `SearchClient` whose `call_chain` answers per entry point.
struct Graph {
    answers: HashMap<String, Answer>,
    fallback: Answer,
    graph_stage: Result<String, u16>,
    status_hangs: bool,
    calls: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl Graph {
    fn new(fallback: Answer) -> Self {
        Self {
            answers: HashMap::new(),
            fallback,
            graph_stage: Ok("ready".to_string()),
            status_hangs: false,
            calls: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    fn with(mut self, entry: &str, answer: Answer) -> Self {
        self.answers.insert(entry.to_string(), answer);
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait]
impl SearchClient for Graph {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Err(SearchClientError::Unavailable("unused".into()))
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(Vec::new())
    }

    async fn index_status(&self, id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        if self.status_hangs {
            std::future::pending::<()>().await;
        }
        match &self.graph_stage {
            Ok(stage) => {
                let mut status = IndexStatusResponse::ready(id);
                status.stages.graph = IndexStageReport {
                    status: stage.clone(),
                    failure: None,
                };
                Ok(status)
            }
            Err(code) => Err(SearchClientError::Api {
                status: *code,
                body: "unknown index: idx".into(),
            }),
        }
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(Vec::new())
    }

    async fn call_chain(
        &self,
        index_id: &str,
        entry_point: &str,
        direction: &str,
        max_depth: u32,
        include_source: bool,
    ) -> Result<String, SearchClientError> {
        assert_eq!(
            (index_id, direction, max_depth, include_source),
            ("idx", "both", 1, false)
        );
        self.calls
            .lock()
            .expect("lock")
            .push(entry_point.to_string());
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        let answer = self
            .answers
            .get(entry_point)
            .cloned()
            .unwrap_or_else(|| self.fallback.clone());
        let out = match answer {
            Answer::Text(text) => Ok(text),
            Answer::Api(status, body) => Err(SearchClientError::Api { status, body }),
            Answer::Down => Err(SearchClientError::Transport("connect refused".into())),
            Answer::Unsupported => Err(SearchClientError::Unavailable("unsupported".into())),
            Answer::Hang => std::future::pending().await,
            Answer::Slow(wait, text) => {
                tokio::time::sleep(wait).await;
                Ok(text)
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

/// A `direction=both` report anchored at `path`, with `callers`.
fn report(path: &str, symbol: &str, callers: &[(&str, &str)]) -> String {
    let mut out = format!(
        "# Call chain: {symbol}\n\n═══\n\n## `{symbol}` [ENTRY]  {path}:1\n\
         Signature: pub fn {symbol}()\nWhy: (no doc)\nWhat: (no doc)\n\nCalls →\n  (none discovered)\n\
         Called by ←\n"
    );
    if callers.is_empty() {
        out.push_str("  (none discovered)\n");
    }
    for (name, location) in callers {
        out.push_str(&format!("  · {name}  {location}\n"));
    }
    out + "\n───\n"
}

/// A diff adding `fns` to `path`, one hunk each.
fn adds(path: &str, fns: &[&str]) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    for (i, name) in fns.iter().enumerate() {
        out.push_str(&format!(
            "@@ -{n},1 +{n},2 @@\n x\n+pub fn {name}() {{}}\n",
            n = i * 10 + 1
        ));
    }
    out
}

fn blank() -> AppliedContext {
    AppliedContext {
        body_in_refs: true,
        sections: String::new(),
        doc_sections: String::new(),
        docs: DocCorpus::default(),
        files: FileSections::default(),
        symbols: FileSections::default(),
    }
}

/// What one phase run left.
struct Ran {
    applied: AppliedContext,
    rows: Vec<ContextSourceRecord>,
    graph: Arc<Graph>,
}

impl Ran {
    fn row(&self) -> &ContextSourceRecord {
        self.rows
            .iter()
            .find(|r| r.source == "symbol_context")
            .unwrap_or_else(|| panic!("no symbol_context row: {:?}", self.rows))
    }

    fn item(&self, id: &str) -> (SourceState, String) {
        let item = self
            .row()
            .items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no {id} item in {:?}", self.row()));
        (item.state, item.detail.clone().unwrap_or_default())
    }

    fn prompt(&self) -> String {
        self.applied.prompt_sections()
    }
}

/// Knobs a run varies.
struct Opts {
    on: bool,
    index: &'static str,
    gate: Option<String>,
    path: ReviewPath,
    mr: MapReduceConfig,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            on: true,
            index: "idx",
            gate: None,
            path: ReviewPath::Unified,
            mr: MapReduceConfig::default(),
        }
    }
}

async fn run_with(diff: &str, graph: Graph, opts: Opts) -> Ran {
    let graph = Arc::new(graph);
    let filtered = DiffAnalyzer::default().analyze(diff).await;
    let (symbols, left, unresolved) = if opts.on {
        candidates(&filtered, diff, (opts.path, &opts.mr))
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    let search: Arc<dyn SearchClient> = graph.clone();
    let call = SymbolsCall {
        on: opts.on,
        search,
        index: opts.index,
        gate: opts.gate,
        symbols,
        left,
        unresolved,
    };
    let mut ledger = ContextLedger::new(true);
    let mut applied = blank();
    apply_symbols(&mut applied, call, &mut ledger).await;
    Ran {
        applied,
        rows: ledger.into_records(),
        graph,
    }
}

async fn run(diff: &str, graph: Graph) -> Ran {
    run_with(diff, graph, Opts::default()).await
}

const A: &str = "src/a.rs";

// ── Arms 1-4: nothing is read ────────────────────────────────────────────────

/// Arm 1: flag off: no call, no row, no section.
#[tokio::test]
async fn symbol_context_off_makes_no_call_chain_call() {
    let opts = Opts {
        on: false,
        ..Opts::default()
    };
    let ran = run_with(&adds(A, &["total"]), Graph::new(Answer::Down), opts).await;
    assert!(ran.graph.calls().is_empty());
    assert!(ran.rows.is_empty());
    assert!(ran.prompt().is_empty());
}

/// Arm 2: no index: `unavailable`, no call.
#[tokio::test]
async fn no_index_is_unavailable_and_makes_no_call() {
    let opts = Opts {
        index: "",
        ..Opts::default()
    };
    let ran = run_with(&adds(A, &["total"]), Graph::new(Answer::Down), opts).await;
    assert!(ran.graph.calls().is_empty());
    assert_eq!(ran.row().state, SourceState::Unavailable);
    assert!(ran.prompt().is_empty());
}

/// Arm 3: a gate that degraded trusty-search: `unavailable` with its reason,
/// no call.
#[tokio::test]
async fn a_degraded_search_gate_skips_the_calls_and_names_the_reason() {
    let opts = Opts {
        gate: Some("index idx: graph failed".into()),
        ..Opts::default()
    };
    let ran = run_with(&adds(A, &["total"]), Graph::new(Answer::Down), opts).await;
    assert!(ran.graph.calls().is_empty());
    assert_eq!(ran.row().state, SourceState::Unavailable);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("graph failed"), "{detail}");
}

/// Architect risk 2: a symbol graph that is not ready is `unavailable`, and
/// no call is made, so an empty graph never renders "none found".
#[tokio::test]
async fn a_graph_stage_not_ready_is_unavailable_and_makes_no_call() {
    let mut graph = Graph::new(Answer::Text(report(A, "total", &[])));
    graph.graph_stage = Ok("in_progress".to_string());
    let ran = run(&adds(A, &["total"]), graph).await;
    assert!(ran.graph.calls().is_empty());
    assert_eq!(ran.row().state, SourceState::Unavailable);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("not ready"), "{detail}");
    assert!(ran.prompt().is_empty());
}

/// Arm 7 at the probe: `404 unknown index` is `unavailable`, never a skip.
#[tokio::test]
async fn an_index_status_failure_is_unavailable_not_a_skip() {
    let mut graph = Graph::new(Answer::Text(report(A, "total", &[])));
    graph.graph_stage = Err(404);
    let ran = run(&adds(A, &["total"]), graph).await;
    assert!(ran.graph.calls().is_empty());
    assert_eq!(ran.row().state, SourceState::Unavailable);
}

/// Architect risk 2: a readiness probe that hangs is `unavailable` after the
/// call timeout, and no call is made.
#[tokio::test(start_paused = true)]
async fn a_hung_readiness_probe_is_unavailable_and_makes_no_call() {
    let mut graph = Graph::new(Answer::Text(report(A, "total", &[])));
    graph.status_hangs = true;
    let ran = run(&adds(A, &["total"]), graph).await;
    assert!(ran.graph.calls().is_empty());
    assert_eq!(ran.row().state, SourceState::Unavailable);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("timed out after 10 s"), "{detail}");
}

/// A daemon whose status reports no graph stage is read; the entry-file
/// check still guards its answer.
#[tokio::test]
async fn a_status_without_a_graph_stage_is_read() {
    let mut graph = Graph::new(Answer::Text(report(A, "total", &[])));
    graph.graph_stage = Ok(String::new());
    let ran = run(&adds(A, &["total"]), graph).await;
    assert_eq!(ran.graph.calls(), ["src/a.rs::total"]);
    assert_eq!(ran.row().state, SourceState::Used);
}

/// Arm 4: a client with the trait's default `call_chain` is `unavailable`
/// for the whole phase, after one batch at most.
#[tokio::test]
async fn a_client_without_call_chain_is_unavailable() {
    let names: Vec<String> = (0..8).map(|i| format!("f{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let ran = run(&adds(A, &refs), Graph::new(Answer::Unsupported)).await;
    assert_eq!(ran.graph.calls().len(), SYMBOL_CALL_CONCURRENCY);
    assert_eq!(ran.row().state, SourceState::Unavailable);
    assert!(ran.prompt().is_empty(), "AC3: an outage omits the section");
}

// ── Arms 5-11: a read fails ──────────────────────────────────────────────────

/// Arm 5: a 500 on one symbol leaves that symbol `unavailable`; the rest
/// are read and shown.
#[tokio::test]
async fn one_failed_symbol_does_not_abort_the_rest() {
    let graph = Graph::new(Answer::Text(String::new()))
        .with("src/a.rs::bad", Answer::Api(500, "boom".into()))
        .with("src/a.rs::good", Answer::Text(report(A, "good", &[])))
        .with("src/a.rs::fine", Answer::Text(report(A, "fine", &[])));
    let ran = run(&adds(A, &["bad", "good", "fine"]), graph).await;
    assert_eq!(ran.item("src/a.rs::bad").0, SourceState::Unavailable);
    assert_eq!(ran.item("src/a.rs::good").0, SourceState::Used);
    assert_eq!(ran.item("src/a.rs::fine").0, SourceState::Used);
    assert!(ran.prompt().contains("### src/a.rs::good"));
}

/// Arm 6: a dead daemon costs one batch of calls; the rest are skipped.
#[tokio::test]
async fn a_dead_daemon_costs_at_most_one_batch_of_calls() {
    let names: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let ran = run(&adds(A, &refs), Graph::new(Answer::Down)).await;
    assert_eq!(ran.graph.calls().len(), SYMBOL_CALL_CONCURRENCY);
    assert_eq!(ran.row().state, SourceState::Unavailable);
    let (_, detail) = ran.item("src/a.rs::f9");
    assert!(
        detail.contains("skipped after a trusty-search failure"),
        "{detail}"
    );
    assert!(ran.graph.peak.load(Ordering::SeqCst) <= SYMBOL_CALL_CONCURRENCY);
}

/// Arm 7: `404 unknown index` at read time is `unavailable` and stops the
/// phase; it never becomes the gather step's skip.
#[tokio::test]
async fn unknown_index_here_is_unavailable_not_a_skip() {
    let body = r#"{"error":"unknown index: idx"}"#.to_string();
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Api(404, body))).await;
    let (state, detail) = ran.item("src/a.rs::total");
    assert_eq!(state, SourceState::Unavailable);
    assert!(detail.contains("index not found"), "{detail}");
    assert_eq!(ran.row().state, SourceState::Unavailable);
}

/// Arm 8: `404 entry point not found` is a symbol the index does not hold,
/// typically one the PR adds: `absent`, not `unavailable`.
#[tokio::test]
async fn a_new_symbol_is_absent_not_unavailable() {
    let body = r#"{"error":"entry point not found: src/a.rs::total"}"#.to_string();
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Api(404, body))).await;
    assert_eq!(ran.item("src/a.rs::total").0, SourceState::Absent);
    assert_eq!(ran.row().state, SourceState::Absent);
    assert!(
        ran.prompt()
            .contains("\"src/a.rs::total\": not in the index")
    );
}

/// Arm 9: a KG-disabled index (`503 kg_unavailable`) is `unavailable` for
/// the phase.
#[tokio::test]
async fn a_kg_disabled_index_is_unavailable() {
    let body = r#"{"error":"kg_unavailable","reason":"skipped_by_config"}"#.to_string();
    let names = ["f0", "f1", "f2", "f3", "f4", "f5"];
    let ran = run(&adds(A, &names), Graph::new(Answer::Api(503, body))).await;
    assert_eq!(ran.row().state, SourceState::Unavailable);
    assert_eq!(ran.graph.calls().len(), SYMBOL_CALL_CONCURRENCY);
    let (_, detail) = ran.item("src/a.rs::f0");
    assert!(
        detail.contains("not ready") && detail.contains("kg_unavailable"),
        "{detail}"
    );
}

/// Condition 2 (Architect risk 2): a daemon "not ready" reply (a cold index
/// or a load in flight, the retryable 503) is the AC3 outage arm:
/// `unavailable`, never `used` or `absent`, and the phase stops.
#[tokio::test]
async fn a_not_ready_daemon_is_unavailable() {
    let body = r#"{"error":"index_not_resident","retryable":true}"#.to_string();
    let names = ["f0", "f1", "f2", "f3", "f4", "f5"];
    let ran = run(&adds(A, &names), Graph::new(Answer::Api(503, body))).await;
    assert_eq!(ran.row().state, SourceState::Unavailable);
    for name in names {
        let (state, _) = ran.item(&format!("src/a.rs::{name}"));
        assert_eq!(state, SourceState::Unavailable, "{name}");
    }
    assert_eq!(ran.graph.calls().len(), SYMBOL_CALL_CONCURRENCY);
    assert!(ran.prompt().is_empty(), "AC3: an outage omits the section");
}

/// Arm 10: a hung call times out at 10 s and is named; the others are kept.
#[tokio::test(start_paused = true)]
async fn a_hung_call_times_out_and_is_named() {
    let graph = Graph::new(Answer::Text(report(A, "ok", &[])))
        .with("src/a.rs::stuck", Answer::Hang)
        .with("src/a.rs::ok", Answer::Text(report(A, "ok", &[])));
    let ran = run(&adds(A, &["stuck", "ok"]), graph).await;
    let (state, detail) = ran.item("src/a.rs::stuck");
    assert_eq!(state, SourceState::Unavailable);
    assert!(detail.contains("timed out after 10 s"), "{detail}");
    assert_eq!(ran.item("src/a.rs::ok").0, SourceState::Used);
}

/// Arm 11: a batch not started by the deadline is left out as `deadline`;
/// started reads keep their result.
#[tokio::test(start_paused = true)]
async fn the_phase_deadline_omits_unstarted_symbols() {
    let slow = Answer::Slow(Duration::from_secs(9), report(A, "f", &[]));
    let graph = Graph::new(slow);
    let symbols: Vec<ChangedSymbol> = (0..12)
        .map(|i| ChangedSymbol {
            path: A.to_string(),
            name: format!("f{i}"),
            kind: super::super::symbols::Kind::Declared,
            diff_lines: 1,
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (reads, failure) = read_all(&graph, "idx", symbols, deadline).await;
    assert!(failure.is_none());
    assert_eq!(graph.calls().len(), 2 * SYMBOL_CALL_CONCURRENCY);
    let late: Vec<&Read> = reads.iter().skip(8).map(|(_, r)| r).collect();
    assert!(
        late.iter()
            .all(|r| matches!(r, Err(l) if l.reason == Reason::Deadline)),
        "the third batch never started"
    );
}

// ── Arms 12-19: what a report or the diff says ───────────────────────────────

/// Arm 12: an unparseable report is `unavailable` and renders nothing from
/// itself.
#[tokio::test]
async fn an_unparseable_report_is_unavailable_and_renders_nothing() {
    let garbage = "<html>502 Bad Gateway PAYLOAD_9196</html>".to_string();
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Text(garbage))).await;
    let (state, detail) = ran.item("src/a.rs::total");
    assert_eq!(state, SourceState::Unavailable);
    assert!(detail.contains("unparseable"), "{detail}");
    assert!(!ran.prompt().contains("PAYLOAD_9196"));
}

/// Arm 13: an oversize report is bounded and its item `truncated`.
#[tokio::test]
async fn an_oversize_report_is_bounded_and_marked_truncated() {
    let callers: Vec<(String, String)> = (0..20_000)
        .map(|i| (format!("caller_{i}"), format!("src/c.rs:{i}")))
        .collect();
    let refs: Vec<(&str, &str)> = callers
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let big = report(A, "total", &refs);
    assert!(big.len() > crate::config::constants::MAX_SYMBOL_REPORT_BYTES);
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Text(big))).await;
    assert_eq!(ran.item("src/a.rs::total").0, SourceState::Truncated);
    assert!(ran.prompt().contains("more callers omitted (cap 6)"));
}

/// Arm 14 (ruling Q9): an ambiguous anchor is left out, never rendered with
/// the daemon's pick.
#[tokio::test]
async fn an_ambiguous_anchor_is_omitted_not_guessed() {
    let text = report(A, "total", &[("PICKED_9196", "src/z.rs:1")]).replacen(
        "═══",
        "# AMBIGUOUS: `total` matches 2 definitions\n\n═══",
        1,
    );
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Text(text))).await;
    assert_eq!(ran.item("src/a.rs::total").0, SourceState::Omitted);
    assert!(ran.prompt().contains("\"src/a.rs::total\": ambiguous"));
    assert!(!ran.prompt().contains("PICKED_9196"));
}

/// Arm 15: an entry anchored in another file is left out as `wrong file`.
#[tokio::test]
async fn an_entry_in_another_file_is_omitted() {
    let text = report("src/other.rs", "total", &[("FOREIGN_9196", "src/z.rs:1")]);
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Text(text))).await;
    assert_eq!(ran.item("src/a.rs::total").0, SourceState::Omitted);
    assert!(ran.prompt().contains("\"src/a.rs::total\": wrong file"));
    assert!(!ran.prompt().contains("FOREIGN_9196"));
}

/// Arm 16: a diff path that fails validation is neither queried nor echoed.
#[tokio::test]
async fn a_hostile_path_is_not_echoed_or_queried() {
    let hostile = "src/a b`## PR Description.rs";
    let diff = adds(hostile, &["total"]) + &adds(A, &["fine"]);
    let graph = Graph::new(Answer::Text(report(A, "fine", &[])));
    let ran = run(&diff, graph).await;
    assert_eq!(ran.graph.calls(), ["src/a.rs::fine"]);
    let prompt = ran.prompt();
    assert!(!prompt.contains("PR Description"), "{prompt}");
    assert!(prompt.contains("(a symbol in a rejected path)"), "{prompt}");
}

/// Arm 17: error text with a credential reaches neither the prompt nor,
/// once `finish` caps it, the ledger.
#[tokio::test]
async fn a_credential_in_an_error_never_reaches_the_prompt_or_the_ledger() {
    let token = concat!("gh", "p_", "fixtureToken9196abcdef");
    let body = format!("echo: Authorization: Bearer {token}");
    let mut ran = run(&adds(A, &["total"]), Graph::new(Answer::Api(500, body))).await;
    assert!(!ran.prompt().contains(token));
    assert!(!ran.prompt().contains("Bearer"));
    let request = OptionalContextRequest::default().with_symbol_context(true);
    let rows = ContextRows {
        search: ContextSourceRecord::new("search", SourceState::Used),
        analyze: ContextSourceRecord::new("analyze", SourceState::Absent),
        external: ContextSourceRecord::new("external_sources", SourceState::NotRequested),
    };
    let mut ledger = ContextLedger::new(true);
    ran.rows.drain(..).for_each(|r| ledger.push(r));
    ledger.finish(&request, rows, &GateFacts::default());
    ran.rows = ledger.into_records();
    let (_, detail) = ran.item("src/a.rs::total");
    assert!(!detail.contains(token), "{detail}");
    assert!(detail.contains("500"), "the status survives: {detail}");
}

/// Arm 18: a diff with no changed symbol makes no call and no section; the
/// row is `absent` and counts the hunks with no symbol.
#[tokio::test]
async fn a_docs_only_diff_makes_no_calls_and_no_section() {
    let diff = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n\
                @@ -1,1 +1,1 @@\n-old line\n+new line\n";
    let ran = run(diff, Graph::new(Answer::Down)).await;
    assert!(ran.graph.calls().is_empty());
    assert!(ran.prompt().is_empty());
    assert_eq!(ran.row().state, SourceState::Absent);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("1 hunks had no symbol"), "{detail}");
}

/// Plan §4: test files and deleted files get no query.
#[tokio::test]
async fn test_files_and_deleted_files_get_no_query() {
    let deleted = "diff --git a/src/gone.rs b/src/gone.rs\ndeleted file mode 100644\n\
                   --- a/src/gone.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-pub fn gone() {}\n";
    let diff = adds("tests/it.rs", &["test_total"]) + deleted + &adds(A, &["total"]);
    let ran = run(&diff, Graph::new(Answer::Text(report(A, "total", &[])))).await;
    assert_eq!(ran.graph.calls(), ["src/a.rs::total"]);
}

/// Arm 19: on the map-reduce path a file no chunk prompt carries gets no
/// read, and its symbols are `not reviewed`, not `used`.
#[tokio::test]
async fn a_unit_without_a_prompt_gets_no_symbols_and_is_not_used() {
    let big = format!(
        "diff --git a/src/big.rs b/src/big.rs\n--- a/src/big.rs\n+++ b/src/big.rs\n\
         @@ -1,1 +1,3 @@\n x\n+pub fn huge() {{}}\n+{}\n",
        "// filler ".repeat(40)
    );
    let diff = big + &adds(A, &["total"]);
    let opts = Opts {
        path: ReviewPath::MapReduce,
        mr: MapReduceConfig {
            per_file_chars: 200,
            ..MapReduceConfig::default()
        },
        ..Opts::default()
    };
    let graph = Graph::new(Answer::Text(report(A, "total", &[])));
    let ran = run_with(&diff, graph, opts).await;
    assert_eq!(ran.graph.calls(), ["src/a.rs::total"]);
    assert_eq!(ran.item("src/big.rs::huge").0, SourceState::Omitted);
    let symbols = &ran.applied.symbols;
    assert!(symbols.for_unit(A, true).contains("### src/a.rs::total"));
    assert!(!symbols.for_unit(A, false).contains("### src/a.rs::total"));
    assert!(
        symbols
            .for_unit("src/big.rs", true)
            .contains("\"src/big.rs::huge\": not reviewed")
    );
}

/// AC2: past twelve symbols the rest are named `over symbol cap`, unread.
#[tokio::test]
async fn over_the_symbol_cap_is_named_not_read() {
    let names: Vec<String> = (0..14).map(|i| format!("f{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let ran = run(
        &adds(A, &refs),
        Graph::new(Answer::Text(report(A, "f", &[]))),
    )
    .await;
    assert_eq!(
        ran.graph.calls().len(),
        crate::config::constants::MAX_SYMBOL_CONTEXT_SYMBOLS
    );
    let over: Vec<&str> = ran
        .row()
        .items
        .iter()
        .filter(|i| i.state == SourceState::Omitted)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(over, ["src/a.rs::f12", "src/a.rs::f13"]);
    assert!(ran.prompt().contains("\"src/a.rs::f13\": over symbol cap"));
}

/// AC1: a shown block lists the report's callers and tests, and the item and
/// row are `used`.
#[tokio::test]
async fn a_shown_symbol_lists_callers_and_tests() {
    let text = report(
        A,
        "total",
        &[
            ("checkout", "src/cart.rs:7"),
            ("test_total", "tests/billing.rs:3"),
        ],
    );
    let ran = run(&adds(A, &["total"]), Graph::new(Answer::Text(text))).await;
    let prompt = ran.prompt();
    assert!(
        prompt.contains("callers:\n- checkout  src/cart.rs:7"),
        "{prompt}"
    );
    assert!(
        prompt.contains("tests:\n- test_total  tests/billing.rs:3"),
        "{prompt}"
    );
    assert_eq!(ran.row().state, SourceState::Used);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("may not be at the PR head"), "{detail}");
}

/// The flag alone turns the ledger on (`requested_new` counts it).
#[test]
fn symbol_context_flag_turns_the_ledger_on() {
    let request = OptionalContextRequest::default().with_symbol_context(true);
    assert!(request.requested_new() && request.ledger_enabled());
    assert!(!OptionalContextRequest::default().requested_new());
}

/// AC3: when a read fails because the daemon is down, the whole section is
/// left out, a symbol already read in the same batch included, and no item
/// reads `used`.
#[tokio::test]
async fn a_search_outage_omits_the_section_and_the_review_completes() {
    let graph = Graph::new(Answer::Down).with("src/a.rs::ok", Answer::Text(report(A, "ok", &[])));
    let ran = run(&adds(A, &["ok", "down"]), graph).await;
    assert!(ran.prompt().is_empty(), "{}", ran.prompt());
    assert_eq!(ran.row().state, SourceState::Unavailable);
    assert_eq!(ran.item("src/a.rs::ok").0, SourceState::Unavailable);
    let detail = ran.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("section left out"), "{detail}");
}

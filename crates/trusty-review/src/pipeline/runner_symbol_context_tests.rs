//! `symbol_context` through the real pipeline (#9196, B5).
//!
//! Why: the symbol section must reach the unified prompt and each map-reduce
//! chunk prompt as the ledger says, come from the changed symbols rather than
//! the top-term query (AC1's shortcut to fail), never become citable or reach
//! the verifier, and leave the review running when trusty-search fails (AC3).
//! What: the fixture PR of `runner_optional_context_off_tests.rs`, the
//! quoting reviewer of `runner_changed_files_tests.rs`, and a search client
//! whose `call_chain` answers per entry point and records every query.
//! Test: this module.

use std::sync::Mutex;

use super::changed_files::QuotingReviewer;
use super::optional_context_off::{
    FakePrSource, RecordingLlm, billing_diff, github_input, hermetic_config, mapreduce_diff,
};
use super::*;
use crate::integrations::search_client::IndexStatusResponse;
use crate::integrations::search_transport::fixture::EnvGuard;
use crate::models::{ContextSourceRecord, ReviewStatus, SourceState};
use crate::pipeline::optional_context::OptionalContextRequest;
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

/// The billing symbol the fixture diff declares.
const TOTAL: &str = "src/billing.rs::total";
/// A caller only the call graph knows.
// No digit: credential-shaped runs (letters and digits, 20+) are masked.
const CALLER: &str = "checkout_canary_bfive";
/// The entry's signature line, as the report renders it.
const SIGNATURE: &str = "fn total(amounts: &[u64], input: u64) -> u64 { // SIG_CANARY_9196";

/// A `direction=both` report for `symbol` in `path` with one caller.
fn report(path: &str, symbol: &str, caller: &str) -> String {
    format!(
        "# Call chain: {symbol}\n\n═══\n\n## `{symbol}` [ENTRY]  {path}:1\n\
         Signature: {SIGNATURE}\nWhy: (no doc)\nWhat: (no doc)\n\nCalls →\n  · step_2  {path}:20\n\
         Called by ←\n  · {caller}  src/cart.rs:7\n  · test_total  tests/billing.rs:3\n\n───\n"
    )
}

/// How `call_chain` answers.
#[derive(Clone, Copy)]
enum Mode {
    /// A report anchored at the queried file, with a per-file caller canary.
    Answer,
    /// `Transport`: the daemon is gone.
    Down,
    /// `503 index_not_resident`: the daemon is not ready.
    NotReady,
}

/// `FakeSearch` for every method but `call_chain`, recording queries.
struct GraphSearch {
    mode: Mode,
    entries: Mutex<Vec<String>>,
    queries: Mutex<Vec<String>>,
}

impl GraphSearch {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            entries: Mutex::new(Vec::new()),
            queries: Mutex::new(Vec::new()),
        })
    }

    fn entries(&self) -> Vec<String> {
        let mut seen = self.entries.lock().expect("lock").clone();
        seen.sort();
        seen
    }

    fn queries(&self) -> Vec<String> {
        self.queries.lock().expect("lock").clone()
    }
}

#[async_trait]
impl SearchClient for GraphSearch {
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        FakeSearch.index_status(index_id).await
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        FakeSearch.health().await
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        FakeSearch.list_indexes().await
    }

    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        self.queries.lock().expect("lock").push(query.to_string());
        FakeSearch.search(index_id, query, top_k).await
    }

    async fn call_chain(
        &self,
        _index_id: &str,
        entry_point: &str,
        _direction: &str,
        _max_depth: u32,
        _include_source: bool,
    ) -> Result<String, SearchClientError> {
        self.entries
            .lock()
            .expect("lock")
            .push(entry_point.to_string());
        let (path, symbol) = entry_point.rsplit_once("::").unwrap_or(("", entry_point));
        match self.mode {
            Mode::Answer => {
                let canary = if path == "src/billing.rs" {
                    CALLER.to_string()
                } else {
                    format!("caller_{symbol}_9196")
                };
                Ok(report(path, symbol, &canary))
            }
            Mode::Down => Err(SearchClientError::Transport("connect refused".into())),
            Mode::NotReady => Err(SearchClientError::Api {
                status: 503,
                body: r#"{"error":"index_not_resident","retryable":true}"#.into(),
            }),
        }
    }
}

/// What one fixture run observed.
struct Seen {
    outcome: ReviewOutcome,
    reviewer: Arc<RecordingLlm>,
    verifier: Arc<RecordingLlm>,
    search: Arc<GraphSearch>,
}

impl Seen {
    fn reviewer_users(&self) -> Vec<String> {
        self.reviewer
            .requests()
            .into_iter()
            .map(|(_, u)| u)
            .collect()
    }

    fn row(&self) -> &ContextSourceRecord {
        self.outcome
            .context_sources
            .iter()
            .find(|r| r.source == "symbol_context")
            .unwrap_or_else(|| panic!("no symbol_context row: {:?}", self.outcome.context_sources))
    }
}

/// One fixture run's inputs.
struct Setup {
    request: OptionalContextRequest,
    diff: String,
    mode: Mode,
    reviewer: QuotingReviewer,
    analyze_down: bool,
}

impl Setup {
    fn new(request: OptionalContextRequest) -> Self {
        Self {
            request,
            diff: billing_diff(),
            mode: Mode::Answer,
            reviewer: QuotingReviewer {
                quote: "let total = amounts.iter().sum::<u64>();",
                line: 10,
            },
            analyze_down: false,
        }
    }
}

fn on() -> OptionalContextRequest {
    OptionalContextRequest::default().with_symbol_context(true)
}

async fn run(setup: Setup) -> Seen {
    let missing = std::env::temp_dir().join(format!(
        "trusty-review-9196-no-search-{}.sock",
        std::process::id()
    ));
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &missing.to_string_lossy());
    let source = FakePrSource::new("Adds an invoice total.", &setup.diff);
    let reviewer = RecordingLlm::new(Arc::new(setup.reviewer));
    let verifier = RecordingLlm::new(Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    }));
    let search = GraphSearch::new(setup.mode);
    let mut deps = ready_deps(reviewer.clone(), Some(verifier.clone()));
    deps.search = search.clone() as Arc<dyn SearchClient>;
    let mut config = hermetic_config();
    if setup.analyze_down {
        // Arm 21: analyze opted out and absent; the run is degraded.
        deps.analyze = None;
        config.context.require_analyze = false;
    }
    let mut options = ReviewOptions::new(setup.request);
    options.pr_source = Some(Arc::new(source));
    let input = github_input(CallerContext::default());
    let outcome = run_review_with(&config, input, deps, options).await;
    Seen {
        outcome,
        reviewer,
        verifier,
        search,
    }
}

/// AC1: the section reaches the unified prompt with the graph's callers and
/// tests, the ledger records it `used`, and the verifier never sees it.
#[serial_test::serial]
#[tokio::test]
async fn symbol_section_reaches_the_unified_prompt_and_the_ledger_as_used() {
    let seen = run(Setup::new(on())).await;
    let users = seen.reviewer_users();
    let prompt = users
        .iter()
        .find(|u| u.contains(CALLER))
        .expect("a prompt with the section");
    assert!(prompt.contains("## Changed symbols: callers, callees and tests"));
    assert!(prompt.contains(&format!("### {TOTAL}")));
    assert!(
        prompt.contains("tests:\n- test_total  tests/billing.rs:3"),
        "{prompt}"
    );
    assert_eq!(seen.row().state, SourceState::Used);
    assert!(
        !seen.verifier.requests().is_empty(),
        "the finding was verified"
    );
}

/// Plan §2: the symbol text never reaches the verifier's prompt.
#[serial_test::serial]
#[tokio::test]
async fn symbol_context_never_reaches_the_verifier_prompt() {
    let seen = run(Setup::new(on())).await;
    let verifier = seen.verifier.requests();
    assert!(!verifier.is_empty(), "the verifier ran");
    for (system, user) in verifier {
        assert!(!user.contains(CALLER) && !system.contains(CALLER));
        assert!(!user.contains("SIG_CANARY_9196"));
    }
}

/// AC1's shortcut to fail: the call-chain entry points are the symbols the
/// diff changes, and the code-context `search` query is unchanged.
#[serial_test::serial]
#[tokio::test]
async fn symbol_query_is_the_changed_symbol_not_the_top_five_terms() {
    let off = run(Setup::new(OptionalContextRequest::default())).await;
    let seen = run(Setup::new(on())).await;
    assert_eq!(seen.search.entries(), [TOTAL]);
    assert_eq!(seen.search.queries(), off.search.queries());
    assert_eq!(seen.search.queries().len(), 1);
}

/// Arm 1 at the runner: off makes no call-chain call and no section, and the
/// reviewer prompts equal a run whose client has no call graph at all.
#[serial_test::serial]
#[tokio::test]
async fn symbol_context_off_makes_no_call_and_leaves_prompts_unchanged() {
    let off = run(Setup::new(OptionalContextRequest::default())).await;
    assert!(off.search.entries().is_empty());
    assert!(off.outcome.context_sources.is_empty());
    let mut down = Setup::new(OptionalContextRequest::default());
    down.mode = Mode::Down;
    let control = run(down).await;
    assert_eq!(off.reviewer_users(), control.reviewer_users());
    assert!(off.reviewer_users().iter().all(|u| !u.contains(CALLER)));
}

/// Plan §2: a citation whose excerpt occurs only in the symbol section is
/// withheld; the section is context, not evidence.
#[serial_test::serial]
#[tokio::test]
async fn a_citation_to_a_symbol_context_line_is_withheld() {
    let mut setup = Setup::new(on());
    setup.reviewer = QuotingReviewer {
        quote: SIGNATURE,
        line: 1,
    };
    let seen = run(setup).await;
    assert!(
        seen.reviewer_users()
            .iter()
            .any(|u| u.contains("SIG_CANARY_9196")),
        "the reviewer saw the signature"
    );
    assert!(
        seen.outcome.result.findings.is_empty(),
        "{:?}",
        seen.outcome.result.findings
    );
    assert_eq!(seen.outcome.result.withheld_findings.len(), 1);
}

/// Ruling Q4 pattern: on the map-reduce path each chunk carries only its own
/// file's symbols.
#[serial_test::serial]
#[tokio::test]
async fn symbol_blocks_ride_only_their_own_chunk() {
    let mut setup = Setup::new(on());
    setup.diff = mapreduce_diff();
    let seen = run(setup).await;
    let chunks: Vec<String> = seen
        .reviewer_users()
        .into_iter()
        .filter(|u| u.contains("+++ b/"))
        .collect();
    assert!(chunks.len() > 1, "map-reduce ran");
    // Twelve f-files and `total` pass the 12-symbol cap by one; the last in
    // priority order (`src/f9.rs::f9`) is named, not shown.
    let mut shown = 0;
    for i in 0..12 {
        let own = format!("caller_f{i}_9196");
        let header = format!("+++ b/src/f{i}.rs");
        if !chunks.iter().any(|c| c.contains(&own)) {
            continue;
        }
        shown += 1;
        for chunk in &chunks {
            assert_eq!(
                chunk.contains(&own),
                chunk.contains(&header),
                "f{i}'s block rides only f{i}'s chunk"
            );
        }
    }
    assert_eq!(shown, 11, "every f-file block under the cap was shown");
    assert!(
        chunks
            .iter()
            .any(|c| c.contains("\"src/f9.rs::f9\": over symbol cap"))
    );
}

/// AC3: a daemon that is down omits the section, records the row
/// `unavailable`, and the review completes with a verdict.
#[serial_test::serial]
#[tokio::test]
async fn an_outage_leaves_the_review_running_without_the_section() {
    let mut setup = Setup::new(on());
    setup.mode = Mode::Down;
    let seen = run(setup).await;
    assert_eq!(seen.row().state, SourceState::Unavailable);
    assert!(
        seen.reviewer_users()
            .iter()
            .all(|u| !u.contains("## Changed symbols"))
    );
    assert_ne!(seen.outcome.result.status, ReviewStatus::Skipped);
    assert_eq!(seen.outcome.result.findings.len(), 1, "the review ran");
}

/// Condition 2 (Architect risk 2): a "not ready" reply is the AC3 outage
/// arm: section left out, row `unavailable`, the review completes.
#[serial_test::serial]
#[tokio::test]
async fn a_not_ready_daemon_omits_the_section_and_the_review_completes() {
    let mut setup = Setup::new(on());
    setup.mode = Mode::NotReady;
    let seen = run(setup).await;
    assert_eq!(seen.row().state, SourceState::Unavailable);
    let detail = seen.row().detail.clone().unwrap_or_default();
    assert!(detail.contains("not ready"), "{detail}");
    assert!(
        seen.reviewer_users()
            .iter()
            .all(|u| !u.contains("## Changed symbols"))
    );
    assert_ne!(seen.outcome.result.status, ReviewStatus::Skipped);
    assert_eq!(seen.outcome.result.findings.len(), 1, "the review ran");
}

/// Arm 21: an analyze outage does not change the symbol section; B5 never
/// reads trusty-analyze.
#[serial_test::serial]
#[tokio::test]
async fn analyze_down_does_not_change_symbol_context() {
    let mut setup = Setup::new(on());
    setup.analyze_down = true;
    let seen = run(setup).await;
    assert_eq!(seen.outcome.result.status, ReviewStatus::Degraded);
    assert_eq!(seen.row().state, SourceState::Used);
    assert!(seen.reviewer_users().iter().any(|u| u.contains(CALLER)));
}

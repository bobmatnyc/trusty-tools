//! Tests for trace assembly, one per fail-closed branch (#6166).
//!
//! Why: every refusal the trace pass can make is an error arm, and an error arm
//! nothing exercises stops firing without anything going red. Each test below
//! drives a stub that produces exactly one failure and asserts the reason
//! reaches the record — a stub that dropped the error would leave the record
//! anchored, which is what these assertions catch.
//! What: the success path, then daemon-unreachable, index-absent,
//! symbol-unresolvable, symbol-absent-from-graph, and entry-file mismatch.
//! Test: included as `#[cfg(test)] mod tests` from `trace.rs`.

use std::sync::Mutex;

use async_trait::async_trait;

use super::super::trace_client::CallChainEntry;
use super::*;
use crate::report::metrics::Severity;

/// The file the fixture findings cite, written into a temp checkout.
const FILE: &str = "src/store.rs";

/// A doc block above a `const`, the shape both live REDs cite into.
const SOURCE: &str = "/// A guard ratio.\n\
/// More prose.\n\
pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;\n";

fn finding(title: &str, severity: Severity, line: Option<u64>) -> VerifiedFinding {
    VerifiedFinding {
        title: title.to_string(),
        severity,
        dimension: "scalability".to_string(),
        file: FILE.to_string(),
        line,
        evidence_quote: "const SHRINK_GUARD_RATIO_DIVISOR".to_string(),
        description: String::new(),
        business_impact: String::new(),
        remediation: String::new(),
        cost_effort: String::new(),
    }
}

/// A checkout holding [`FILE`] with [`SOURCE`] in it.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(FILE);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, SOURCE).expect("write");
    dir
}

/// What one stub answers with.
enum Answer {
    Entry(CallChainEntry),
    Fail(TraceError),
}

/// A [`TraceSource`] with a scripted answer and a call counter.
struct Stub {
    reachable: bool,
    answer: Answer,
    usages: Vec<TraceUsage>,
    entry_calls: Mutex<usize>,
}

impl Stub {
    fn answering(answer: Answer) -> Self {
        Self {
            reachable: true,
            answer,
            usages: Vec::new(),
            entry_calls: Mutex::new(0),
        }
    }

    fn entry_at(file: &str) -> Self {
        Self::answering(Answer::Entry(CallChainEntry {
            symbol: "SHRINK_GUARD_RATIO_DIVISOR".to_string(),
            file: file.to_string(),
            line: 3,
            signature: "pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;".to_string(),
        }))
    }
}

#[async_trait]
impl TraceSource for Stub {
    async fn reachable(&self) -> bool {
        self.reachable
    }

    async fn entry_node(
        &self,
        _index_id: &str,
        _symbol: &str,
    ) -> Result<CallChainEntry, TraceError> {
        *self.entry_calls.lock().expect("lock") += 1;
        match &self.answer {
            Answer::Entry(e) => Ok(e.clone()),
            Answer::Fail(e) => Err(e.clone()),
        }
    }

    async fn usages(
        &self,
        _index_id: &str,
        _symbol: &str,
        _path_prefix: &str,
        _limit: usize,
        _snippet_bytes: usize,
    ) -> Result<Vec<TraceUsage>, TraceError> {
        Ok(self.usages.clone())
    }
}

async fn trace_with(stub: &Stub, findings: &[VerifiedFinding]) -> TraceSet {
    let dir = checkout();
    assemble_traces(dir.path(), findings, stub, TraceLimits::default()).await
}

// ─── The success path ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_matching_entry_becomes_the_anchor() {
    let mut stub = Stub::entry_at(FILE);
    stub.usages = vec![TraceUsage {
        file: FILE.to_string(),
        line: 3,
        snippet: "const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;".to_string(),
    }];
    // Line 2 is inside the doc block — the citation shape both live REDs have.
    let set = trace_with(&stub, &[finding("guard", Severity::Red, Some(2))]).await;

    assert_eq!((set.candidates, set.assembled, set.no_trace), (1, 1, 0));
    let t = &set.traces[0];
    assert_eq!(t.no_trace, None);
    assert_eq!(t.symbol.as_deref(), Some("SHRINK_GUARD_RATIO_DIVISOR"));
    let anchor = t.anchor.as_ref().expect("anchor");
    assert_eq!(anchor.file, FILE);
    assert_eq!(anchor.line, 3);
    assert_eq!(t.usages.len(), 1);
    assert_eq!(t.usages_status, "");
}

/// The edge slot stays empty and says why, on every record — #6167.
#[tokio::test]
async fn the_edge_slot_is_empty_and_names_its_blocker() {
    let set = trace_with(
        &Stub::entry_at(FILE),
        &[finding("guard", Severity::Red, Some(2))],
    )
    .await;
    let t = &set.traces[0];
    assert!(t.call_edges.is_empty());
    assert_eq!(t.call_edges_status, CALL_EDGES_DISABLED);
    assert!(
        t.call_edges_status.contains("#6167"),
        "{}",
        t.call_edges_status
    );
}

#[tokio::test]
async fn candidates_are_every_red_plus_a_bounded_amber_tail() {
    let mut findings = vec![finding("r1", Severity::Red, Some(2))];
    for i in 0..25 {
        findings.push(finding(&format!("a{i}"), Severity::Amber, Some(2)));
    }
    findings.push(finding("g", Severity::Green, Some(2)));
    findings.push(finding("r2", Severity::Red, Some(2)));

    let limits = TraceLimits::default();
    let picked = candidates(&findings, limits);
    assert_eq!(picked.len(), 2 + limits.max_amber);
    assert_eq!(picked[0].title, "r1");
    assert_eq!(picked[1].title, "r2");
    assert_eq!(picked[2].title, "a0");
    assert!(picked.iter().all(|f| f.severity != Severity::Green));
}

#[tokio::test]
async fn the_set_counts_match_its_records() {
    let set = trace_with(
        &Stub::entry_at("other/file.rs"),
        &[
            finding("a", Severity::Red, Some(2)),
            finding("b", Severity::Amber, Some(2)),
        ],
    )
    .await;
    assert_eq!(set.candidates, set.traces.len());
    assert_eq!(set.assembled + set.no_trace, set.candidates);
    assert_eq!(set.no_trace, 2);
}

// ─── Branch 1: the daemon is not there ────────────────────────────────────────

#[tokio::test]
async fn an_unreachable_daemon_refuses_every_candidate() {
    let mut stub = Stub::entry_at(FILE);
    stub.reachable = false;
    let set = trace_with(
        &stub,
        &[
            finding("a", Severity::Red, Some(2)),
            finding("b", Severity::Amber, Some(2)),
        ],
    )
    .await;

    assert_eq!(set.no_trace, 2);
    for t in &set.traces {
        let reason = t.no_trace.as_deref().expect("refused");
        assert!(reason.contains("not reachable"), "{reason}");
        assert!(t.anchor.is_none());
    }
    // Nothing was asked of a daemon that is not there.
    assert_eq!(*stub.entry_calls.lock().expect("lock"), 0);
}

// ─── Branch 2: the index is not registered ────────────────────────────────────

#[tokio::test]
async fn an_absent_index_refuses_every_candidate_after_one_call() {
    let stub = Stub::answering(Answer::Fail(TraceError::IndexAbsent("idx".to_string())));
    let set = trace_with(
        &stub,
        &[
            finding("a", Severity::Red, Some(2)),
            finding("b", Severity::Amber, Some(2)),
            finding("c", Severity::Amber, Some(2)),
        ],
    )
    .await;

    assert_eq!(set.no_trace, 3);
    for t in &set.traces {
        let reason = t.no_trace.as_deref().expect("refused");
        assert!(reason.contains("not registered"), "{reason}");
    }
    // An absent index answers the same way for all three; asking twice more
    // would spend live HTTP calls to learn nothing.
    assert_eq!(*stub.entry_calls.lock().expect("lock"), 1);
}

// ─── Branch 3: the citation resolves to no declaration ────────────────────────

#[tokio::test]
async fn a_citation_with_no_declaration_is_refused_before_any_call() {
    let stub = Stub::entry_at(FILE);
    let mut f = finding("a", Severity::Red, Some(2));
    f.file = "src/absent.rs".to_string();
    let set = trace_with(&stub, &[f]).await;

    let reason = set.traces[0].no_trace.as_deref().expect("refused");
    assert!(reason.contains("no item declaration found"), "{reason}");
    assert_eq!(set.traces[0].symbol, None);
    assert_eq!(*stub.entry_calls.lock().expect("lock"), 0);
}

#[tokio::test]
async fn a_finding_with_no_line_is_refused() {
    let set = trace_with(&Stub::entry_at(FILE), &[finding("a", Severity::Red, None)]).await;
    let reason = set.traces[0].no_trace.as_deref().expect("refused");
    assert!(reason.contains("no item declaration found"), "{reason}");
}

// ─── Branch 4: the symbol is not in the graph ─────────────────────────────────

/// Observed live: `MAX_ALLOC_PROBES` is declared in the checkout and answers
/// `404 entry point not found`. That is a fact about the graph, not about the
/// daemon, so it refuses ONE candidate rather than halting the pass.
#[tokio::test]
async fn a_symbol_absent_from_the_graph_refuses_only_its_own_candidate() {
    let stub = Stub::answering(Answer::Fail(TraceError::SymbolAbsent(
        "MAX_ALLOC_PROBES".to_string(),
    )));
    let set = trace_with(
        &stub,
        &[
            finding("a", Severity::Red, Some(2)),
            finding("b", Severity::Amber, Some(2)),
        ],
    )
    .await;

    assert_eq!(set.no_trace, 2);
    for t in &set.traces {
        let reason = t.no_trace.as_deref().expect("refused");
        assert!(reason.contains("not in the symbol graph"), "{reason}");
    }
    assert_eq!(*stub.entry_calls.lock().expect("lock"), 2);
}

// ─── Branch 5: the entry landed in another file ───────────────────────────────

/// 13 of 30 candidates on the live engagement resolved to a symbol the graph
/// placed in a different file. Admitting one would attach another crate's
/// declaration to this finding — the #6167 defect, one node earlier.
#[tokio::test]
async fn an_entry_in_another_file_is_an_ambiguous_name() {
    let stub = Stub::entry_at("crates/elsewhere/src/lib.rs");
    let set = trace_with(&stub, &[finding("a", Severity::Red, Some(2))]).await;

    let t = &set.traces[0];
    let reason = t.no_trace.as_deref().expect("refused");
    assert_eq!(
        reason,
        "no trace: symbol SHRINK_GUARD_RATIO_DIVISOR resolved to \
         crates/elsewhere/src/lib.rs — ambiguous name"
    );
    assert!(t.anchor.is_none(), "a mismatched entry is never an anchor");
    assert_eq!(t.symbol.as_deref(), Some("SHRINK_GUARD_RATIO_DIVISOR"));
}

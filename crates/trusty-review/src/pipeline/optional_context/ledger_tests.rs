//! `ContextLedger::finish`: the full row set, keyed on the request (#9194).
//!
//! Why: AC2 asks for every source when reporting is on, and amendment 1
//! requires a missing row the request asked for to read `unavailable`, never
//! `not_requested`.
//! What: drives `finish` directly with hand-built rows and gate facts.
//! Test: this module.

use super::*;
use crate::models::SourceState;
use crate::pipeline::optional_context::IssueDoc;

/// The canonical row order when reporting is on.
pub(crate) const CANONICAL: [&str; 8] = [
    "pr_body",
    "caller_context",
    "issues",
    "spec_docs",
    "claude_md",
    "search",
    "analyze",
    "external_sources",
];

fn rows() -> ContextRows {
    ContextRows {
        search: ContextSourceRecord::new("search", SourceState::Used),
        analyze: ContextSourceRecord::new("analyze", SourceState::Absent),
        external: ContextSourceRecord::new("external_sources", SourceState::NotRequested),
    }
}

fn names(records: &[ContextSourceRecord]) -> Vec<&str> {
    records.iter().map(|r| r.source.as_str()).collect()
}

fn find<'a>(records: &'a [ContextSourceRecord], source: &str) -> &'a ContextSourceRecord {
    records
        .iter()
        .find(|r| r.source == source)
        .unwrap_or_else(|| panic!("no `{source}` row in {records:?}"))
}

/// #9194 X1: with only `report_context`, every input the request did not
/// ask for is `not_requested` with its reason, in canonical order.
#[test]
fn ledger_fills_not_requested_rows() {
    let request = OptionalContextRequest::default().with_report_context(true);
    let mut ledger = ContextLedger::new(true);
    ledger.push(ContextSourceRecord::new(
        "caller_context",
        SourceState::Used,
    ));
    ledger.finish(&request, rows(), &GateFacts::default());
    let records = ledger.into_records();
    assert_eq!(names(&records), CANONICAL);
    for (source, detail) in [
        ("pr_body", "include_pr_body is off"),
        ("issues", "no issue_docs were sent"),
        ("spec_docs", "spec_docs is off"),
        ("claude_md", "claude_md is off"),
    ] {
        let row = find(&records, source);
        assert_eq!(
            (row.state, row.detail.as_deref()),
            (SourceState::NotRequested, Some(detail)),
            "{source}"
        );
    }
}

/// #9194 amendment 1: an input the request asked for whose row was never
/// recorded reads `unavailable`, "row not recorded" — the fill is keyed on
/// the request, not on the row being missing.
#[test]
fn a_requested_row_that_was_not_recorded_is_unavailable() {
    let request = OptionalContextRequest::default()
        .with_issue_docs(Vec::<IssueDoc>::new())
        .with_spec_docs(true);
    let mut ledger = ContextLedger::new(true);
    ledger.finish(&request, rows(), &GateFacts::default());
    let records = ledger.into_records();
    for source in ["issues", "spec_docs", "caller_context"] {
        let row = find(&records, source);
        assert_eq!(
            (row.state, row.detail.as_deref()),
            (SourceState::Unavailable, Some("row not recorded")),
            "{source}"
        );
    }
    assert_eq!(find(&records, "claude_md").state, SourceState::NotRequested);
}

/// #9194 P5: a second `finish` replaces rows by name; none is duplicated.
#[test]
fn finish_does_not_duplicate_a_row() {
    let request = OptionalContextRequest::default().with_report_context(true);
    let mut ledger = ContextLedger::new(true);
    ledger.finish(&request, rows(), &GateFacts::default());
    let mut again = rows();
    again.search = ContextSourceRecord::new("search", SourceState::Absent);
    ledger.finish(&request, again, &GateFacts::default());
    let records = ledger.into_records();
    assert_eq!(names(&records), CANONICAL);
    assert_eq!(find(&records, "search").state, SourceState::Absent);
}

/// #9194 (ruling Q5): a dependency the gate degraded is `unavailable` with
/// the gate's reason, even when its own row says it was used.
#[test]
fn a_gate_fact_marks_its_row_unavailable() {
    let request = OptionalContextRequest::default().with_report_context(true);
    let facts = GateFacts {
        search: Some("trusty-search at x: index degraded".to_string()),
        analyze: None,
    };
    let mut ledger = ContextLedger::new(true);
    ledger.finish(&request, rows(), &facts);
    let records = ledger.into_records();
    let search = find(&records, "search");
    assert_eq!(
        (search.state, search.detail.as_deref()),
        (
            SourceState::Unavailable,
            Some("trusty-search at x: index degraded")
        )
    );
    assert_eq!(find(&records, "analyze").state, SourceState::Absent);
}

/// #9194: a ledger that is off keeps nothing, `finish` included.
#[test]
fn a_disabled_ledger_finishes_empty() {
    let mut ledger = ContextLedger::new(false);
    ledger.finish(
        &OptionalContextRequest::default(),
        rows(),
        &GateFacts::default(),
    );
    assert!(ledger.into_records().is_empty());
}

/// #9194: `finish` passes every row and item detail through `cap_detail`,
/// so a producer that skipped it (the `pr_body` fetch error, a docs
/// `unavailable_row`) cannot leak a token into CLI or MCP output.
#[test]
fn finish_caps_every_row_and_item_detail() {
    let leak = "PR metadata fetch failed: GET \
                https://api.github.com/repos/o/r/pulls/7?access_token=s3cr3tvalue\nstatus 401";
    let request = OptionalContextRequest::default()
        .with_pr_body(true)
        .with_spec_docs(true);
    let mut ledger = ContextLedger::new(true);
    ledger.push(ContextSourceRecord::new("pr_body", SourceState::Unavailable).with_detail(leak));
    let mut docs =
        ContextSourceRecord::new("spec_docs", SourceState::Unavailable).with_detail(leak);
    docs.items = vec![
        crate::models::ContextItemRecord::new("docs/a.md", SourceState::Unavailable, 0, 0)
            .with_detail(leak),
    ];
    ledger.push(docs);
    ledger.finish(&request, rows(), &GateFacts::default());
    let records = ledger.into_records();
    let pr_body = find(&records, "pr_body");
    let spec_docs = find(&records, "spec_docs");
    for detail in [
        &pr_body.detail,
        &spec_docs.detail,
        &spec_docs.items[0].detail,
    ] {
        let detail = detail.as_deref().unwrap_or_default();
        assert!(!detail.contains("s3cr3tvalue"), "token survived: {detail}");
        assert!(detail.contains("access_token=[redacted]"), "{detail}");
        assert!(!detail.contains('\n'), "{detail}");
    }
}

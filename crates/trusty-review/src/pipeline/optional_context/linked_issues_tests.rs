//! Tests for `fetch_linked_issues`: selection, fetch failures, fencing
//! (#9197, B2b).
//!
//! Why: every branch must leave an item or a row state, never an error, and
//! a wrong implementation would fetch another repository's issue, fall back
//! to the host `gh` login, reorder blocks, or let a fetched body escape its
//! fence.
//! What: the [`FakeFetcher`] harness, plus the selection tests. The
//! failure branches are in `linked_issues_failure_tests.rs`, the cap tests
//! in `linked_issues_budget_tests.rs`, the pipeline tests in
//! `runner_linked_issues_tests.rs`.
//! Test: this module.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use trusty_common::intent_source::IsrError;

use super::*;
use crate::models::ContextSourceRecord;
use crate::pipeline::{
    citation_gate::DocCorpus,
    optional_context::{files_render::FileSections, issues::issue_section},
};

/// What [`FakeFetcher`] answers for one issue number.
pub(crate) enum Answer {
    /// The issue, with this title and body.
    Doc(String, String),
    /// `IsrError::TicketFetch` with this text.
    Fail(String),
    /// `IsrError::NoToken`.
    NoToken,
    /// Never answers.
    Hang,
    /// The issue body, after this many seconds.
    Late(u64, String),
    /// An answer from this `html_url` and number, with this body (a
    /// transferred issue, a PR, or another repository's issue).
    At(String, u64, String),
}

/// A [`TicketFetcher`] with fixed answers that records each call's id; a
/// number with no answer is a 404.
#[derive(Default)]
pub(crate) struct FakeFetcher {
    answers: HashMap<u64, Answer>,
    calls: Mutex<Vec<String>>,
}

impl FakeFetcher {
    /// This fetcher answering `n` with `answer`.
    pub(crate) fn answer(mut self, n: u64, answer: Answer) -> Self {
        self.answers.insert(n, answer);
        self
    }

    /// This fetcher answering `n` with title `Title n` and `body`.
    pub(crate) fn doc(self, n: u64, body: &str) -> Self {
        self.answer(n, Answer::Doc(format!("Title {n}"), body.to_string()))
    }

    /// The ids fetched, in call order.
    pub(crate) fn ids(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

fn ticket(id: String, title: &str, body: &str, url: Option<&str>) -> TicketData {
    let url = url.map(str::to_string);
    let (title, body, backend) = (title.to_string(), body.to_string(), "github".to_string());
    TicketData {
        id,
        title,
        body,
        url,
        backend,
    }
}

#[async_trait]
impl TicketFetcher for FakeFetcher {
    async fn fetch(&self, _owner: &str, _repo: &str, id: &str) -> Result<TicketData, IsrError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(id.to_string());
        }
        let n: u64 = id.parse().unwrap_or(0);
        let url = format!("https://github.com/acme/billing/issues/{n}");
        match self.answers.get(&n) {
            None => Err(IsrError::TicketFetch(format!(
                "github API failed: 404 Not Found: {n}"
            ))),
            Some(Answer::Doc(title, body)) => Ok(ticket(format!("#{n}"), title, body, Some(&url))),
            Some(Answer::Fail(text)) => Err(IsrError::TicketFetch(text.clone())),
            Some(Answer::NoToken) => Err(IsrError::NoToken("fixture".to_string())),
            Some(Answer::Hang) => std::future::pending().await,
            Some(Answer::Late(secs, body)) => {
                tokio::time::sleep(Duration::from_secs(*secs)).await;
                Ok(ticket(format!("#{n}"), "Late", body, Some(&url)))
            }
            Some(Answer::At(at, m, body)) => {
                Ok(ticket(format!("#{m}"), "Elsewhere", body, Some(at)))
            }
        }
    }
}

/// The fixture PR: `acme/billing#pr`.
pub(crate) fn github(pr: u64) -> DiffSource {
    let (owner, repo, token) = ("acme".into(), "billing".into(), "fixture-token".into());
    DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    }
}

/// A supplied doc.
pub(crate) fn doc(id: &str, body: &str) -> IssueDoc {
    IssueDoc::new(id, Some("Supplied"), body, None).expect("valid doc")
}

/// What one [`apply_linked_issues`] call left.
pub(crate) struct Run {
    pub(crate) section: String,
    pub(crate) row: ContextSourceRecord,
    pub(crate) calls: Vec<String>,
}

/// Render `request`'s supplied docs as `apply_caller_context` does, then run
/// the fetch step against `source`, asserting at most one `issues` row.
pub(crate) async fn run(
    request: OptionalContextRequest,
    body: PrBody<'_>,
    source: &DiffSource,
    fetcher: FakeFetcher,
) -> Run {
    let fetcher = Arc::new(fetcher);
    let mut ledger = ContextLedger::new(request.ledger_enabled());
    let mut applied = AppliedContext {
        body_in_refs: true,
        sections: issue_section(request.issue_docs.as_deref(), &mut ledger),
        doc_sections: String::new(),
        docs: DocCorpus::default(),
        files: FileSections::default(),
        symbols: FileSections::default(),
    };
    let mut options = ReviewOptions::new(request);
    options.ticket_fetcher = Some(fetcher.clone());
    let call = LinkedIssuesCall::new(&options, source, body);
    apply_linked_issues(&mut applied, call, &mut ledger).await;
    let mut rows: Vec<_> = ledger.into_records();
    rows.retain(|r| r.source == "issues");
    assert!(rows.len() <= 1, "at most one issues row: {rows:?}");
    let row = rows
        .pop()
        .unwrap_or_else(|| ContextSourceRecord::new("none", SourceState::NotRequested));
    Run {
        section: applied.sections,
        row,
        calls: fetcher.ids(),
    }
}

/// The fetch step on `acme/billing#7`, flag on, for PR `body`.
pub(crate) async fn fetch(
    body: &str,
    supplied: Option<Vec<IssueDoc>>,
    fetcher: FakeFetcher,
) -> Run {
    let mut request = OptionalContextRequest::default().with_fetch_linked_issues(true);
    if let Some(docs) = supplied {
        request = request.with_issue_docs(docs);
    }
    run(request, PrBody::Fetched(body), &github(7), fetcher).await
}

/// The item `id` of `row`.
pub(crate) fn item<'a>(row: &'a ContextSourceRecord, id: &str) -> &'a ContextItemRecord {
    let found = row.items.iter().find(|i| i.id == id);
    found.unwrap_or_else(|| panic!("no item {id} in {:?}", row.items))
}

/// AC13: the flag is a new input and turns the ledger on.
#[test]
fn fetch_linked_issues_turns_the_ledger_on() {
    let request = OptionalContextRequest::default();
    assert!(!request.fetch_linked_issues && !request.requested_new());
    let request = request.with_fetch_linked_issues(true);
    assert!(request.requested_new() && request.ledger_enabled());
}

/// AC2: with the flag off nothing is fetched, and `issue_docs` alone leaves
/// the section and the row exactly as B2a renders them.
#[tokio::test]
async fn flag_off_makes_no_ticket_call() {
    let docs = vec![doc("#3", "SUPPLIED_3")];
    let mut ledger = ContextLedger::new(true);
    let want = issue_section(Some(&docs), &mut ledger);
    let request = OptionalContextRequest::default().with_issue_docs(docs);
    let seen = run(
        request,
        PrBody::Fetched("Refs #5"),
        &github(7),
        FakeFetcher::default(),
    )
    .await;
    assert!(seen.calls.is_empty());
    assert_eq!(
        (seen.section, Some(seen.row)),
        (want, ledger.into_records().pop())
    );
    let off = OptionalContextRequest::default();
    let seen = run(
        off,
        PrBody::Fetched("Refs #5"),
        &github(7),
        FakeFetcher::default(),
    )
    .await;
    assert!(seen.calls.is_empty() && seen.section.is_empty());
    assert_eq!(seen.row.source, "none", "no ledger, no row");
}

/// AC4: same-repository refs only, the PR itself silent, the scanner's
/// exclusions kept, body order kept.
#[test]
fn select_refs_skips_supplied_self_and_other_repos_in_body_order() {
    let body = "Fixes #4, #7, other/repo#5 and #3\nSee discussion in #42\nRefs ADR-0043\n\
                ```\nFixes #9\n```\nRefs `#10`\nCloses acme/billing#4, #2";
    let sel = select_refs(body, "acme", "billing", 7, &[doc("#3", "")]);
    assert_eq!(sel.fetch, [4, 2]);
    let ids: Vec<&str> = sel.items.iter().map(|i| i.id.as_str()).collect();
    // Ruling R3: the supplied `#3` is counted, never an item.
    assert_eq!((ids, sel.supplied_refs), (vec!["(other repositories)"], 1));
    assert_eq!(
        sel.items[0].detail.as_deref(),
        Some("1 refs to other repositories are not fetched")
    );
}

/// AC4 (ruling Q5): `Owner/Repo#N` names the reviewed repository whatever
/// its case.
#[test]
fn a_mixed_case_owner_repo_ref_is_fetched() {
    let sel = select_refs("Refs Acme/BILLING#12", "acme", "billing", 7, &[]);
    assert_eq!((sel.fetch, sel.items), (vec![12], Vec::new()));
}

#[path = "linked_issues_budget_tests.rs"]
mod budget;

#[path = "linked_issues_failure_tests.rs"]
mod failure;

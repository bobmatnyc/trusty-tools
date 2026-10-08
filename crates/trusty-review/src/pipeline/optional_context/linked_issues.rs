//! `fetch_linked_issues`: the issues a PR body links, fetched from GitHub
//! (#9197, B2b; epic #9191).
//!
//! Why: B2a lets a caller hand the reviewer issue text. Most callers do not
//! hold it, but the PR body names the issues (`Refs #12`), so the review can
//! fetch them itself, opt-in, after any supplied docs.
//! What: [`select_refs`] reads the keyword-linked refs of the RAW PR body and
//! picks at most 5 to fetch; [`apply_linked_issues`] fetches them through a
//! `TicketFetcher` with the diff read's token, re-renders `## Linked issues`
//! (supplied docs first, then fetched) and replaces the `issues` ledger row.
//! Every failure is an item or row state; none stops the review. Fetched
//! text reaches the reviewer prompt and the refs corpus only, never the
//! verifier (Ruling A).
//! Test: `linked_issues_tests.rs`, `runner_linked_issues_tests.rs`.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures_util::future::join_all;
use tokio::time::timeout;
use trusty_common::intent_source::{
    TicketData, TicketFetcher, backend_fetcher::BackendTicketFetcher, extract_issue_refs,
};

use crate::{
    config::constants::{
        LINKED_ISSUE_TIMEOUT_SECS, MAX_ISSUE_DOC_LINE_CHARS, MAX_LINKED_ISSUE_FETCHES,
    },
    integrations::context::ticket_token::FixedTicketToken,
    models::{ContextItemRecord, SourceState},
    pipeline::diff::DiffSource,
};

use super::{
    OptionalContextRequest, ReviewOptions,
    assemble::{AppliedContext, PrBody},
    issues::{IssueDoc, IssueRender, issues_row, omitted, section_cap_reason},
    ledger::ContextLedger,
};

/// The detail when the review has no PR to read links from.
pub(crate) const LOCAL_DIFF_DETAIL: &str = "local diff has no PR";

/// The detail when the PR body links no issue (B2b amendment 13).
pub(crate) const NO_REFS_DETAIL: &str = "no keyword-linked issue refs in the PR body";

/// Everything [`apply_linked_issues`] reads from the runner (#9197, B2b).
///
/// Why: `runner.rs` has no line budget, so the runner makes one call.
/// What: the request, the ticket-fetch seam, the diff source (owner, repo,
/// PR number and token of a GitHub PR) and what the runner knows about the
/// PR body.
/// Test: `runner_linked_issues_tests.rs`.
pub(crate) struct LinkedIssuesCall<'a> {
    request: &'a OptionalContextRequest,
    seam: Option<Arc<dyn TicketFetcher>>,
    source: &'a DiffSource,
    body: PrBody<'a>,
}

impl<'a> LinkedIssuesCall<'a> {
    /// What the runner knows after the PR metadata read.
    pub(crate) fn new(
        options: &'a ReviewOptions,
        source: &'a DiffSource,
        body: PrBody<'a>,
    ) -> Self {
        Self {
            request: &options.request,
            seam: options.ticket_fetcher.clone(),
            source,
            body,
        }
    }
}

/// The refs one PR body yields: what to fetch, and bounded ledger items.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Issue numbers to fetch, in body order, at most 5.
    pub(crate) fetch: Vec<u64>,
    /// Items for refs that are not fetched; each id is a fixed label.
    pub(crate) items: Vec<ContextItemRecord>,
    /// Refs to a supplied doc's number: not fetched, no item (ruling R3).
    pub(crate) supplied_refs: usize,
}

/// Pick the issues to fetch from the raw PR `body` (#9197, B2b).
///
/// Why: a PR body is untrusted and unbounded; selection must cap the calls
/// and the ledger it produces, and never fetch from another repository.
/// What: reads `extract_issue_refs(body)` in order. A ref is a candidate when
/// it is bare `#N` or `owner/repo#N` naming `owner`/`repo` (ASCII
/// case-insensitive, ruling Q5). The PR's own number and a repeated number
/// are skipped silently. A number a supplied doc already carries is never
/// fetched, whatever that doc's body (amendment 8), and adds no item: the
/// supplied doc has its own (ruling R3), so it is only counted in
/// `supplied_refs`. The first 5 other candidates are fetched.
/// Other-repository refs fold into one `(other repositories)` item and refs
/// past the limit into one `(over the 5-issue fetch limit)` item; neither
/// takes fetch budget, and no untrusted `owner/repo` text reaches an item
/// (amendment 4).
/// Test: `select_refs_skips_supplied_self_and_other_repos_in_body_order`,
/// `a_mixed_case_owner_repo_ref_is_fetched`, `ten_thousand_bare_refs_make_five_calls_and_a_bounded_row`,
/// `a_thousand_long_other_repo_refs_fold_into_one_item`.
pub(crate) fn select_refs(
    body: &str,
    owner: &str,
    repo: &str,
    pr: u64,
    supplied: &[IssueDoc],
) -> Selection {
    let supplied: HashSet<&str> = supplied.iter().map(|d| d.id.as_str()).collect();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut out = Selection::default();
    let (mut other, mut over) = (0_usize, 0_usize);
    for found in extract_issue_refs(body) {
        if let Some((o, r)) = &found.owner_repo
            && !(o.eq_ignore_ascii_case(owner) && r.eq_ignore_ascii_case(repo))
        {
            other += 1;
            continue;
        }
        if found.number == pr || !seen.insert(found.number) {
            continue; // the PR itself, or a repeat: silent, no budget
        }
        let id = format!("#{}", found.number);
        if supplied.contains(id.as_str()) {
            out.supplied_refs += 1; // #9197 ruling R3: informational, no item
        } else if out.fetch.len() < MAX_LINKED_ISSUE_FETCHES {
            out.fetch.push(found.number);
        } else {
            over += 1;
        }
    }
    let folds = [
        (
            "(other repositories)".to_string(),
            other,
            "refs to other repositories are not fetched",
        ),
        (
            format!("(over the {MAX_LINKED_ISSUE_FETCHES}-issue fetch limit)"),
            over,
            "more same-repository refs are not fetched",
        ),
    ];
    for (label, count, what) in folds {
        if count > 0 {
            out.items
                .push(omitted(&label, 0, format!("{count} {what}")));
        }
    }
    out
}

/// The ticket fetcher for `source`, or why there is no PR to fetch for
/// (#9197, B2b amendment 2).
///
/// Why: `docs::fetcher_for` needs a head SHA; an issue fetch does not.
/// What: a local diff (`LocalFile`, `GitRange`) is `Err` with
/// [`LOCAL_DIFF_DETAIL`]. A GitHub PR gets `seam` when set, otherwise a
/// `BackendTicketFetcher` over [`FixedTicketToken`] holding the diff read's
/// token. Building it opens no socket.
/// Test: `ticket_fetcher_for_picks_by_diff_source_and_the_seam_wins`.
pub(crate) fn ticket_fetcher_for(
    source: &DiffSource,
    seam: Option<&Arc<dyn TicketFetcher>>,
) -> Result<Arc<dyn TicketFetcher>, String> {
    let DiffSource::Github { token, .. } = source else {
        return Err(LOCAL_DIFF_DETAIL.to_string());
    };
    if let Some(seam) = seam {
        return Ok(seam.clone());
    }
    let resolver = Box::new(FixedTicketToken(token.clone()));
    Ok(Arc::new(BackendTicketFetcher::new(resolver)))
}

/// Fetch the issues the PR body links and re-render the issue section
/// (#9197, B2b).
///
/// Why: AC1-AC12: fetched issues follow the supplied docs, under the same
/// caps, opt-in, and no failure blocks the review.
/// What: with `fetch_linked_issues` off, does nothing. A local diff or a
/// failed PR metadata read records the `issues` row `unavailable` with the
/// reason, keeps the supplied docs, and fetches nothing. Otherwise the
/// supplied docs are placed first; when they left no characters, every
/// candidate is `omitted` with the cap reason and nothing is fetched
/// (amendment 7). Else up to 5 fetches run concurrently, each under
/// `LINKED_ISSUE_TIMEOUT_SECS`; a failure or timeout is an `unavailable`
/// item, an answer from another repository or number is an `omitted`
/// [`MOVED_DETAIL`] item and shows nothing, and a fetched issue is placed in
/// body order. `applied.sections` gets
/// the new section and the `issues` row is replaced by name (amendment 6).
/// Error text is kept only in item details, which `ContextLedger::finish`
/// passes through `cap_detail`.
/// Test: `fetched_issue_reaches_the_prompt_and_the_corpus`,
/// `flag_off_makes_no_ticket_call`, `local_diff_is_unavailable_and_fetches_nothing`,
/// `fetch_is_skipped_when_supplied_fill_the_cap`, `fetch_error_is_recorded_unavailable_with_its_text`.
pub(crate) async fn apply_linked_issues(
    applied: &mut AppliedContext,
    call: LinkedIssuesCall<'_>,
    ledger: &mut ContextLedger,
) {
    if !call.request.fetch_linked_issues {
        return;
    }
    let supplied = call.request.issue_docs.as_deref().unwrap_or(&[]);
    let mut render = IssueRender::new();
    render.supplied(supplied);
    let target = match (call.body, call.source) {
        (
            PrBody::Fetched(body),
            DiffSource::Github {
                owner, repo, pr, ..
            },
        ) => ticket_fetcher_for(call.source, call.seam.as_ref())
            .map(|fetcher| (fetcher, body, owner.as_str(), repo.as_str(), *pr)),
        (PrBody::Failed(e), _) => Err(format!("PR metadata fetch failed: {e}")),
        _ => Err(LOCAL_DIFF_DETAIL.to_string()),
    };
    let row = match target {
        Ok((fetcher, body, owner, repo, pr)) => {
            let selection = select_refs(body, owner, repo, pr, supplied);
            let no_refs = selection.fetch.is_empty()
                && selection.items.is_empty()
                && selection.supplied_refs == 0;
            place_fetched(&mut render, fetcher.as_ref(), owner, repo, selection).await;
            let (section, items) = render.finish();
            applied.sections = section;
            let mut row = issues_row(items);
            if no_refs && row.state == SourceState::Absent {
                row.detail = Some(NO_REFS_DETAIL.to_string());
            }
            row
        }
        Err(detail) => {
            let (section, items) = render.finish();
            applied.sections = section;
            let mut row = issues_row(items);
            row.state = SourceState::Unavailable;
            row.detail = Some(detail);
            row
        }
    };
    ledger.upsert(row);
}

/// Fetch `selection.fetch` (unless the supplied docs filled the section) and
/// place each result, then the selection's own items.
async fn place_fetched(
    render: &mut IssueRender,
    fetcher: &dyn TicketFetcher,
    owner: &str,
    repo: &str,
    selection: Selection,
) {
    if render.remaining() == 0 {
        // #9197 amendment 7: the supplied docs filled the section; no call.
        for n in &selection.fetch {
            render.push_item(omitted(&format!("#{n}"), 0, section_cap_reason()));
        }
    } else {
        let results = fetch_all(fetcher, owner, repo, &selection.fetch).await;
        for (n, result) in selection.fetch.iter().zip(results) {
            match result {
                Fetched::Doc(doc) => render.fetched(&doc),
                Fetched::Moved => {
                    render.push_item(omitted(&format!("#{n}"), 0, MOVED_DETAIL.to_string()));
                }
                Fetched::Failed(detail) => {
                    let item =
                        ContextItemRecord::new(&format!("#{n}"), SourceState::Unavailable, 0, 0);
                    // #9197: raw error text; `finish` caps and redacts it.
                    render.push_item(item.with_detail(&detail));
                }
            }
        }
    }
    selection
        .items
        .into_iter()
        .for_each(|i| render.push_item(i));
}

/// The detail of a fetched issue that answered from elsewhere (#9197).
pub(crate) const MOVED_DETAIL: &str = "moved to another repository";

/// What one linked-issue fetch yielded.
enum Fetched {
    /// The issue, from the reviewed repository, under the linked number.
    Doc(IssueDoc),
    /// An answer for another repository or number: never shown.
    Moved,
    /// The error text, or the timeout.
    Failed(String),
}

/// Fetch every number concurrently, each under the timeout; results follow
/// `numbers`' order whatever order the fetches finish in.
async fn fetch_all(
    fetcher: &dyn TicketFetcher,
    owner: &str,
    repo: &str,
    numbers: &[u64],
) -> Vec<Fetched> {
    let limit = Duration::from_secs(LINKED_ISSUE_TIMEOUT_SECS);
    let fetches = numbers.iter().map(|n| async move {
        match timeout(limit, fetcher.fetch(owner, repo, &n.to_string())).await {
            Err(_) => Fetched::Failed(format!("timed out after {LINKED_ISSUE_TIMEOUT_SECS} s")),
            Ok(Err(e)) => Fetched::Failed(e.to_string()),
            Ok(Ok(data)) if !is_from(&data, owner, repo, *n) => Fetched::Moved,
            Ok(Ok(data)) => Fetched::Doc(doc_from_ticket(*n, data)),
        }
    });
    join_all(fetches).await
}

/// Whether `data` is item `n` of `owner`/`repo` (#9197 fix round, HIGH).
///
/// Why: GitHub answers a transferred issue with a 301 that the tickets
/// backend follows with the token, so a public PR's `Refs #N` could pull a
/// private repository's issue into the prompt and the citation corpus.
/// What: true only when the answer's number is `n` and its `html_url` starts
/// with `https://github.com/{owner}/{repo}/` (ASCII case-insensitive, ruling
/// Q5); a pull request of the same repository qualifies, a missing url does
/// not.
/// Test: `a_moved_issue_is_omitted_not_rendered`,
/// `a_same_number_answer_from_another_repo_is_omitted`,
/// `a_pull_request_in_the_reviewed_repo_still_renders`.
fn is_from(data: &TicketData, owner: &str, repo: &str, n: u64) -> bool {
    let prefix = format!("https://github.com/{owner}/{repo}/");
    let url_ok = data.url.as_deref().is_some_and(|u| {
        u.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
    });
    url_ok && data.id == format!("#{n}")
}

/// `data` as the doc for issue `n`; never fails (#9197 B2b amendment 9).
///
/// Why: the title and url render outside the data fence, so they must be
/// single bounded lines, but a fetched issue must never be lost over them.
/// What: the id is `#n` as linked. The title is its first line with control
/// characters as spaces, trimmed and cut to `MAX_ISSUE_DOC_LINE_CHARS`;
/// blank is `None`. The url is kept only when `IssueDoc::new` accepts it.
/// Test: `a_bad_fetched_title_or_url_never_fails_the_doc`.
pub(crate) fn doc_from_ticket(n: u64, data: TicketData) -> IssueDoc {
    let id = format!("#{n}");
    let first = data.title.split(['\n', '\r']).next().unwrap_or_default();
    let clean: String = first
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title: String = clean
        .trim()
        .chars()
        .take(MAX_ISSUE_DOC_LINE_CHARS)
        .collect();
    let url = data
        .url
        .as_deref()
        .and_then(|u| IssueDoc::new(&id, None, "", Some(u)).ok())
        .and_then(|d| d.url);
    IssueDoc {
        id,
        title: Some(title).filter(|t| !t.trim().is_empty()),
        body: data.body,
        url,
    }
}

#[cfg(test)]
#[path = "linked_issues_tests.rs"]
pub(crate) mod tests;

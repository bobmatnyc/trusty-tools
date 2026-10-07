//! ADR, spec and SLD docs and CLAUDE.md, read at the PR head SHA (#9193).
//!
//! Why: a reviewer that cannot see the ADR or spec a PR follows cannot judge
//! the PR against it. Epic #9191 phase B3 reads those docs, opt-in, at the
//! commit under review, never the default branch.
//! What: [`DocsCall`] gathers what the runner knows; [`apply_docs`] picks the
//! candidate paths (PR body first, then trusty-search hits), reads them
//! through a `DocFetcher` at the head SHA, and leaves the rendered sections
//! and the citable text in `AppliedContext`. Every failure omits the doc
//! and is recorded; none stops the review. The orchestrator is bypassed, as
//! B1 and B2a do (Architect ruling Q1).
//! Test: `docs_tests.rs`, `runner_spec_docs_tests.rs`.

use std::{sync::Arc, time::Duration};

use futures_util::future::join_all;
use tokio::time::timeout;

use crate::{
    config::{
        ReviewConfig,
        constants::{DOC_READ_TIMEOUT_SECS, MAX_DOC_DISCOVERY_HITS},
    },
    integrations::{
        context::contents_at_ref::{DocFetcher, GithubDocFetcher, is_head_sha},
        search_client::SearchClient,
    },
    pipeline::{
        citation_check::normalize_path, citation_gate::DocCorpus, diff::DiffSource,
        diff_analyzer::models::FilteredDiff, prompt::ReviewPrMeta, runner::ReviewDeps,
    },
};

use super::{
    OptionalContextRequest, ReviewOptions,
    assemble::AppliedContext,
    doc_refs::{extract_doc_paths, is_claude_md, is_doc_path, repo_relative},
    docs_render::{CLAUDE_MD, DocRead, Kind, Read, SPEC_DOCS, render_kind, unavailable_row},
    ledger::ContextLedger,
    seams::PrHead,
};

/// Nested CLAUDE.md files read after the root one.
const MAX_NESTED_CLAUDE_MD: usize = 3;

/// Everything [`apply_docs`] reads from the runner (#9193).
///
/// Why: plan amendment 13: the runner makes one call, so its line budget
/// holds.
/// What: the request and the doc-read seam, the search client and index, the
/// diff source (owner, repo and token of a GitHub PR), the PR title and
/// body, the paths the diff changes, and the head.
/// Test: `runner_spec_docs_tests.rs`.
pub(crate) struct DocsCall<'a> {
    request: &'a OptionalContextRequest,
    seam: Option<Arc<dyn DocFetcher>>,
    search: &'a dyn SearchClient,
    index: &'a str,
    source: &'a DiffSource,
    pr_meta: &'a ReviewPrMeta,
    changed: Vec<String>,
    head: PrHead,
}

impl<'a> DocsCall<'a> {
    /// What the runner knows before the head is attached.
    pub(crate) fn new(
        config: &'a ReviewConfig,
        deps: &'a ReviewDeps,
        options: &'a ReviewOptions,
        source: &'a DiffSource,
        pr_meta: &'a ReviewPrMeta,
        filtered: &FilteredDiff,
    ) -> Self {
        let changed = filtered
            .files
            .iter()
            .map(|f| normalize_path(&f.filename))
            .chain(
                filtered
                    .dropped_files
                    .iter()
                    .map(|d| normalize_path(&d.path)),
            )
            .collect();
        Self {
            request: &options.request,
            seam: options.doc_fetcher.clone(),
            search: deps.search.as_ref(),
            index: &config.search_index,
            source,
            pr_meta,
            changed,
            head: PrHead::default(),
        }
    }

    /// This call reading at `head`.
    pub(crate) fn at(mut self, head: &PrHead) -> Self {
        self.head = head.clone();
        self
    }
}

/// Read the requested docs at the head and leave them in `applied` (#9193).
///
/// Why: criteria 1, 3, 4 and 7: docs named in the PR body or found by
/// search are read at the head SHA, only when asked for, and no failure
/// blocks the review.
/// What: with neither flag on, does nothing (no read, no search, no row).
/// A local diff, or a head SHA that is not 40 or 64 lowercase hex, records
/// each requested row `unavailable` and reads nothing. Otherwise one
/// discovery search (failure recorded, never fatal), then up to the read
/// budget per kind, concurrently, each with a timeout. Renders `## Referenced
/// docs` and `## Repository conventions (CLAUDE.md)` into
/// `applied.doc_sections` and their kept text into `applied.docs`.
/// Test: `spec_docs_off_with_a_doc_path_in_the_body_reads_nothing`,
/// `empty_head_sha_never_fetches`, `malformed_sha_never_fetches`,
/// `review_diff_spec_docs_reports_unavailable_no_head_sha`,
/// `search_down_is_unavailable_and_explicit_paths_still_read`,
/// `claude_md_alone_fetches_only_claude_md`, `spec_docs_alone_never_fetches_claude_md`.
pub(crate) async fn apply_docs(
    applied: &mut AppliedContext,
    call: DocsCall<'_>,
    ledger: &mut ContextLedger,
) {
    let kinds: Vec<&Kind> = [
        (call.request.spec_docs, &SPEC_DOCS),
        (call.request.claude_md, &CLAUDE_MD),
    ]
    .into_iter()
    .filter_map(|(on, kind)| on.then_some(kind))
    .collect();
    if kinds.is_empty() {
        return;
    }
    let fetcher = match fetcher_for(&call) {
        Ok(fetcher) => fetcher,
        Err(detail) => {
            kinds
                .iter()
                .for_each(|k| ledger.push(unavailable_row(k, &detail)));
            return;
        }
    };
    let (hits, discovery_failure) = discover(&call).await;
    let sha = call.head.sha.as_str();
    let mut corpus = DocCorpus::at(sha);
    let mut sections = Vec::new();
    for kind in kinds {
        let mut paths = candidates(kind, &call, &hits);
        let skipped = paths.len().saturating_sub(kind.max_reads);
        paths.truncate(kind.max_reads);
        let reads = read_all(fetcher.as_ref(), &paths, &call).await;
        let (section, row) = render_kind(
            kind,
            sha,
            &reads,
            skipped,
            discovery_failure.as_deref(),
            &mut corpus,
        );
        ledger.push(row);
        if !section.is_empty() {
            sections.push(section);
        }
    }
    applied.doc_sections = sections.join("\n\n");
    applied.docs = corpus;
}

/// The fetcher for this call, or why no doc can be read (#9193 amendment 2).
fn fetcher_for(call: &DocsCall<'_>) -> Result<Arc<dyn DocFetcher>, String> {
    let DiffSource::Github {
        owner, repo, token, ..
    } = call.source
    else {
        return Err("no PR head SHA: a local diff has no commit to read docs at".to_string());
    };
    if !is_head_sha(&call.head.sha) {
        return Err(
            "no valid PR head SHA: the PR metadata read failed or returned a malformed \
                    SHA"
            .to_string(),
        );
    }
    if let Some(seam) = &call.seam {
        return Ok(seam.clone());
    }
    GithubDocFetcher::new(owner, repo, token)
        .map(|f| Arc::new(f) as Arc<dyn DocFetcher>)
        .map_err(|e| e.to_string())
}

/// Candidate paths for one kind, in read order (#9193 amendment 10).
fn candidates(kind: &Kind, call: &DocsCall<'_>, hits: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if kind.source == SPEC_DOCS.source {
        let (owner, repo) = match call.source {
            DiffSource::Github { owner, repo, .. } => (owner.as_str(), repo.as_str()),
            _ => ("", ""),
        };
        out = extract_doc_paths(&call.pr_meta.body, owner, repo);
        for hit in hits.iter().filter(|h| is_doc_path(h)) {
            if !out.contains(hit) {
                out.push(hit.clone());
            }
        }
        return out;
    }
    out.push("CLAUDE.md".to_string());
    // Nested files nearest a changed path first: longest matching directory.
    let depth = |hit: &String| {
        let dir = hit.strip_suffix("CLAUDE.md").unwrap_or(hit);
        let near = call.changed.iter().any(|c| c.starts_with(dir));
        if near { dir.len() } else { 0 }
    };
    let mut nested: Vec<&String> = hits
        .iter()
        .filter(|h| is_claude_md(h) && h.as_str() != "CLAUDE.md")
        .collect();
    nested.sort_by_key(|h| std::cmp::Reverse(depth(h)));
    nested.dedup();
    out.extend(nested.into_iter().take(MAX_NESTED_CLAUDE_MD).cloned());
    out
}

/// Read every path concurrently, each under the read timeout.
async fn read_all(fetcher: &dyn DocFetcher, paths: &[String], call: &DocsCall<'_>) -> Vec<DocRead> {
    let (sha, fork) = (call.head.sha.as_str(), call.head.fork);
    let reads = paths.iter().map(|path| async move {
        let limit = Duration::from_secs(DOC_READ_TIMEOUT_SECS);
        let read = match timeout(limit, fetcher.fetch(path, sha)).await {
            Err(_) => Read::Unavailable(format!("timed out after {DOC_READ_TIMEOUT_SECS} s")),
            Ok(Ok(Some(text))) => Read::Text(text),
            // #9193 amendment 6: a fork's 404 does not prove the path is absent.
            Ok(Ok(None)) if fork => Read::Unavailable(format!(
                "not found at {}; the head is in a fork, so its commit may not be readable \
                 from this repository",
                &sha[..7]
            )),
            Ok(Ok(None)) => Read::Absent(format!("not found at {}", &sha[..7])),
            Ok(Err(e)) => Read::Unavailable(e.to_string()),
        };
        DocRead {
            path: path.clone(),
            read,
            self_edited: call.changed.iter().any(|c| c == path),
        }
    });
    join_all(reads).await
}

/// Candidate paths from trusty-search, and why discovery failed (#9193 criterion 7).
///
/// Why: search adds docs the PR body does not name; it supplies paths only,
/// since the index is not the head, and its failure never blocks the review.
/// What: one query (PR title plus changed-file stems) for
/// `MAX_DOC_DISCOVERY_HITS` hits under the read timeout; each hit's path is
/// made repository-relative (an absolute one through the index root from
/// `list_indexes`). Any error or timeout, of the search or of that root
/// lookup, or an index missing from the list, returns no hits and a detail.
/// Test: `search_down_is_unavailable_and_explicit_paths_still_read`,
/// `search_hit_adds_candidate_and_text_comes_from_head`,
/// `search_hit_absolute_path_is_made_repo_relative`,
/// `index_root_lookup_failure_marks_discovery_unavailable`.
async fn discover(call: &DocsCall<'_>) -> (Vec<String>, Option<String>) {
    let limit = Duration::from_secs(DOC_READ_TIMEOUT_SECS);
    let stems: Vec<&str> = call
        .changed
        .iter()
        .filter_map(|p| p.rsplit('/').next()?.split('.').next())
        .take(16)
        .collect();
    let query = format!("{} {}", call.pr_meta.title, stems.join(" "));
    let search = call
        .search
        .search(call.index, &query, Some(MAX_DOC_DISCOVERY_HITS));
    let hits = match timeout(limit, search).await {
        Err(_) => {
            return (
                Vec::new(),
                Some(format!(
                    "trusty-search timed out after {DOC_READ_TIMEOUT_SECS} s"
                )),
            );
        }
        Ok(Err(e)) => return (Vec::new(), Some(format!("trusty-search failed: {e}"))),
        Ok(Ok(hits)) => hits,
    };
    // #9193 amendment 7: without the root every absolute hit would be dropped
    // silently, so a failed lookup is a discovery failure, not "no hits".
    let root = if hits.iter().any(|h| h.file.starts_with('/')) {
        let fail = |detail: String| (Vec::new(), Some(detail));
        match timeout(limit, call.search.list_indexes()).await {
            Err(_) => return fail("index root lookup timed out".to_string()),
            Ok(Err(e)) => return fail(format!("index root lookup failed: {e}")),
            Ok(Ok(indexes)) => match indexes.into_iter().find(|i| i.id == call.index) {
                Some(index) => index.root_path,
                None => return fail(format!("index {} not in list_indexes", call.index)),
            },
        }
    } else {
        None
    };
    let mut paths: Vec<String> = Vec::new();
    for path in hits
        .iter()
        .filter_map(|h| repo_relative(&h.file, root.as_deref()))
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    (paths, None)
}

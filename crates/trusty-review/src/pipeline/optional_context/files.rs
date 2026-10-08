//! The PR's changed files, read whole at the head SHA (#9195).
//!
//! Why: a reviewer that sees only the diff cannot see the code around a
//! change. Epic #9191 phase B4 shows the changed files whole, opt-in, read at
//! the commit under review, within a byte budget, and names every file it
//! leaves out.
//! What: [`FilesCall`] gathers the changed paths and what the runner knows;
//! [`apply_files`] reads them through the `DocFetcher` seam B3 added, picks
//! what fits ([`super::files_select`]), and leaves the rendered section in
//! `AppliedContext::files` and one `changed_files` row in the ledger. Every
//! failure omits a file or the whole source and is recorded; none stops the
//! review. The text is prompt-only: never citable, never in the verifier's
//! prompt (ruling Q2).
//! Test: `files_tests.rs`, `runner_changed_files_tests.rs`.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures_util::stream::{self, StreamExt};
use tokio::time::{error::Elapsed, timeout};

use crate::{
    config::{
        MapReduceConfig, ReviewPath,
        constants::{
            CHANGED_FILE_FETCH_CONCURRENCY, DEFAULT_CHANGED_FILES_BUDGET, DOC_READ_TIMEOUT_SECS,
            MAX_CHANGED_FILE_FETCHES, MAX_CHANGED_FILES_BUDGET,
        },
    },
    integrations::context::contents_at_ref::{DocFetchError, DocFetcher, validate_repo_path},
    models::SourceState,
    pipeline::{
        citation_check::normalize_path,
        diff::DiffSource,
        diff_analyzer::{
            models::{FileDisposition, FilteredDiff},
            parse_diff_files,
        },
        mapreduce::{map::sends_prompt, split_into_units},
        reply_shape::mask_credential_shapes,
    },
};

use super::{
    OptionalContextRequest, ReviewOptions,
    assemble::AppliedContext,
    docs::{fetcher_for, fork_not_found},
    files_render::{empty_row, render, row},
    files_select::{
        Candidate, NotShown, Reason, Shown, classify, is_sensitive, select, split_fetch_cap,
    },
    ledger::ContextLedger,
    seams::PrHead,
};

/// Everything [`apply_files`] reads from the runner (#9195).
///
/// Why: the runner makes one call, as for docs (B3 amendment 13).
/// What: the request, the doc-read seam, the diff source, the changed paths
/// as [`Candidate`]s in diff order, and the head.
/// Test: `runner_changed_files_tests.rs`.
pub(crate) struct FilesCall<'a> {
    request: &'a OptionalContextRequest,
    seam: Option<Arc<dyn DocFetcher>>,
    source: &'a DiffSource,
    candidates: Vec<Candidate>,
    head: PrHead,
}

impl<'a> FilesCall<'a> {
    /// What the runner knows before the head is attached.
    ///
    /// Why: ruling B: a file no prompt carries gets no text, so on the
    /// map-reduce path the units decide which files are carried.
    /// What: with `changed_files` off, no candidate (nothing is split or
    /// parsed). Otherwise one [`Candidate`] per changed path: the kept files
    /// in diff order (a rename by its new name), then the noise filter's
    /// dropped files, each classed, sized by its diff lines, marked removed
    /// when the PR deletes it, and carried unless the map-reduce split gives
    /// it no unit that sends a prompt.
    /// Test: `a_unit_without_a_prompt_gets_no_text_and_is_not_used`,
    /// `deleted_files_are_named_and_never_fetched`, `rename_fetches_the_new_path_not_the_old`.
    pub(crate) fn new(
        options: &'a ReviewOptions,
        source: &'a DiffSource,
        filtered: &FilteredDiff,
        raw_diff: &str,
        path: (ReviewPath, &MapReduceConfig),
    ) -> Self {
        let candidates = if options.request.changed_files {
            candidates(filtered, raw_diff, path)
        } else {
            Vec::new()
        };
        Self {
            request: &options.request,
            seam: options.doc_fetcher.clone(),
            source,
            candidates,
            head: PrHead::default(),
        }
    }

    /// This call reading at `head`.
    pub(crate) fn at(mut self, head: &PrHead) -> Self {
        self.head = head.clone();
        self
    }
}

/// The changed paths, in diff order, before any read.
fn candidates(
    filtered: &FilteredDiff,
    raw_diff: &str,
    (review_path, mr): (ReviewPath, &MapReduceConfig),
) -> Vec<Candidate> {
    // `(removed, diff lines)` per path, dropped files included.
    let parsed: HashMap<String, (bool, usize)> = parse_diff_files(raw_diff)
        .into_iter()
        .map(|(path, status, patch)| {
            let lines = patch
                .lines()
                .filter(|l| {
                    (l.starts_with('+') || l.starts_with('-'))
                        && !l.starts_with("+++")
                        && !l.starts_with("---")
                })
                .count();
            (normalize_path(&path), (status == "removed", lines))
        })
        .collect();
    // #9195 ruling B: on the map-reduce path a file is carried only by a unit
    // that sends a prompt.
    let carried: Option<HashSet<String>> = (review_path == ReviewPath::MapReduce).then(|| {
        split_into_units(filtered, mr)
            .iter()
            .filter(|u| sends_prompt(u))
            .map(|u| normalize_path(&u.file))
            .collect()
    });
    let kept = filtered.files.iter().map(|f| {
        let generated = f.disposition == FileDisposition::SummaryOnly;
        (
            normalize_path(&f.filename),
            f.status == "removed",
            generated,
        )
    });
    let dropped = filtered
        .dropped_files
        .iter()
        .map(|d| (normalize_path(&d.path), false, true));
    let mut seen = HashSet::new();
    kept.chain(dropped)
        .filter(|(path, _, _)| seen.insert(path.clone()))
        .map(|(path, removed, generated)| {
            let (parsed_removed, diff_lines) = parsed.get(&path).copied().unwrap_or_default();
            Candidate {
                class: classify(&path, generated),
                diff_lines,
                removed: removed || parsed_removed,
                carried: carried.as_ref().is_none_or(|c| c.contains(&path)),
                path,
            }
        })
        .collect()
}

/// Read the changed files at the head and leave them in `applied` (#9195).
///
/// Why: AC1-AC4: changed files are read at the head SHA within the budget,
/// drop in the AC2 order, are each named when left out, and no failure stops
/// the review.
/// What: with `changed_files` off, does nothing (no read, no row). A budget
/// of 0 records the row `absent` and reads nothing; a budget over
/// `MAX_CHANGED_FILES_BUDGET` is clamped and the clamp named. A local diff or
/// a head SHA that is not full hex records the row `unavailable` and reads
/// nothing. Otherwise a deleted, deny-listed (ruling A), uncarried (ruling B)
/// or unreadable path is named without a read; the rest, up to
/// `MAX_CHANGED_FILE_FETCHES` (ruling C), are read at most
/// `CHANGED_FILE_FETCH_CONCURRENCY` at a time, each under the read timeout.
/// Read text with a NUL byte is left out as binary; the rest is masked
/// (`mask_credential_shapes`), selected within the budget, and rendered.
/// Test: `budget_zero_makes_no_fetch_and_no_section`, `flag_off_makes_no_fetch_and_no_row`,
/// `local_diff_reports_unavailable_no_head_sha`, `malformed_sha_never_fetches`,
/// `every_omitted_file_is_a_ledger_item_and_a_prompt_line`,
/// `a_budget_above_the_clamp_is_clamped_and_reported`,
/// `fetch_concurrency_is_at_most_eight`, `a_hung_read_times_out_and_is_named`.
pub(crate) async fn apply_files(
    applied: &mut AppliedContext,
    call: FilesCall<'_>,
    ledger: &mut ContextLedger,
) {
    if !call.request.changed_files {
        return; // #9195 amendment 10: a budget without the flag is inert.
    }
    let asked = call
        .request
        .changed_files_budget
        .unwrap_or(DEFAULT_CHANGED_FILES_BUDGET);
    let budget = asked.min(MAX_CHANGED_FILES_BUDGET);
    if budget == 0 {
        let detail = "budget is 0: the review runs on the diff only";
        ledger.push(empty_row(SourceState::Absent, detail));
        return;
    }
    let seam = call.seam.as_ref();
    let fetcher = match fetcher_for(call.source, &call.head, seam, "changed files") {
        Ok(fetcher) => fetcher,
        Err(detail) => {
            ledger.push(empty_row(SourceState::Unavailable, &detail));
            return;
        }
    };
    let (mut not_shown, readable) = screen(call.candidates);
    let (fetch, over_cap) = split_fetch_cap(readable, MAX_CHANGED_FILE_FETCHES);
    not_shown.extend(over_cap.iter().map(|c| {
        let detail = format!("past the first {MAX_CHANGED_FILE_FETCHES} reads");
        NotShown::new(&c.path, Reason::OverFetchCap, SourceState::Omitted, &detail)
    }));
    let mut shown = Vec::new();
    for (candidate, read) in read_all(fetcher.as_ref(), fetch, &call.head.sha).await {
        match outcome(candidate, read, &call.head) {
            Ok(file) => shown.push(file),
            Err(left_out) => not_shown.push(left_out),
        }
    }
    let kept = select(shown, &mut not_shown, budget);
    let sha7 = call.head.sha.get(..7).unwrap_or_default();
    let mut detail = if kept.is_empty() && not_shown.is_empty() {
        "the diff names no changed file".to_string()
    } else {
        format!("read at {sha7}; budget {budget} bytes")
    };
    if asked > budget {
        detail.push_str(&format!(" ({asked} asked for, clamped to {budget})"));
    }
    applied.files = render(&call.head.sha, &kept, &not_shown);
    ledger.push(row(&detail, &kept, &not_shown));
}

/// `(named, readable)`: the candidates named without a read, and the rest.
fn screen(candidates: Vec<Candidate>) -> (Vec<NotShown>, Vec<Candidate>) {
    let mut named = Vec::new();
    let mut readable = Vec::new();
    for c in candidates {
        let (reason, state, detail) = if c.removed {
            (Reason::Deleted, SourceState::Absent, "deleted in this PR")
        } else if is_sensitive(&c.path) {
            let why = "on the sensitive-path deny-list; never read";
            (Reason::SensitivePath, SourceState::Omitted, why)
        } else if !c.carried {
            let why = "no map-reduce chunk prompt reviews this file";
            (Reason::NotReviewed, SourceState::Omitted, why)
        } else if validate_repo_path(&c.path).is_err() {
            // #9195 amendment 4: a fixed detail; the untrusted path is never echoed.
            let why = "path rejected: not a plain repository path of at most 200 characters";
            (Reason::ReadFailed, SourceState::Unavailable, why)
        } else {
            readable.push(c);
            continue;
        };
        named.push(NotShown::new(&c.path, reason, state, detail));
    }
    (named, readable)
}

/// One read's result, or the timeout that cut it off.
type ReadResult = Result<Result<Option<String>, DocFetchError>, Elapsed>;

/// Read every candidate at `sha`, in order, a bounded number at a time
/// (amendment 7), each under the read timeout.
async fn read_all(
    fetcher: &dyn DocFetcher,
    fetch: Vec<Candidate>,
    sha: &str,
) -> Vec<(Candidate, ReadResult)> {
    let limit = Duration::from_secs(DOC_READ_TIMEOUT_SECS);
    stream::iter(fetch)
        .map(|c| async move {
            let read = timeout(limit, fetcher.fetch(&c.path, sha)).await;
            (c, read)
        })
        .buffered(CHANGED_FILE_FETCH_CONCURRENCY)
        .collect()
        .await
}

/// The shown file a read gives, or why it is left out (amendments 2 and 9).
///
/// Why: the prompt carries a fixed reason word; only the ledger carries the
/// error text, capped and redacted by `ContextLedger::finish`.
/// What: a timeout, a directory, a 5xx, a transport error or a fork head's
/// 404 is `read failed`, `unavailable`; a 404 on a non-fork head is `read
/// failed`, `absent`; text that is not UTF-8, has no inline content (over
/// 1 MB) or holds a NUL byte is omitted as `not UTF-8`, `too large` or
/// `binary`. Text is masked before it is shown (ruling A).
/// Test: `each_read_failure_has_a_fixed_prompt_reason`,
/// `a_404_on_a_non_fork_file_is_absent_and_a_5xx_is_unavailable`,
/// `fork_head_404_is_unavailable_not_absent`,
/// `a_secret_on_an_unchanged_line_never_reaches_the_prompt`.
fn outcome(c: Candidate, read: ReadResult, head: &PrHead) -> Result<Shown, NotShown> {
    let left_out = |reason: Reason, state: SourceState, detail: &str| {
        Err(NotShown::new(&c.path, reason, state, detail))
    };
    let failed = |detail: &str| left_out(Reason::ReadFailed, SourceState::Unavailable, detail);
    match read {
        Err(_) => failed(&format!("timed out after {DOC_READ_TIMEOUT_SECS} s")),
        Ok(Ok(Some(text))) if text.contains('\0') => {
            let mut entry = NotShown::new(
                &c.path,
                Reason::Binary,
                SourceState::Omitted,
                "the text holds a NUL byte",
            );
            entry.chars_omitted = text.chars().count();
            Err(entry)
        }
        Ok(Ok(Some(text))) => Ok(Shown {
            text: mask_credential_shapes(&text), // #9195 ruling A
            path: c.path,
            class: c.class,
        }),
        Ok(Ok(None)) if head.fork => failed(&fork_not_found(&head.sha)),
        Ok(Ok(None)) => {
            let detail = format!("not found at {}", head.sha.get(..7).unwrap_or_default());
            left_out(Reason::ReadFailed, SourceState::Absent, &detail)
        }
        Ok(Err(e @ DocFetchError::Undecodable(_))) => {
            left_out(Reason::NotUtf8, SourceState::Omitted, &e.to_string())
        }
        Ok(Err(e @ DocFetchError::NoInlineContent(_))) => {
            left_out(Reason::TooLarge, SourceState::Omitted, &e.to_string())
        }
        Ok(Err(e)) => failed(&e.to_string()),
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

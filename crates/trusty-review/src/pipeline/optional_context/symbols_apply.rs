//! Each changed symbol's callers, callees and tests, from the call graph (#9196).
//!
//! Why: epic #9191 phase B5. A reviewer that sees a changed function but not
//! who calls it cannot judge the change's reach; the symbol-derived call
//! graph answers that, opt-in, and an outage must never stop the review (AC3).
//! What: [`SymbolsCall`] gathers the changed symbols the diff names and what
//! the runner knows; [`apply_symbols`] reads one call-chain report per
//! symbol, at most `SYMBOL_CALL_CONCURRENCY` at a time under a per-call
//! timeout, a phase deadline and a circuit breaker, and leaves the rendered
//! section in `AppliedContext::symbols` and one `symbol_context` row. Every
//! failure omits a symbol or the whole source and is recorded. The text is
//! prompt-only: never citable, never in the verifier's prompt.
//! Test: `symbols_apply_tests.rs`, `runner_symbol_context_tests.rs`.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures_util::future::join_all;
use tokio::time::{Instant, timeout};

use crate::{
    config::{
        MapReduceConfig, ReviewConfig, ReviewPath,
        constants::{
            MAX_SYMBOL_CONTEXT_SYMBOLS, SYMBOL_CALL_CONCURRENCY, SYMBOL_CALL_TIMEOUT_SECS,
            SYMBOL_PHASE_DEADLINE_SECS,
        },
    },
    integrations::{
        context::contents_at_ref::validate_repo_path,
        search_client::{SearchClient, SearchClientError},
    },
    models::SourceState,
    pipeline::{
        citation_check::normalize_path,
        context_gate::GateFacts,
        diff_analyzer::{
            models::{FileDisposition, FilteredDiff},
            parse_diff_files,
        },
        runner::ReviewDeps,
    },
};

use super::{
    ReviewOptions,
    assemble::AppliedContext,
    callgraph::{ParseError, SymbolEdges, parse_report},
    files::carried_paths,
    files_select::{Class, cap_path, classify, is_sensitive},
    ledger::ContextLedger,
    symbols::{ChangedSymbol, changed_symbols, priority},
    symbols_render::{Left, Reason, block, empty_row, render, row},
};

/// The label a symbol in a rejected path carries; the path is never echoed.
const REJECTED: &str = "(a symbol in a rejected path)";

/// Everything [`apply_symbols`] reads from the runner (#9196).
///
/// Why: the runner makes one call, as for docs and changed files.
/// What: whether the flag is on, the search client and index, the gate's
/// search fact, and the changed symbols in priority order, with the ones no
/// prompt carries already left out.
/// Test: `runner_symbol_context_tests.rs`.
pub(crate) struct SymbolsCall<'a> {
    on: bool,
    search: Arc<dyn SearchClient>,
    index: &'a str,
    gate: Option<String>,
    symbols: Vec<ChangedSymbol>,
    left: Vec<Left>,
    unresolved: usize,
}

impl<'a> SymbolsCall<'a> {
    /// What the runner knows for the symbol phase.
    ///
    /// Why: plan §4: only symbols a reviewer prompt carries are read, and
    /// with the flag off nothing is parsed.
    /// What: with `symbol_context` off, nothing. Otherwise the symbols of
    /// every kept, non-generated file the PR does not delete, that is not a
    /// test or a sensitive path; a file whose path fails
    /// `validate_repo_path` is one `read failed` entry with a fixed label,
    /// and a symbol no map-reduce prompt carries is `not reviewed`.
    /// Test: `a_unit_without_a_prompt_gets_no_symbols_and_is_not_used`,
    /// `a_hostile_path_is_not_echoed_or_queried`, `test_files_and_deleted_files_get_no_query`.
    pub(crate) fn new(
        options: &ReviewOptions,
        deps: &ReviewDeps,
        config: &'a ReviewConfig,
        facts: &GateFacts,
        filtered: &FilteredDiff,
        raw_diff: &str,
        path: (ReviewPath, &MapReduceConfig),
    ) -> Self {
        let on = options.request.symbol_context;
        let (symbols, left, unresolved) = if on {
            candidates(filtered, raw_diff, path)
        } else {
            (Vec::new(), Vec::new(), 0)
        };
        Self {
            on,
            search: Arc::clone(&deps.search),
            index: &config.search_index,
            gate: facts.search.clone(),
            symbols,
            left,
            unresolved,
        }
    }
}

/// `(symbols in priority order, left out, hunks with no symbol)`.
fn candidates(
    filtered: &FilteredDiff,
    raw_diff: &str,
    path: (ReviewPath, &MapReduceConfig),
) -> (Vec<ChangedSymbol>, Vec<Left>, usize) {
    let kept: HashSet<String> = filtered
        .files
        .iter()
        .filter(|f| f.disposition != FileDisposition::SummaryOnly && f.status != "removed")
        .map(|f| normalize_path(&f.filename))
        .collect();
    let carried = carried_paths(filtered, path);
    let (mut symbols, mut left, mut unresolved) = (Vec::new(), Vec::new(), 0);
    for (file, status, patch) in parse_diff_files(raw_diff) {
        let file = normalize_path(&file);
        let skip = status == "removed"
            || !kept.contains(&file)
            || classify(&file, false) == Class::Test
            || is_sensitive(&file);
        if skip {
            continue;
        }
        if validate_repo_path(&file).is_err() {
            // B4 amendment 4: a fixed label and detail; the path is untrusted.
            let detail = "path rejected: not a plain repository path of at most 200 characters";
            left.push(Left::new(
                REJECTED,
                Reason::ReadFailed,
                SourceState::Unavailable,
                detail,
            ));
            continue;
        }
        let found = changed_symbols(&file, &patch);
        unresolved += found.unresolved;
        if carried.as_ref().is_some_and(|c| !c.contains(&file)) {
            let why = "no map-reduce chunk prompt reviews this file";
            left.extend(
                found
                    .symbols
                    .iter()
                    .map(|s| Left::new(&s.id(), Reason::NotReviewed, SourceState::Omitted, why)),
            );
        } else {
            symbols.extend(found.symbols);
        }
    }
    (priority(symbols), left, unresolved)
}

/// Read each changed symbol's call graph and leave it in `applied` (#9196).
///
/// Why: AC1-AC3: callers, callees and tests per changed symbol, capped and
/// reported; a search outage omits the section and the review completes.
/// What: with `symbol_context` off, nothing (no read, no row). No index, a
/// gate that degraded trusty-search, or a symbol graph that is not ready
/// records the row `unavailable` and reads nothing; no changed symbol
/// records it `absent`. Otherwise the first `MAX_SYMBOL_CONTEXT_SYMBOLS`
/// symbols are read as `path::name`, `direction=both`, `max_depth=1`, no
/// source; the rest are `over symbol cap`. A read's outcome is a block, or a
/// symbol left out with its reason; an entry in another file is `wrong
/// file`. The section cap then applies, and the row names the index, which
/// may not be at the PR head (ruling Q4). AC3: when a read says the daemon
/// cannot answer, the whole section is left out of the prompt and every
/// symbol is recorded, none `used`.
/// Test: `no_index_is_unavailable_and_makes_no_call`,
/// `a_degraded_search_gate_skips_the_calls_and_names_the_reason`,
/// `a_graph_stage_not_ready_is_unavailable_and_makes_no_call`,
/// `a_docs_only_diff_makes_no_calls_and_no_section`,
/// `an_entry_in_another_file_is_omitted`, `over_the_symbol_cap_is_named_not_read`,
/// `a_search_outage_omits_the_section_and_the_review_completes`.
pub(crate) async fn apply_symbols(
    applied: &mut AppliedContext,
    call: SymbolsCall<'_>,
    ledger: &mut ContextLedger,
) {
    if !call.on {
        return;
    }
    if call.index.is_empty() {
        let detail = "no trusty-search index covers this checkout";
        ledger.push(empty_row(SourceState::Unavailable, detail));
        return;
    }
    if let Some(reason) = &call.gate {
        let detail = format!("trusty-search is degraded: {reason}");
        ledger.push(empty_row(SourceState::Unavailable, &detail));
        return;
    }
    let mut detail = format!(
        "index {:?}; may not be at the PR head",
        cap_path(call.index)
    );
    if call.unresolved > 0 {
        detail.push_str(&format!("; {} hunks had no symbol", call.unresolved));
    }
    if call.symbols.is_empty() && call.left.is_empty() {
        detail.insert_str(0, "no changed symbols found in the diff; ");
        ledger.push(empty_row(SourceState::Absent, &detail));
        return;
    }
    // Plan arm 11: the phase deadline covers the readiness probe too.
    let deadline = Instant::now() + Duration::from_secs(SYMBOL_PHASE_DEADLINE_SECS);
    let search = call.search.as_ref();
    if !call.symbols.is_empty()
        && let Err(why) = graph_ready(search, call.index).await
    {
        ledger.push(empty_row(SourceState::Unavailable, &why));
        return;
    }
    let mut left = call.left;
    let mut ask = call.symbols;
    let over = ask.split_off(ask.len().min(MAX_SYMBOL_CONTEXT_SYMBOLS));
    left.extend(over.iter().map(|s| {
        let why = format!("past the first {MAX_SYMBOL_CONTEXT_SYMBOLS} symbols");
        Left::new(&s.id(), Reason::OverSymbolCap, SourceState::Omitted, &why)
    }));
    let (reads, failure) = read_all(search, call.index, ask, deadline).await;
    let mut shown = Vec::new();
    for (symbol, read) in reads {
        match read {
            Ok(edges) if edges.entry_file != symbol.path => {
                let why = "the call graph anchored this name in another file";
                left.push(Left::new(
                    &symbol.id(),
                    Reason::WrongFile,
                    SourceState::Omitted,
                    why,
                ));
            }
            Ok(edges) => shown.push(block(&symbol, &edges)),
            Err(entry) => left.push(entry),
        }
    }
    let sensitive: usize = shown.iter().map(|s| s.sensitive).sum();
    if sensitive > 0 {
        detail.push_str(&format!("; {sensitive} edges in sensitive paths dropped"));
    }
    if let Some(failure) = failure {
        // AC3: an outage omits the section; what was read is recorded, unshown.
        detail.push_str(&format!("; section left out: {failure}"));
        left.extend(shown.drain(..).map(|s| {
            let why = "read, then left out: trusty-search failed during the phase";
            Left::new(&s.id, Reason::ReadFailed, SourceState::Unavailable, why)
        }));
        ledger.push(row(&detail, &[], &left));
        return;
    }
    let (sections, shown) = render(call.index, shown, &mut left);
    applied.symbols = sections;
    ledger.push(row(&detail, &shown, &left));
}

/// `Err(why)` unless the index's symbol graph can answer (#9196, Architect
/// risk 2: a not-ready graph must read `unavailable`, never empty-but-used).
///
/// What: `index_status` under the call timeout; an error (a `404 unknown
/// index` included: never a skip here) or a `graph` stage that reports a
/// status other than `ready` is `Err`. A daemon that reports no stage status
/// passes; the entry-file check still guards it.
/// Test: `a_graph_stage_not_ready_is_unavailable_and_makes_no_call`,
/// `an_index_status_failure_is_unavailable_not_a_skip`,
/// `a_hung_readiness_probe_is_unavailable_and_makes_no_call`,
/// `a_status_without_a_graph_stage_is_read`.
async fn graph_ready(search: &dyn SearchClient, index: &str) -> Result<(), String> {
    let limit = Duration::from_secs(SYMBOL_CALL_TIMEOUT_SECS);
    match timeout(limit, search.index_status(index)).await {
        Err(_) => Err(format!(
            "trusty-search index status timed out after {SYMBOL_CALL_TIMEOUT_SECS} s"
        )),
        Ok(Err(e)) => Err(format!("trusty-search index status failed: {e}")),
        Ok(Ok(status)) => {
            let stage = status.stages.graph.status;
            if stage.is_empty() || stage.eq_ignore_ascii_case("ready") {
                Ok(())
            } else {
                Err(format!(
                    "the symbol graph is not ready (graph stage: {stage})"
                ))
            }
        }
    }
}

/// One symbol's read: its edges, or why it is left out.
type Read = Result<SymbolEdges, Left>;

/// Read every symbol, `SYMBOL_CALL_CONCURRENCY` at a time (#9196).
///
/// Why: plan arms 6, 10 and 11: a dead daemon must cost at most one batch
/// of calls, a hung call its timeout, and the phase its deadline.
/// What: batches in priority order. Once a read fails in a way that says the
/// daemon cannot answer (see [`classify_error`]), every later symbol is left
/// out unread as `read failed`, and that failure is returned for the row;
/// once `deadline` passes, every later symbol is left out as `deadline`.
/// Test: `a_dead_daemon_costs_at_most_one_batch_of_calls`,
/// `a_hung_call_times_out_and_is_named`, `the_phase_deadline_omits_unstarted_symbols`.
async fn read_all(
    search: &dyn SearchClient,
    index: &str,
    symbols: Vec<ChangedSymbol>,
    deadline: Instant,
) -> (Vec<(ChangedSymbol, Read)>, Option<String>) {
    let mut failure: Option<String> = None;
    let mut out = Vec::new();
    for batch in symbols.chunks(SYMBOL_CALL_CONCURRENCY) {
        if failure.is_some() || Instant::now() >= deadline {
            let (reason, state, why) = if failure.is_some() {
                let why = "skipped after a trusty-search failure";
                (Reason::ReadFailed, SourceState::Unavailable, why)
            } else {
                let why = "not started before the phase deadline";
                (Reason::Deadline, SourceState::Omitted, why)
            };
            out.extend(
                batch
                    .iter()
                    .map(|s| (s.clone(), Err(Left::new(&s.id(), reason, state, why)))),
            );
            continue;
        }
        let reads = join_all(batch.iter().map(|s| read_one(search, index, s))).await;
        for (symbol, (read, fatal)) in batch.iter().zip(reads) {
            if fatal && failure.is_none() {
                failure = read.as_ref().err().map(|l| l.detail.clone());
            }
            out.push((symbol.clone(), read));
        }
    }
    (out, failure)
}

/// Read one symbol; `true` when the failure says the daemon cannot answer.
async fn read_one(search: &dyn SearchClient, index: &str, symbol: &ChangedSymbol) -> (Read, bool) {
    let id = symbol.id();
    let limit = Duration::from_secs(SYMBOL_CALL_TIMEOUT_SECS);
    let left = |reason, state, why: &str| Err(Left::new(&id, reason, state, why));
    match timeout(limit, search.call_chain(index, &id, "both", 1, false)).await {
        Err(_) => {
            // One slow symbol (a huge graph) is not an outage; the deadline
            // bounds a daemon that hangs on every read.
            let why = format!("timed out after {SYMBOL_CALL_TIMEOUT_SECS} s");
            (
                left(Reason::ReadFailed, SourceState::Unavailable, &why),
                false,
            )
        }
        Ok(Err(e)) => {
            let (reason, state, why, fatal) = classify_error(&e);
            (left(reason, state, &why), fatal)
        }
        Ok(Ok(text)) => match parse_report(&text) {
            Ok(edges) => (Ok(edges), false),
            Err(ParseError::Ambiguous) => {
                let why = "the name matches several definitions; none is picked";
                (left(Reason::Ambiguous, SourceState::Omitted, why), false)
            }
            Err(ParseError::Unparseable) => {
                let why = "unparseable call-chain report";
                (
                    left(Reason::ReadFailed, SourceState::Unavailable, why),
                    false,
                )
            }
        },
    }
}

/// How a failed read is recorded, and whether it stops the phase (AC3).
///
/// Why: plan arms 4-9 and Architect risk 2: an outage or a not-ready daemon
/// is `unavailable` and the review continues; only a symbol the index does
/// not hold is `absent`; nothing here ever skips the review.
/// What: a 404 whose body says `entry point not found` is `not in the
/// index`, `absent`. Every other failure is `read failed`, `unavailable`,
/// with the error text for the ledger only; a transport failure, an
/// unsupported client, any 503 (a cold index, a load in flight, a
/// migration, a KG-disabled index) and any other 404 (`unknown index`) also
/// stop the phase.
/// Test: `a_new_symbol_is_absent_not_unavailable`,
/// `unknown_index_here_is_unavailable_not_a_skip`, `a_not_ready_daemon_is_unavailable`,
/// `a_kg_disabled_index_is_unavailable`, `one_failed_symbol_does_not_abort_the_rest`.
fn classify_error(e: &SearchClientError) -> (Reason, SourceState, String, bool) {
    let failed = |why: String, fatal| (Reason::ReadFailed, SourceState::Unavailable, why, fatal);
    match e {
        SearchClientError::Api { status: 404, body } if body.contains("entry point not found") => {
            let why = "the index has no such entry point".to_string();
            (Reason::NotInIndex, SourceState::Absent, why, false)
        }
        SearchClientError::Api { status: 404, .. } => failed(format!("index not found: {e}"), true),
        SearchClientError::Api { status: 503, .. } => {
            failed(format!("trusty-search is not ready: {e}"), true)
        }
        SearchClientError::Api { .. } | SearchClientError::Parse(_) => failed(e.to_string(), false),
        _ => failed(e.to_string(), true),
    }
}

#[cfg(test)]
#[path = "symbols_apply_tests.rs"]
mod tests;

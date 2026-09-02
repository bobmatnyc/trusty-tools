//! Trace-to-verify: giving a finding an anchor a reader can check (#6166).
//!
//! Why: a verified finding proves its evidence quote exists at `file:line`. It
//! does not say what that line IS. A due-diligence reader who wants to act on
//! "the shrink guard is inert" has to open the repository and find the symbol
//! themselves. The trace pass does that lookup once, mechanically, and records
//! it beside the finding.
//!
//! What: for each candidate finding — every RED plus the first
//! [`TraceLimits::max_amber`] AMBERs — [`assemble_traces`] resolves the citation
//! to a symbol from the file on disk, confirms that symbol against the index's
//! symbol graph, and collects its usage sites inside the finding's own file. A
//! candidate that cannot complete every step records WHY as a `no trace:` line
//! and carries no anchor; there is no partial or guessed record.
//!
//! ## What leg 1 deliberately leaves out
//!
//! Call edges. `GET /indexes/{id}/call_chain` resolves callees and callers by
//! BARE symbol name, so on this workspace 254 of one symbol's 321 callee edges
//! cross a crate boundary, 16 land in non-Rust files, and one bare `get`
//! collects 2391 caller edges. That is tracked as #6167. Until it is fixed the
//! edge slot stays empty and says so, rather than shipping edges a reader would
//! have to disprove one at a time.
//!
//! Test: `trace_tests.rs`.

use std::path::Path;

use serde::Serialize;

use crate::report::metrics::Severity;

use crate::report::index_registry::{derive_index_id, resolve_report_index};

use super::trace_client::{TraceError, TraceSource, TraceUsage};
use super::trace_symbol::resolve_symbol;
use super::verify::VerifiedFinding;

/// What the empty [`FindingTrace::call_edges`] slot means.
pub const CALL_EDGES_DISABLED: &str =
    "disabled: symgraph resolves call edges by bare symbol name (see #6167)";

/// How much of the finding set the trace pass reaches, and how much of each
/// finding it records.
///
/// Why: every field is a bound on live HTTP work inside a report run. REDs are
/// unbounded on purpose — an engagement that produced 40 REDs wants all 40
/// anchored — while AMBERs are capped because they are the long tail (145 on
/// the engagement that drove this, against 2 REDs).
/// What: `max_amber` caps the AMBER candidates; `max_usages` caps usage sites
/// per finding; `snippet_bytes` caps each usage snippet.
/// Test: `trace_tests::candidates_are_every_red_plus_a_bounded_amber_tail`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TraceLimits {
    /// AMBER findings traced, in the order the investigation produced them.
    pub max_amber: usize,
    /// Usage sites recorded per traced finding.
    pub max_usages: usize,
    /// Byte ceiling on one usage snippet.
    pub snippet_bytes: usize,
}

impl Default for TraceLimits {
    fn default() -> Self {
        Self {
            max_amber: 10,
            max_usages: 5,
            snippet_bytes: 400,
        }
    }
}

/// The symbol-graph anchor a traced finding was confirmed against.
///
/// Why: this is the checkable half of the record — a reader can open
/// `file:line` and read the declaration the signature quotes.
/// What: the graph's own spelling of the symbol, where it declared it, and its
/// signature line.
/// Test: `trace_tests::a_matching_entry_becomes_the_anchor`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceAnchor {
    /// The symbol as the graph spells it.
    pub symbol: String,
    /// Repository-relative declaration file.
    pub file: String,
    /// 1-based declaration line.
    pub line: u64,
    /// Declaration signature, empty when the report carried none.
    pub signature: String,
}

/// One candidate finding's trace record.
///
/// Why: a record exists for every candidate, including the ones that failed —
/// a missing record and a failed one are indistinguishable to a reader, and the
/// failures are the informative half (13 of 30 candidates on the engagement
/// that drove this resolved to a different file under an ambiguous name).
/// What: `anchor` and `usages` are populated only when every step completed;
/// `no_trace` carries the reason otherwise, and exactly one of the two is set.
/// `call_edges` is the reserved slot #6167 will fill.
/// Test: `trace_tests::{a_matching_entry_becomes_the_anchor,
/// an_entry_in_another_file_is_an_ambiguous_name}`.
#[derive(Debug, Clone, Serialize)]
pub struct FindingTrace {
    /// The finding's title, its identity within the repository.
    pub title: String,
    /// The finding's cited repository-relative file.
    pub file: String,
    /// The finding's cited line.
    pub line: Option<u64>,
    /// The symbol the citation resolved to locally, `Type::method` for a method.
    pub symbol: Option<String>,
    /// The confirmed symbol-graph anchor, when one was reached.
    pub anchor: Option<TraceAnchor>,
    /// Usage sites inside the finding's own file.
    pub usages: Vec<TraceUsage>,
    /// Why the usage query returned nothing it should have; empty on success.
    pub usages_status: String,
    /// Reserved for #6167; always empty in leg 1.
    ///
    /// Typed as strings rather than a struct nobody constructs — the element
    /// type is #6167's to choose when it has real edges to put here.
    pub call_edges: Vec<String>,
    /// Why [`Self::call_edges`] is empty: [`CALL_EDGES_DISABLED`].
    pub call_edges_status: String,
    /// The fail-closed reason, when this candidate produced no anchor.
    pub no_trace: Option<String>,
}

impl FindingTrace {
    /// A record that got no further than a reason.
    fn refused(f: &VerifiedFinding, symbol: Option<String>, reason: String) -> Self {
        Self {
            title: f.title.clone(),
            file: f.file.clone(),
            line: f.line,
            symbol,
            anchor: None,
            usages: Vec::new(),
            usages_status: String::new(),
            call_edges: Vec::new(),
            call_edges_status: CALL_EDGES_DISABLED.to_string(),
            no_trace: Some(reason),
        }
    }
}

/// Every candidate finding's trace record for one repository.
///
/// Why: the counts are what the coverage section states, and they must come
/// from the records rather than be recomputed at render time.
/// What: `index_id` names what the anchors were resolved against (`None` when
/// none could be derived); `assembled` and `no_trace` partition `traces`.
/// Test: `trace_tests::the_set_counts_match_its_records`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceSet {
    /// The trusty-search index the anchors were confirmed against.
    pub index_id: Option<String>,
    /// One record per candidate finding.
    pub traces: Vec<FindingTrace>,
    /// Candidate findings considered.
    pub candidates: usize,
    /// Records that reached an anchor.
    pub assembled: usize,
    /// Records that fail-closed with a reason.
    pub no_trace: usize,
    /// The bounds this run applied.
    pub limits: TraceLimits,
}

impl TraceSet {
    /// Build the set from its records, deriving the counts once.
    fn from_records(
        index_id: Option<String>,
        traces: Vec<FindingTrace>,
        limits: TraceLimits,
    ) -> Self {
        let no_trace = traces.iter().filter(|t| t.no_trace.is_some()).count();
        Self {
            index_id,
            candidates: traces.len(),
            assembled: traces.len() - no_trace,
            no_trace,
            traces,
            limits,
        }
    }
}

/// The findings worth tracing: every RED, then a bounded AMBER tail.
///
/// Why: an unbounded pass would make two live HTTP reads for each of 147
/// RED/AMBER findings on a single repository. REDs are what an acquirer acts
/// on first, so they are never dropped; the AMBER tail is where the bound goes.
/// What: preserves the investigation's own order within each band.
/// Test: `trace_tests::candidates_are_every_red_plus_a_bounded_amber_tail`.
fn candidates(findings: &[VerifiedFinding], limits: TraceLimits) -> Vec<&VerifiedFinding> {
    let reds = findings.iter().filter(|f| f.severity == Severity::Red);
    let ambers = findings
        .iter()
        .filter(|f| f.severity == Severity::Amber)
        .take(limits.max_amber);
    reds.chain(ambers).collect()
}

/// Assemble one repository's traces.
///
/// Why: the whole pass in one place, so every fail-closed exit is visible
/// beside the success path it replaces.
/// What: probes the daemon once, resolves the index id once (#6677), then per candidate
/// resolves the symbol from disk, confirms the `[ENTRY]` node, checks the entry
/// file against the finding's, and collects in-file usages. An unreachable
/// daemon or an absent index stops further HTTP work and gives every remaining
/// candidate the same reason, because retrying it 29 more times changes
/// nothing.
/// Test: `trace_tests.rs` — one test per fail-closed branch, plus the success path.
pub async fn assemble_traces(
    repo_path: &Path,
    findings: &[VerifiedFinding],
    source: &dyn TraceSource,
    limits: TraceLimits,
) -> TraceSet {
    let candidates = candidates(findings, limits);

    let mut halt: Option<String> = if source.reachable().await {
        None
    } else {
        Some("no trace: trusty-search is not reachable".to_string())
    };
    // #6677: a checkout registered under an id other than its derived one is
    // addressed by the id it IS registered under. A daemon that did not answer
    // has no registry to substitute from, so that branch records what the path
    // derives to and halts on the reason already set.
    let index_id = if halt.is_some() {
        derive_index_id(repo_path)
    } else {
        resolve_report_index(repo_path, &source.registered_indexes().await).into_id()
    };
    if halt.is_none() && index_id.is_none() {
        halt = Some(format!(
            "no trace: no trusty-search index id could be derived for {}",
            repo_path.display()
        ));
    }

    let mut traces = Vec::with_capacity(candidates.len());
    for f in candidates {
        if let Some(reason) = &halt {
            traces.push(FindingTrace::refused(f, None, reason.clone()));
            continue;
        }
        let id = index_id.as_deref().unwrap_or_default();
        traces.push(trace_one(repo_path, id, f, source, limits, &mut halt).await);
    }
    TraceSet::from_records(index_id, traces, limits)
}

/// Trace one candidate, setting `halt` when the failure would repeat for all of
/// them.
async fn trace_one(
    repo_path: &Path,
    index_id: &str,
    f: &VerifiedFinding,
    source: &dyn TraceSource,
    limits: TraceLimits,
    halt: &mut Option<String>,
) -> FindingTrace {
    let Some(sym) = resolve_local_symbol(repo_path, f) else {
        return FindingTrace::refused(
            f,
            None,
            format!(
                "no trace: no item declaration found at {}:{}",
                f.file,
                f.line.unwrap_or(0)
            ),
        );
    };

    let entry = match source.entry_node(index_id, &sym).await {
        Ok(e) => e,
        Err(e @ (TraceError::Unreachable(_) | TraceError::IndexAbsent(_))) => {
            let reason = format!("no trace: {e}");
            *halt = Some(reason.clone());
            return FindingTrace::refused(f, Some(sym), reason);
        }
        Err(e) => return FindingTrace::refused(f, Some(sym), format!("no trace: {e}")),
    };

    // The anchor is only an anchor if it is in the file the finding cited. A
    // bare name the graph resolved elsewhere is the #6167 defect arriving one
    // node earlier, and admitting it would attach another crate's declaration
    // to this finding.
    if entry.file != f.file {
        return FindingTrace::refused(
            f,
            Some(sym.clone()),
            format!(
                "no trace: symbol {sym} resolved to {} — ambiguous name",
                entry.file
            ),
        );
    }

    let (usages, usages_status) = match source
        .usages(
            index_id,
            &sym,
            &f.file,
            limits.max_usages,
            limits.snippet_bytes,
        )
        .await
    {
        Ok(u) => (u, String::new()),
        Err(e) => (Vec::new(), format!("usages unavailable: {e}")),
    };

    FindingTrace {
        title: f.title.clone(),
        file: f.file.clone(),
        line: f.line,
        symbol: Some(sym),
        anchor: Some(TraceAnchor {
            symbol: entry.symbol,
            file: entry.file,
            line: entry.line,
            signature: entry.signature,
        }),
        usages,
        usages_status,
        call_edges: Vec::new(),
        call_edges_status: CALL_EDGES_DISABLED.to_string(),
        no_trace: None,
    }
}

/// Read the finding's file off disk and resolve its cited line to a symbol.
///
/// The file is read rather than taken from the selection because the selection
/// truncates at 24 KiB and a citation past that point would silently resolve to
/// the wrong item — or to nothing.
fn resolve_local_symbol(repo_path: &Path, f: &VerifiedFinding) -> Option<String> {
    let line = f.line?;
    let source = std::fs::read_to_string(repo_path.join(&f.file)).ok()?;
    resolve_symbol(&source, line).map(|r| r.name)
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;

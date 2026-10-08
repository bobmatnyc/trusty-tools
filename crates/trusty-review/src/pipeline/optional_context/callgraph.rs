//! The trusty-search call-chain report, read into edges (#9196).
//!
//! Why: `search.call_chain` answers plain text, an unversioned cross-crate
//! contract (Architect ruling Q10). Format drift must surface as an error the
//! caller records `unavailable`, never as an empty edge set it records `used`.
//! What: [`parse_report`] reads the `[ENTRY]` header, its `Signature:` line,
//! and the `Calls →` and `Called by ←` lists of a `direction=both`,
//! `max_depth=1` report. Why/What doc lines and function bodies are never
//! read. [`is_test_edge`] says which callers are tests (ruling Q11).
//! Test: `callgraph_tests.rs`.

use crate::{config::constants::MAX_SYMBOL_REPORT_BYTES, pipeline::citation_check::normalize_path};

use super::files_select::{Class, classify};

/// One caller or callee: the symbol as the graph names it and its `file:line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edge {
    /// The symbol name.
    pub(crate) symbol: String,
    /// `file:line`, or the daemon's chunk id when it rendered no location.
    pub(crate) location: String,
}

impl Edge {
    /// The file part of `location`.
    pub(crate) fn file(&self) -> &str {
        match self.location.rsplit_once(':') {
            Some((file, line)) if line.chars().all(|c| c.is_ascii_digit()) => file,
            _ => &self.location,
        }
    }
}

/// What one report says about its entry point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SymbolEdges {
    /// The entry's file, normalised.
    pub(crate) entry_file: String,
    /// The entry's 1-based line.
    pub(crate) entry_line: u64,
    /// The entry's signature line; empty when the report carried none.
    pub(crate) signature: String,
    /// What the entry calls.
    pub(crate) callees: Vec<Edge>,
    /// What calls the entry.
    pub(crate) callers: Vec<Edge>,
    /// The report was longer than [`MAX_SYMBOL_REPORT_BYTES`] and was cut.
    pub(crate) cut: bool,
}

/// Why a report gave no edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// `# AMBIGUOUS:`: the daemon picked one of several definitions (ruling Q9).
    Ambiguous,
    /// No `[ENTRY]` header, or no edge list where one must be.
    Unparseable,
}

/// The entry header's marker.
const ENTRY: &str = "[ENTRY]";
/// The callee list's header.
const CALLS: &str = "Calls →";
/// The caller list's header.
const CALLED_BY: &str = "Called by ←";

/// Read a `direction=both` call-chain report (#9196).
///
/// Why: AC1 needs callers and callees per changed symbol; ruling Q9 refuses
/// an ambiguous anchor; plan arm 12: drift is an error, not an empty set.
/// What: reads at most [`MAX_SYMBOL_REPORT_BYTES`] (cut at a char boundary,
/// `cut` set). A `# AMBIGUOUS:` line is [`ParseError::Ambiguous`]. The
/// ``## `sym` [ENTRY]  file:line`` header and both list headers must be
/// present, unless the cut removed a later one; each list holds
/// `  · symbol  location` lines or `  (none discovered)`. Anything else is
/// [`ParseError::Unparseable`].
/// Test: `parses_a_real_report`, `renamed_headers_are_unparseable_not_empty`,
/// `an_ambiguous_report_is_refused`, `an_oversize_report_is_cut_and_marked`.
pub(crate) fn parse_report(report: &str) -> Result<SymbolEdges, ParseError> {
    let mut end = report.len().min(MAX_SYMBOL_REPORT_BYTES);
    while !report.is_char_boundary(end) {
        end -= 1;
    }
    let cut = end < report.len();
    // A cut report ends at its last whole line, so no half edge is read.
    let text = match report[..end].rfind('\n') {
        Some(nl) if cut => &report[..nl],
        _ => &report[..end],
    };
    if text.lines().any(|l| l.starts_with("# AMBIGUOUS:")) {
        return Err(ParseError::Ambiguous);
    }
    let mut lines = text.lines().skip_while(|l| !is_entry_header(l));
    let header = lines.next().ok_or(ParseError::Unparseable)?;
    let (entry_file, entry_line) = header_location(header).ok_or(ParseError::Unparseable)?;
    let mut edges = SymbolEdges {
        entry_file,
        entry_line,
        cut,
        ..SymbolEdges::default()
    };
    let (mut seen_calls, mut seen_callers) = (false, false);
    let mut list: Option<&mut Vec<Edge>> = None;
    for line in lines {
        if line.starts_with("## ") || line.starts_with('─') {
            break;
        }
        if let Some(sig) = line.strip_prefix("Signature:") {
            edges.signature = sig.trim().to_string();
            list = None;
        } else if line.trim_end() == CALLS {
            seen_calls = true;
            list = Some(&mut edges.callees);
        } else if line.trim_end() == CALLED_BY {
            seen_callers = true;
            list = Some(&mut edges.callers);
        } else if let Some(rest) = line.strip_prefix("  · ") {
            let target = list.as_deref_mut().ok_or(ParseError::Unparseable)?;
            target.push(edge(rest).ok_or(ParseError::Unparseable)?);
        } else if line.trim() != "(none discovered)" {
            list = None;
        }
    }
    if !cut && !(seen_calls && seen_callers) {
        return Err(ParseError::Unparseable);
    }
    Ok(edges)
}

/// Whether `line` is the ``## `sym` [ENTRY]  file:line`` header.
fn is_entry_header(line: &str) -> bool {
    line.starts_with("## `") && line.contains(ENTRY)
}

/// `(file, line)` from the entry header.
fn header_location(header: &str) -> Option<(String, u64)> {
    let (file, line) = header.split_once(ENTRY)?.1.trim().rsplit_once(':')?;
    let line = line.trim().parse().ok()?;
    let file = normalize_path(file);
    (!file.is_empty()).then_some((file, line))
}

/// One `symbol  location` list entry.
fn edge(rest: &str) -> Option<Edge> {
    let (symbol, location) = rest.trim_end().rsplit_once("  ")?;
    let (symbol, location) = (symbol.trim(), location.trim());
    (!symbol.is_empty() && !location.is_empty()).then(|| Edge {
        symbol: symbol.to_string(),
        location: location.to_string(),
    })
}

/// Whether a caller is a test (ruling Q11).
///
/// Why: AC1 asks for tests; the call graph only carries what it indexes, so
/// an inline `#[cfg(test)]` module in the same file is not told apart.
/// What: the caller's file classifies as a test path, or its bare symbol
/// name starts with `test_`.
/// Test: `test_callers_are_told_apart_by_path_or_name`.
pub(crate) fn is_test_edge(edge: &Edge) -> bool {
    let bare = edge.symbol.rsplit("::").next().unwrap_or(&edge.symbol);
    bare.starts_with("test_") || classify(&normalize_path(edge.file()), false) == Class::Test
}

#[cfg(test)]
#[path = "callgraph_tests.rs"]
mod tests;

//! `get_call_chain` — annotated call tree for a given entry-point function
//! (issue #76).
//!
//! Why: `search_code` returns chunks that match a query but no structural
//! context. When an LLM is editing `fn search()` it benefits from seeing what
//! `search()` calls (depth-1 callees with full source), what calls
//! `search()` (depth-1 callers with signatures), and the `Why:` / `What:`
//! doc-comment intent of each — research shows depth-1 call chains with
//! doc annotations measurably improve multi-function edit quality.
//! What: a pure renderer that, given a [`SymbolGraph`] and a snapshot of the
//! `RawChunk` corpus, produces a plain-text call-tree report for a single
//! entry point. LLMs read prose trees better than JSON, so the output is a
//! string. Resolves entry points by exact symbol name, fuzzy substring, or
//! `file:line` lookup, picking the most-connected candidate when several
//! symbols share a name. No I/O — the HTTP handler does the lock acquisition
//! and hands the snapshot in.
//! Test: `tests` module covers doc extraction (single + multi-line),
//! signature extraction (Rust + Python), fuzzy resolution, `file:line`
//! resolution, depth limits, and direction filtering.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;

use crate::core::chunker::RawChunk;
use crate::core::symbol_graph::SymbolGraph;

/// Direction of traversal for `get_call_chain`.
///
/// Why: callers sometimes only want to see *what an entry point depends on*
/// (`Outgoing`) or *who depends on it* (`Callers`). Default `Both` matches
/// the issue spec.
/// What: simple enum mapped from the optional `direction` JSON arg.
/// Test: covered by `tests::direction_outgoing_omits_callers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallChainDirection {
    /// Walk both outgoing (callees) and incoming (callers) edges.
    Both,
    /// Walk only outgoing edges (what the entry point calls).
    Outgoing,
    /// Walk only incoming edges (who calls the entry point).
    Callers,
}

impl CallChainDirection {
    /// Parse a string from the MCP `direction` argument.
    ///
    /// Why: the tool spec accepts `"both"`, `"outgoing"`, or `"callers"`;
    /// anything else is an invalid-params error at the dispatch layer, so
    /// here we return `None` and let the caller produce the error.
    /// What: case-insensitive match against the three known variants.
    /// Test: `tests::direction_parses_known_variants`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "both" => Some(Self::Both),
            "outgoing" | "callees" => Some(Self::Outgoing),
            "callers" | "incoming" => Some(Self::Callers),
            _ => None,
        }
    }
}

/// Hard upper bound on `max_depth`, per the issue spec.
const MAX_DEPTH_CAP: u32 = 4;
/// Default `max_depth` when the caller omits it.
const DEFAULT_DEPTH: u32 = 2;

/// Request shape decoded from the MCP `arguments` object.
///
/// Why: keeps the dispatcher in `mcp/tools.rs` free of validation noise;
/// `serde` performs the basic type-checking and defaults, and
/// [`CallChainRequest::validate`] applies the semantic clamps.
/// What: mirrors the issue's parameter list; all fields except
/// `index_id` / `entry_point` are optional.
/// Test: `tests::request_validate_clamps_depth_and_normalises_direction`.
#[derive(Debug, Deserialize)]
pub struct CallChainRequest {
    pub index_id: String,
    pub entry_point: String,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub max_depth: Option<u32>,
    #[serde(default)]
    pub include_source: Option<bool>,
}

/// Post-validation request, ready to drive the renderer.
#[derive(Debug, Clone)]
pub struct ValidatedCallChainRequest {
    pub index_id: String,
    pub entry_point: String,
    pub direction: CallChainDirection,
    pub max_depth: u32,
    pub include_source: bool,
}

impl CallChainRequest {
    /// Validate and normalise the raw request.
    ///
    /// Why: clamps `max_depth` to `[1, MAX_DEPTH_CAP]`, defaults
    /// `direction`/`include_source`, and rejects unknown direction strings
    /// with a static error message so the MCP layer can map it to
    /// `INVALID_PARAMS` without re-deriving the rule.
    /// What: returns a [`ValidatedCallChainRequest`] on success.
    /// Test: `tests::request_validate_*`.
    pub fn validate(self) -> Result<ValidatedCallChainRequest, &'static str> {
        if self.index_id.trim().is_empty() {
            return Err("'index_id' must be a non-empty string");
        }
        if self.entry_point.trim().is_empty() {
            return Err("'entry_point' must be a non-empty string");
        }
        let direction = match self.direction.as_deref() {
            None => CallChainDirection::Both,
            Some(s) => CallChainDirection::parse(s)
                .ok_or("'direction' must be one of: both, outgoing, callers")?,
        };
        let max_depth = self
            .max_depth
            .unwrap_or(DEFAULT_DEPTH)
            .clamp(1, MAX_DEPTH_CAP);
        let include_source = self.include_source.unwrap_or(true);
        Ok(ValidatedCallChainRequest {
            index_id: self.index_id,
            entry_point: self.entry_point,
            direction,
            max_depth,
            include_source,
        })
    }
}

/// Extract `Why:` and `What:` sections from leading `///` (or `#`) doc comments.
///
/// Why: `RawChunk.content` carries the full function body including the
/// `///`-prefixed doc comments produced by the Why/What/Test convention. We
/// pull the `Why:` and `What:` paragraphs out as plain prose so the call-tree
/// report can annotate every function with its design intent.
/// What: scans the *leading* comment block (lines starting with `///`, `//!`,
/// or `#` for Python). For each `<Section>:` prefix among `Why`/`What`/`Test`
/// the section is captured; continuation lines (further `///` lines without a
/// new section header) are appended. Returns `(why, what)` — both may be
/// `None`.
/// Test: `tests::extract_doc_sections_*`.
pub fn extract_doc_sections(source: &str) -> (Option<String>, Option<String>) {
    let mut why: Option<String> = None;
    let mut what: Option<String> = None;
    // Track which section the current continuation line belongs to.
    enum Section {
        None,
        Why,
        What,
        Other,
    }
    let mut cur = Section::None;

    for line in source.lines() {
        let trimmed = line.trim_start();
        // Pull off the comment prefix; bail out of the doc-comment block as
        // soon as we see a non-comment line.
        let body = if let Some(rest) = trimmed.strip_prefix("///") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("//!") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix('#') {
            // Python: `# Why: ...` doc-comment style.
            rest
        } else if trimmed.is_empty() {
            // Blank line inside the doc block keeps continuation alive.
            continue;
        } else {
            // First non-comment line ends the doc block.
            break;
        };
        let body = body.trim();

        // New section header takes precedence over continuation.
        if let Some(rest) = section_value(body, "Why") {
            cur = Section::Why;
            push_into(&mut why, rest);
        } else if let Some(rest) = section_value(body, "What") {
            cur = Section::What;
            push_into(&mut what, rest);
        } else if let Some(_rest) = section_value(body, "Test") {
            // We don't surface Test: but we still want to stop continuation
            // from leaking Test-section prose into Why/What.
            cur = Section::Other;
        } else {
            // Continuation line — append to whatever section we're in.
            match cur {
                Section::Why => push_continuation(&mut why, body),
                Section::What => push_continuation(&mut what, body),
                Section::None | Section::Other => {}
            }
        }
    }

    (
        why.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        what.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    )
}

/// If `line` starts with `Section:` (case-sensitive), return the trimmed text
/// after the colon. Used by [`extract_doc_sections`].
fn section_value<'a>(line: &'a str, section: &str) -> Option<&'a str> {
    let prefix = format!("{section}:");
    line.strip_prefix(&prefix).map(str::trim_start)
}

fn push_into(slot: &mut Option<String>, value: &str) {
    let v = value.trim().to_string();
    *slot = Some(v);
}

fn push_continuation(slot: &mut Option<String>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(s) = slot.as_mut() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(value.trim());
    }
}

/// Extract a function signature (first non-comment, non-attribute line)
/// from `RawChunk.content`.
///
/// Why: we display a one-liner under each function name so LLMs see the
/// parameter list and return type at a glance, even at deeper depths where
/// we skip the full body.
/// What: walks lines, skipping leading doc comments (`///`, `//!`, `//`,
/// `#`), Rust attributes (`#[…]`), blank lines, and Python decorators
/// (`@…`). Returns the first surviving line trimmed, truncated to 240 chars
/// to keep wide signatures readable.
/// Test: `tests::extract_signature_*`.
pub fn extract_signature(source: &str) -> Option<String> {
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("///")
            || line.starts_with("//!")
            || line.starts_with("//")
            || line.starts_with("#[")
            || line.starts_with("#!")
            || line.starts_with('@')
        {
            continue;
        }
        // Allow Python `#` comments but not `# Why:` etc — fall through and
        // skip pure-comment lines that aren't doc-comment style.
        if line.starts_with('#') && !line.starts_with("#define") {
            continue;
        }
        let truncated: String = line.chars().take(240).collect();
        return Some(truncated);
    }
    None
}

/// Resolve a user-supplied `entry_point` argument to a `(symbol, chunk)`
/// pair. The `entry_point` may be:
/// - an exact symbol name (`authenticate`, `Foo::bar`),
/// - a case-insensitive substring (fuzzy),
/// - a `file:line` reference (`src/auth.rs:42`).
///
/// When multiple candidates match, we prefer the one with the largest total
/// degree in the symbol graph (in + out edges) — the rationale being that
/// the most-connected symbol is almost always what the LLM meant.
///
/// Test: `tests::resolve_entry_point_*`.
pub fn resolve_entry_point<'a>(
    entry_point: &str,
    graph: &SymbolGraph,
    chunks: &'a [RawChunk],
) -> Option<(String, &'a RawChunk)> {
    // `file:line` form takes precedence — exact and unambiguous.
    if let Some((file_part, line_part)) = entry_point.rsplit_once(':') {
        if let Ok(line_no) = line_part.parse::<usize>() {
            if let Some(c) = chunks.iter().find(|c| {
                c.file.ends_with(file_part) && c.start_line <= line_no && line_no <= c.end_line
            }) {
                let symbol = c
                    .function_name
                    .clone()
                    .unwrap_or_else(|| format!("{}:{}", c.file, c.start_line));
                return Some((symbol, c));
            }
        }
    }

    // Otherwise: collect symbol candidates from the graph and pick the most
    // connected one. Fall back to chunk-only scanning if the graph has no
    // matches (BM25-only indexer with no symbol graph).
    let degrees = graph.degrees();
    let needle = entry_point.to_ascii_lowercase();

    // First pass: exact symbol match.
    let mut candidates: Vec<&String> = degrees
        .keys()
        .filter(|s| s.as_str() == entry_point)
        .collect();
    // Second pass: case-insensitive substring fuzzy.
    if candidates.is_empty() {
        candidates = degrees
            .keys()
            .filter(|s| s.to_ascii_lowercase().contains(&needle))
            .collect();
    }
    // Sort by descending degree, then by ascending name for stability.
    candidates.sort_by(|a, b| {
        let da = degrees.get(*a).copied().unwrap_or(0);
        let db = degrees.get(*b).copied().unwrap_or(0);
        db.cmp(&da).then_with(|| a.cmp(b))
    });

    for sym in candidates {
        if let Some(c) = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some(sym.as_str()))
        {
            return Some((sym.clone(), c));
        }
    }

    // Fall back: scan chunks for a function_name match (helps when the graph
    // wasn't built but the corpus still names functions).
    let by_name = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some(entry_point));
    if let Some(c) = by_name {
        return Some((entry_point.to_string(), c));
    }
    let fuzzy = chunks.iter().find(|c| {
        c.function_name
            .as_deref()
            .is_some_and(|n| n.to_ascii_lowercase().contains(&needle))
    });
    if let Some(c) = fuzzy {
        let sym = c
            .function_name
            .clone()
            .unwrap_or_else(|| entry_point.to_string());
        return Some((sym, c));
    }

    None
}

/// Top-level entry: render the annotated call-tree report.
///
/// Why: one synchronous function the HTTP handler / MCP dispatcher can call
/// after grabbing the indexer snapshot. Returns the final `String` — no I/O,
/// no locks held.
/// What: resolves the entry point, walks 1-hop callees and callers, then —
/// when `include_source` and depth > 1 — recursively walks deeper callees
/// emitting compact signature-only entries.
/// Test: `tests::render_includes_entry_signature_and_neighbors`.
pub fn render_call_chain(
    req: &ValidatedCallChainRequest,
    graph: &SymbolGraph,
    chunks: &[RawChunk],
) -> Result<String, String> {
    let (entry_symbol, entry_chunk) = resolve_entry_point(&req.entry_point, graph, chunks)
        .ok_or_else(|| format!("entry point not found: {}", req.entry_point))?;

    // Build an index from symbol -> chunk so we don't re-scan chunks for
    // every neighbour.
    let by_symbol: HashMap<&str, &RawChunk> = chunks
        .iter()
        .filter_map(|c| c.function_name.as_deref().map(|n| (n, c)))
        .collect();

    let mut out = String::new();
    let direction_label = match req.direction {
        CallChainDirection::Both => "both",
        CallChainDirection::Outgoing => "outgoing",
        CallChainDirection::Callers => "callers",
    };
    out.push_str(&format!("# Call chain: {entry_symbol}\n"));
    out.push_str(&format!(
        "# Index: {}  Direction: {}  Depth: {}\n",
        req.index_id, direction_label, req.max_depth
    ));
    out.push_str(&format!("# Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("═══════════════════════════════════════\n\n");

    render_entry_block(&mut out, &entry_symbol, entry_chunk, graph, req);
    out.push_str("\n───────────────────────────────────────\n\n");

    // Depth-1 callees: emit full source (when include_source) + their own
    // depth-2 callee signatures.
    if matches!(
        req.direction,
        CallChainDirection::Both | CallChainDirection::Outgoing
    ) {
        for (sym, _chunk_id) in graph.callees_of(&entry_symbol, 1) {
            let Some(chunk) = by_symbol.get(sym.as_str()) else {
                continue;
            };
            render_neighbor_block(
                &mut out,
                &sym,
                chunk,
                1,
                req.include_source,
                graph,
                &by_symbol,
                req.max_depth,
            );
            out.push_str("\n───────────────────────────────────────\n\n");
        }
    }

    // Depth-1 callers: signature-only (the caller's full body is rarely what
    // the LLM needs — they usually want callees deeply, callers shallowly).
    if matches!(
        req.direction,
        CallChainDirection::Both | CallChainDirection::Callers
    ) {
        for (sym, _chunk_id) in graph.callers_of(&entry_symbol, 1) {
            let Some(chunk) = by_symbol.get(sym.as_str()) else {
                continue;
            };
            render_caller_block(&mut out, &sym, chunk);
            out.push_str("\n───────────────────────────────────────\n\n");
        }
    }

    Ok(out)
}

fn render_entry_block(
    out: &mut String,
    symbol: &str,
    chunk: &RawChunk,
    graph: &SymbolGraph,
    req: &ValidatedCallChainRequest,
) {
    let (why, what) = extract_doc_sections(&chunk.content);
    let sig = extract_signature(&chunk.content).unwrap_or_else(|| "(signature unavailable)".into());
    out.push_str(&format!(
        "## `{symbol}` [ENTRY]  {}:{}\n",
        chunk.file, chunk.start_line
    ));
    out.push_str(&format!("Signature: {sig}\n"));
    out.push_str(&format!("Why: {}\n", why.as_deref().unwrap_or("(no doc)")));
    out.push_str(&format!(
        "What: {}\n",
        what.as_deref().unwrap_or("(no doc)")
    ));

    if matches!(
        req.direction,
        CallChainDirection::Both | CallChainDirection::Outgoing
    ) {
        let callees = graph.callees_of(symbol, 1);
        out.push_str("\nCalls →\n");
        if callees.is_empty() {
            out.push_str("  (none discovered)\n");
        } else {
            for (sym, chunk_id) in &callees {
                let loc = location_from_chunk_id(chunk_id);
                out.push_str(&format!("  · {sym}  {loc}\n"));
            }
        }
    }
    if matches!(
        req.direction,
        CallChainDirection::Both | CallChainDirection::Callers
    ) {
        let callers = graph.callers_of(symbol, 1);
        out.push_str("Called by ←\n");
        if callers.is_empty() {
            out.push_str("  (none discovered)\n");
        } else {
            for (sym, chunk_id) in &callers {
                let loc = location_from_chunk_id(chunk_id);
                out.push_str(&format!("  · {sym}  {loc}\n"));
            }
        }
    }
}

// `clippy::too_many_arguments` (8/7) fires under clippy 1.94 (newer than the
// repo's MSRV 1.88 CI baseline) on this private render helper. Bundling the
// args into a struct would add a single-use type for no clarity gain on an
// internal call-chain formatter; suppress locally.
#[allow(clippy::too_many_arguments)]
fn render_neighbor_block(
    out: &mut String,
    symbol: &str,
    chunk: &RawChunk,
    depth: u32,
    include_source: bool,
    graph: &SymbolGraph,
    by_symbol: &HashMap<&str, &RawChunk>,
    max_depth: u32,
) {
    let (why, what) = extract_doc_sections(&chunk.content);
    let sig = extract_signature(&chunk.content).unwrap_or_else(|| "(signature unavailable)".into());
    out.push_str(&format!(
        "## `{symbol}` [depth={depth}]  {}:{}\n",
        chunk.file, chunk.start_line
    ));
    out.push_str(&format!("Signature: {sig}\n"));
    out.push_str(&format!("Why: {}\n", why.as_deref().unwrap_or("(no doc)")));
    out.push_str(&format!(
        "What: {}\n",
        what.as_deref().unwrap_or("(no doc)")
    ));

    if include_source && depth <= 1 {
        let lang = chunk.language.as_deref().unwrap_or("").to_ascii_lowercase();
        out.push_str(&format!("\n```{lang}\n"));
        out.push_str(&chunk.content);
        if !chunk.content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }

    // Show one more level of depth as signature-only entries.
    if depth < max_depth {
        let next = graph.callees_of(symbol, 1);
        if !next.is_empty() {
            out.push_str(&format!(
                "\nCalls →  (depth={}, signatures only)\n",
                depth + 1
            ));
            for (sym, _chunk_id) in &next {
                let (next_sig, next_why) = by_symbol
                    .get(sym.as_str())
                    .map(|c| {
                        let s = extract_signature(&c.content)
                            .unwrap_or_else(|| "(signature unavailable)".into());
                        let (why_doc, _) = extract_doc_sections(&c.content);
                        (s, why_doc)
                    })
                    .unwrap_or_else(|| ("(unknown)".into(), None));
                let why_short = next_why
                    .map(|s| {
                        let first_line: String =
                            s.lines().next().unwrap_or("").chars().take(120).collect();
                        if first_line.is_empty() {
                            String::new()
                        } else {
                            format!("  // Why: {first_line}")
                        }
                    })
                    .unwrap_or_default();
                out.push_str(&format!("  · {sym}  {next_sig}{why_short}\n"));
            }
        }
    }
}

fn render_caller_block(out: &mut String, symbol: &str, chunk: &RawChunk) {
    let (why, _what) = extract_doc_sections(&chunk.content);
    let sig = extract_signature(&chunk.content).unwrap_or_else(|| "(signature unavailable)".into());
    out.push_str(&format!(
        "## `{symbol}` [caller]  {}:{}\n",
        chunk.file, chunk.start_line
    ));
    out.push_str(&format!("{sig}\n"));
    let why_line = why
        .map(|s| s.lines().next().unwrap_or("").to_string())
        .unwrap_or_default();
    if !why_line.is_empty() {
        out.push_str(&format!("// Why: {why_line}\n"));
    }
}

/// Parse a `RawChunk.id` of the form `"{file}:{start}:{end}"` back into a
/// human-readable `"file:line"` location string.
///
/// Why: the graph hands us `chunk_id` for each neighbour but the report
/// reads better with `file:line` than the synthetic id.
/// What: takes the last two `:`-separated components as `start:end`, keeps
/// just `start`, and prefixes with the file portion. Falls back to the id
/// itself on parse failure.
fn location_from_chunk_id(chunk_id: &str) -> String {
    // Split from the right twice to recover start_line.
    let parts: Vec<&str> = chunk_id.rsplitn(3, ':').collect();
    if parts.len() == 3 {
        // parts = [end, start, file]
        format!("{}:{}", parts[2], parts[1])
    } else {
        chunk_id.to_string()
    }
}

/// Convenience wrapper used by the HTTP/MCP entry points: take a graph
/// snapshot, chunk snapshot, and a validated request; produce the text.
///
/// Why: lets the two transports share one call site, and keeps `render_call_chain`
/// free of `Arc` plumbing for the unit tests.
pub fn render_from_snapshots(
    req: &ValidatedCallChainRequest,
    graph: Arc<SymbolGraph>,
    chunks: Vec<RawChunk>,
) -> Result<String, String> {
    render_call_chain(req, graph.as_ref(), &chunks)
}

#[cfg(test)]
mod tests;

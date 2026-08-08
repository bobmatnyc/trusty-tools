//! Generated documentation regions — render volatile facts from source and
//! assert the checked-in markdown still matches.
//!
//! Why (#5205 follow-up): a documentation sweep found 13 false claims in crate
//! READMEs, and the PR that hand-fixed them introduced a fourteenth — it cited
//! a function `tool_definitions` that does not exist in `trusty-search` (the
//! real symbol is `tool_descriptors`). Hand-maintained copies of a fact the
//! code already knows drift, and the same fact was maintained twice (README
//! and CLAUDE.md), so every fix had to land twice. This module makes the code
//! the single place those facts live: a test calls the real function, renders
//! the markdown, and fails if the file disagrees.
//!
//! What: marker parsing (`<!-- BEGIN GENERATED: <id> -->` …
//! `<!-- END GENERATED: <id> -->`), a deterministic MCP-tool-table renderer,
//! and [`assert_region`] / [`sync_region`], which check the region or — with
//! `UPDATE_DOCS=1` in the environment — rewrite it in place.
//!
//! Test: `crates/trusty-common/src/docgen/tests.rs`, plus the three consumer
//! tests `crates/{trusty-search,trusty-memory,trusty-analyze}/tests/generated_docs.rs`.
//!
//! Coverage is opt-in per file: a markdown file with no markers is not
//! checked by anything here. See `docs/reference/generated-doc-regions.md`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Environment variable that switches [`sync_region`] from checking to rewriting.
pub const UPDATE_ENV: &str = "UPDATE_DOCS";

/// Maximum rendered length of a tool summary before it is elided.
const SUMMARY_CAP: usize = 140;

/// Abbreviations whose trailing period must not end the first sentence.
const ABBREVIATIONS: &[&str] = &["e.g.", "i.e.", "etc.", "vs.", "approx.", "cf.", "Fig."];

/// Opening marker for a generated region.
pub fn begin_marker(id: &str) -> String {
    format!("<!-- BEGIN GENERATED: {id} -->")
}

/// Closing marker for a generated region.
pub fn end_marker(id: &str) -> String {
    format!("<!-- END GENERATED: {id} -->")
}

/// What went wrong while reading or rewriting a generated region.
#[derive(Debug)]
pub enum DocGenError {
    /// The file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The opening or closing marker is missing.
    MissingMarker {
        path: PathBuf,
        id: String,
        marker: String,
    },
    /// A marker appears more than once, so the region is ambiguous.
    DuplicateMarker {
        path: PathBuf,
        id: String,
        marker: String,
    },
    /// The closing marker precedes the opening one.
    InvertedMarkers { path: PathBuf, id: String },
    /// The descriptor value was not an array of objects carrying `name`.
    MalformedDescriptors(String),
}

impl fmt::Display for DocGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::MissingMarker { path, id, marker } => write!(
                f,
                "{}: no generated region `{id}` — expected the line `{marker}`. \
                 A file that loses its markers is no longer checked, so this is a failure, \
                 not a skip.",
                path.display()
            ),
            Self::DuplicateMarker { path, id, marker } => write!(
                f,
                "{}: `{marker}` appears more than once, so region `{id}` is ambiguous",
                path.display()
            ),
            Self::InvertedMarkers { path, id } => write!(
                f,
                "{}: the END marker for region `{id}` precedes its BEGIN marker",
                path.display()
            ),
            Self::MalformedDescriptors(msg) => write!(f, "malformed tool descriptors: {msg}"),
        }
    }
}

impl std::error::Error for DocGenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// One row of a rendered MCP tool table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    /// The tool's wire name.
    pub name: String,
    /// Accepted arguments, required first, optional suffixed with `?`.
    pub arguments: String,
    /// First sentence of the tool's own description, whitespace-collapsed.
    pub summary: String,
    /// Build configuration the tool ships under, when the crate has more than one.
    pub availability: Option<String>,
}

/// Extract one [`ToolRow`] per descriptor.
///
/// Why: the descriptor functions build their payload with `json!()` macros, so
/// executing them is the only exact oracle — parsing the Rust source is
/// fragile and a hand-written table is what drifted in the first place.
/// What: accepts either a bare descriptor array or the `{"tools": [...]}`
/// envelope `trusty-memory` returns, and yields name + first-sentence summary.
/// Ordering is imposed later by [`render_tool_section`], so no iteration order
/// from the caller can leak into the rendered output.
/// Test: `rows_accept_both_shapes`, `rows_reject_missing_name`.
pub fn tool_rows(descriptors: &Value) -> Result<Vec<ToolRow>, DocGenError> {
    let array = descriptors
        .as_array()
        .or_else(|| descriptors.get("tools").and_then(Value::as_array))
        .ok_or_else(|| {
            DocGenError::MalformedDescriptors(
                "expected an array, or an object with a `tools` array".to_string(),
            )
        })?;

    array
        .iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DocGenError::MalformedDescriptors(format!("descriptor without `name`: {tool}"))
                })?
                .to_string();
            let summary = first_sentence(
                tool.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let arguments = arguments(tool.get("inputSchema"));
            Ok(ToolRow {
                name,
                arguments,
                summary,
                availability: None,
            })
        })
        .collect()
}

/// Render a tool's argument list from its JSON Schema.
///
/// Why: the memory README maintained this column by hand and it drifted along
/// with everything else; the schema already states it exactly.
/// What: names in the schema's `required` array in declaration order, then
/// every remaining property sorted alphabetically and suffixed `?`. Sorting the
/// optional half is what keeps the output stable no matter how `serde_json`
/// happens to order object keys. Returns `—` for a tool that takes nothing.
/// Test: `arguments_lists_required_then_sorted_optional`.
fn arguments(schema: Option<&Value>) -> String {
    let Some(schema) = schema else {
        return "—".to_string();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut optional: Vec<&str> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| {
            p.keys()
                .map(String::as_str)
                .filter(|k| !required.contains(k))
                .collect()
        })
        .unwrap_or_default();
    optional.sort_unstable();

    let mut parts: Vec<String> = required.iter().map(|s| format!("`{s}`")).collect();
    parts.extend(optional.iter().map(|s| format!("`{s}?`")));
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(", ")
    }
}

/// Tag every row with the build configuration it ships under.
///
/// Why: `trusty-analyze` serves 19 tools by default and 3 more only under
/// `--features review`. A section stating one number is false under the other
/// configuration, so the rows carry the composition instead.
/// What: returns the rows with `availability` set to `label`.
/// Test: `analyze_section_states_both_configurations` in
/// `crates/trusty-analyze/tests/generated_docs.rs`.
#[must_use]
pub fn labelled(rows: Vec<ToolRow>, label: &str) -> Vec<ToolRow> {
    rows.into_iter()
        .map(|row| ToolRow {
            availability: Some(label.to_string()),
            ..row
        })
        .collect()
}

/// Render the markdown body of an MCP tool section.
///
/// Why: `README.md` and `CLAUDE.md` carried the same table by hand, so a wrong
/// entry had to be fixed twice. One render call feeds both files.
/// What: sorts rows by name (the stable key — no source or map ordering
/// reaches the output), emits a count sentence, an authoritative-source
/// pointer, and a two- or three-column table. Panics on duplicate tool names,
/// which would make the table silently lossy.
/// Test: `render_is_sorted_and_stable`, `render_adds_availability_column`.
#[must_use]
pub fn render_tool_section(source: &str, count_note: &str, rows: &[ToolRow]) -> String {
    let mut rows: Vec<&ToolRow> = rows.iter().collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(dup) = rows.windows(2).find(|w| w[0].name == w[1].name) {
        panic!("duplicate tool name in descriptors: {}", dup[0].name);
    }

    let with_availability = rows.iter().any(|r| r.availability.is_some());
    let mut out = String::new();
    out.push_str(&format!(
        "The MCP server registers {count_note}. Authoritative source: `{source}` —\n\
         this table is generated from it, not maintained by hand.\n\n"
    ));
    if with_availability {
        out.push_str("| Tool | Available | Arguments | Summary |\n|---|---|---|---|\n");
    } else {
        out.push_str("| Tool | Arguments | Summary |\n|---|---|---|\n");
    }
    for row in rows {
        let summary = escape_cell(&row.summary);
        let arguments = escape_cell(&row.arguments);
        if with_availability {
            let availability = escape_cell(row.availability.as_deref().unwrap_or("—"));
            out.push_str(&format!(
                "| `{}` | {availability} | {arguments} | {summary} |\n",
                row.name
            ));
        } else {
            out.push_str(&format!("| `{}` | {arguments} | {summary} |\n", row.name));
        }
    }
    out
}

/// Build the count sentence from derived numbers.
///
/// Why: the count is the fact that went stale — 18 documented against 21 real
/// tools. It must never be typed by a human again.
/// What: `[( "", 21 )]` renders `**21 tools**`; multiple entries render
/// `**19 tools** with default features, **22 tools** with \`--features review\``.
/// Test: `count_note_renders_single_and_multi`.
#[must_use]
pub fn count_note(counts: &[(&str, usize)]) -> String {
    counts
        .iter()
        .map(|(label, n)| {
            if label.is_empty() {
                format!("**{n} tools**")
            } else {
                format!("**{n} tools** {label}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What [`sync_region`] did to the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The checked-in region already matched.
    UpToDate,
    /// The region differed and `UPDATE_DOCS` was set, so the file was rewritten.
    Rewritten,
    /// The region differed and `UPDATE_DOCS` was not set.
    Stale {
        /// A line-level diff of checked-in versus generated content.
        diff: String,
    },
}

/// Check — or, under `UPDATE_DOCS`, rewrite — one generated region.
///
/// Why: the same call has to serve both the CI gate and the developer's
/// regeneration step; two code paths would let them disagree about what
/// "correct" means.
/// What: locates the region by its markers, compares the body against `body`,
/// and either reports [`Outcome::Stale`] with a diff or writes the new body
/// back when `UPDATE_DOCS` is set to anything other than empty or `0`.
/// Test: `sync_reports_stale_then_rewrites`.
pub fn sync_region(path: &Path, id: &str, body: &str) -> Result<Outcome, DocGenError> {
    sync_region_mode(path, id, body, update_requested())
}

/// [`sync_region`] with the update decision supplied rather than read from the
/// environment, so unit tests exercise both modes without mutating global state.
fn sync_region_mode(
    path: &Path,
    id: &str,
    body: &str,
    update: bool,
) -> Result<Outcome, DocGenError> {
    let text = std::fs::read_to_string(path).map_err(|source| DocGenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let (start, end) = region_bounds(&text, path, id)?;
    let current = &text[start..end];
    let desired = format!("\n{}\n", body.trim_end());

    if current == desired {
        return Ok(Outcome::UpToDate);
    }
    if !update {
        return Ok(Outcome::Stale {
            diff: line_diff(current, &desired),
        });
    }
    let mut rewritten = String::with_capacity(text.len() + desired.len());
    rewritten.push_str(&text[..start]);
    rewritten.push_str(&desired);
    rewritten.push_str(&text[end..]);
    std::fs::write(path, rewritten).map_err(|source| DocGenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Outcome::Rewritten)
}

/// Assert a generated region is current, naming the exact command that fixes it.
///
/// Why: a diff with no remedy is a bad gate — the developer who hits this has
/// usually never seen the mechanism before.
/// What: calls [`sync_region`] and panics on [`Outcome::Stale`] with the diff,
/// the file, and `regen_cmd`. Succeeds silently when the region was rewritten
/// under `UPDATE_DOCS`.
/// Test: proven in both directions by `generated_docs.rs` in each consumer
/// crate; the panic text itself by `assert_region_panics_with_remedy`.
pub fn assert_region(path: &Path, id: &str, body: &str, regen_cmd: &str) {
    match sync_region(path, id, body) {
        Ok(Outcome::UpToDate) => {}
        Ok(Outcome::Rewritten) => {
            eprintln!("docgen: rewrote region `{id}` in {}", path.display());
        }
        Ok(Outcome::Stale { diff }) => panic!(
            "\ngenerated region `{id}` in {} is stale.\n\n{diff}\n\
             Do not hand-edit inside the markers — the source of truth is the code.\n\
             Regenerate with:\n\n    {regen_cmd}\n",
            path.display()
        ),
        Err(e) => panic!("\ndocgen failed for region `{id}`: {e}\n"),
    }
}

/// Byte range of the region body, exclusive of the marker lines.
fn region_bounds(text: &str, path: &Path, id: &str) -> Result<(usize, usize), DocGenError> {
    let begin = locate(text, path, id, &begin_marker(id))?;
    let end = locate(text, path, id, &end_marker(id))?;
    let body_start = begin.1;
    let body_end = end.0;
    if body_end < body_start {
        return Err(DocGenError::InvertedMarkers {
            path: path.to_path_buf(),
            id: id.to_string(),
        });
    }
    Ok((body_start, body_end))
}

/// Byte offsets of a marker's first and last character, rejecting duplicates.
fn locate(text: &str, path: &Path, id: &str, marker: &str) -> Result<(usize, usize), DocGenError> {
    let mut hits = text.match_indices(marker);
    let (at, _) = hits.next().ok_or_else(|| DocGenError::MissingMarker {
        path: path.to_path_buf(),
        id: id.to_string(),
        marker: marker.to_string(),
    })?;
    if hits.next().is_some() {
        return Err(DocGenError::DuplicateMarker {
            path: path.to_path_buf(),
            id: id.to_string(),
            marker: marker.to_string(),
        });
    }
    Ok((at, at + marker.len()))
}

/// Whether `UPDATE_DOCS` asks for a rewrite rather than a check.
fn update_requested() -> bool {
    match std::env::var(UPDATE_ENV) {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// Line-level diff, capped so a large drift stays readable.
fn line_diff(checked_in: &str, generated: &str) -> String {
    let old: Vec<&str> = checked_in.lines().collect();
    let new: Vec<&str> = generated.lines().collect();
    let mut out = String::from("--- checked in\n+++ generated from source\n");
    let mut shown = 0usize;
    for line in old.iter().filter(|l| !new.contains(*l)) {
        if shown == 40 {
            out.push_str("… (diff truncated)\n");
            return out;
        }
        out.push_str(&format!("-{line}\n"));
        shown += 1;
    }
    for line in new.iter().filter(|l| !old.contains(*l)) {
        if shown == 40 {
            out.push_str("… (diff truncated)\n");
            return out;
        }
        out.push_str(&format!("+{line}\n"));
        shown += 1;
    }
    out
}

/// First sentence of a description, whitespace-collapsed and length-capped.
fn first_sentence(description: &str) -> String {
    let collapsed = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentence = collapsed.as_str();
    let mut from = 0usize;
    while let Some(rel) = collapsed[from..].find(". ") {
        let at = from + rel;
        let head = &collapsed[..=at];
        if ABBREVIATIONS.iter().any(|abbr| head.ends_with(abbr)) {
            from = at + 2;
            continue;
        }
        sentence = &collapsed[..=at];
        break;
    }
    let sentence = sentence.trim();
    if sentence.chars().count() <= SUMMARY_CAP {
        return sentence.to_string();
    }
    let truncated: String = sentence.chars().take(SUMMARY_CAP).collect();
    let cut = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{}…", truncated[..cut].trim_end())
}

/// Escape the characters that would break out of a markdown table cell.
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Last `::` segment of a stringified path, whitespace removed.
///
/// Why: `stringify!` inserts spaces around `::` on some token streams.
#[must_use]
pub fn normalise_path(stringified: &str) -> String {
    stringified.split_whitespace().collect()
}

/// Name the descriptor function by a compiler-checked path.
///
/// Why: the fourteenth false claim was a citation of `tool_definitions`, a
/// function that does not exist. This macro stringifies a path that must also
/// resolve to a real `fn() -> serde_json::Value`, so a wrong or renamed symbol
/// is a compile error rather than a wrong sentence in a README.
/// What: expands to a `String` holding the normalised path, after coercing it
/// to a zero-argument function pointer. The return type stays generic so both
/// `fn() -> Value` and `fn() -> Vec<Value>` descriptor functions qualify.
/// Test: `crates/trusty-search/tests/generated_docs.rs` cites
/// `trusty_search::mcp::tools::tool_descriptors` through it.
#[macro_export]
macro_rules! descriptor_source {
    ($path:path) => {{
        // The coercion forces the path to resolve to a zero-argument function;
        // a rename breaks the build here instead of silently leaving a false
        // citation in the docs.
        fn _resolves<T>(_descriptor_fn: fn() -> T) {}
        _resolves($path);
        $crate::docgen::normalise_path(stringify!($path))
    }};
}

#[cfg(test)]
mod tests;

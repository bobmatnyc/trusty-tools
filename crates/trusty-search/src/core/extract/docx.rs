//! DOCX (Word) text extraction (issue #2923).
//!
//! Why: `.docx` is a zip container around a handful of OOXML parts. Rather
//! than pull in a full-featured docx crate for a single-purpose need
//! (paragraph text extraction), this module owns a small streaming
//! `quick-xml` parse of `word/document.xml`, matching this workspace's
//! own-the-parsing convention (see the tree-sitter chunkers under
//! `core::chunker`).
//!
//! What: [`extract`] opens the zip, locates `word/document.xml`, and streams
//! it collecting `<w:t>` run-text content, emitting a paragraph break at each
//! `</w:p>`. Paragraph structure that carries meaning is preserved in the
//! emitted text (#4879): a `<w:tbl>` becomes markdown-style pipe rows so cell
//! boundaries survive into the chunker, and a heading paragraph is prefixed
//! with markdown `#` markers at its resolved outline depth. Headers/footers
//! (separate zip parts, e.g. `word/header1.xml`) are intentionally out of
//! scope for v1 — the body is the primary searchable content.
//!
//! Truncated input is a first-class case, not an edge case: the file watcher
//! reads documents while Word is still writing them. A document ending inside
//! an open element emits everything parsed so far — see [`close_open_tables`].
//!
//! Test: `test_extracts_paragraphs_preserving_breaks`,
//! `test_table_rows_become_pipe_delimited_lines`,
//! `test_heading_paragraphs_get_markdown_prefix`,
//! `test_unterminated_table_still_emits_its_content`,
//! `test_missing_document_xml_errors`, `test_not_a_zip_errors`.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::{ExtractError, Extracted};

/// The zip entry holding the document body, per the OOXML WordprocessingML
/// package convention.
const DOCUMENT_XML_PATH: &str = "word/document.xml";

/// The zip entry defining named paragraph styles.
///
/// Why: a heading paragraph names a style ID (`<w:pStyle w:val="Heading1"/>`),
/// and only this part maps that ID to an outline level. Word's en-US IDs
/// happen to be self-describing, but a localized or custom style ID is
/// opaque — resolving it here is what stopped the #4875 spike from reading 0
/// headings on a document that had 6.
/// What: read best-effort; an absent or unreadable part degrades to the
/// style-ID-name heuristic in [`heading_level_from_name`] rather than failing
/// the extraction.
/// Test: `test_heading_level_resolved_via_styles_xml`,
/// `test_missing_styles_xml_falls_back_to_style_id`.
const STYLES_XML_PATH: &str = "word/styles.xml";

/// Deepest markdown heading level emitted.
///
/// Word outline levels run 0..=8 (nine heading depths); markdown defines six.
/// Deeper Word headings clamp here rather than emitting `#######`, which no
/// markdown reader treats as a heading.
const MAX_HEADING_DEPTH: u8 = 6;

/// Maximum `<w:tbl>` nesting depth that allocates a [`TableCtx`].
///
/// Why: the parse pushes one context per open table, so a crafted
/// `document.xml` sitting under [`MAX_DOCUMENT_XML_BYTES`] could otherwise
/// drive millions of allocated contexts from ~19 bytes of markup each. Real
/// documents nest one or two deep; 16 is far past anything Word produces.
/// What: tables opened past this depth are not tracked — their text still
/// reaches the innermost tracked cell, so content degrades to plain text
/// instead of being lost.
/// Test: `test_table_nesting_depth_is_capped`.
const MAX_TABLE_NESTING_DEPTH: u8 = 16;

/// The `w:outlineLvl` value meaning "body text, not in the outline".
///
/// Why: OOXML uses 0..=8 for heading depths and 9 for "no outline level", so
/// a style carrying 9 (Word's built-in `TOC Heading` does) must NOT be
/// reported as a heading.
/// Test: `test_outline_level_nine_is_not_a_heading`.
const OUTLINE_LEVEL_BODY_TEXT: u8 = 9;

/// Cap on the UNCOMPRESSED size of `word/document.xml` (bytes).
///
/// Why: `MAX_OFFICE_FILE_BYTES` caps only the compressed container on disk;
/// DEFLATE ratios can reach ~1000:1, so without a decompressed-size bound a
/// crafted ~10 MiB `.docx` (a zip bomb) could expand to multi-GB in memory
/// before the post-hoc `MAX_EXTRACTED_TEXT_BYTES` truncation in
/// `extract_text` ever runs — one hostile file in a watched directory would
/// OOM the daemon. 50 MiB of XML gives ~10x markup overhead headroom over
/// the 5 MiB extracted-text cap while keeping worst-case memory bounded.
/// What: enforced twice in [`extract`] — the zip entry's declared
/// uncompressed size is rejected up front, AND the reader is wrapped in
/// `Read::take` so a lying size field cannot bypass the bound.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
const MAX_DOCUMENT_XML_BYTES: u64 = 50 * 1024 * 1024;

/// Extract paragraph text from a `.docx` file.
///
/// Why/What: see module docs. Decompression is bounded by
/// [`MAX_DOCUMENT_XML_BYTES`] (zip-bomb defence; see that constant's docs).
/// Test: `test_extracts_paragraphs_preserving_breaks`,
/// `test_oversized_document_xml_rejected_by_declared_size`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let file = std::fs::File::open(path).map_err(|source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| ExtractError::Docx(e.to_string()))?;

    // #4879: styles must be read before the body so heading paragraphs can
    // resolve their outline depth as they stream past.
    let styles = heading_styles(&mut archive, path);

    let entry = archive
        .by_name(DOCUMENT_XML_PATH)
        .map_err(|e| ExtractError::Docx(format!("{DOCUMENT_XML_PATH}: {e}")))?;
    let declared = entry.size();
    let xml = read_entry_bounded(entry, declared, DOCUMENT_XML_PATH, MAX_DOCUMENT_XML_BYTES)?;

    Ok(Extracted {
        text: paragraphs_from_document_xml(&xml, &styles)?,
        warning: None,
    })
}

/// Read `word/styles.xml` and map each paragraph style ID to a heading depth.
///
/// Why: heading depth lives in the style definition, not on the paragraph, so
/// without this part a `<w:pStyle w:val="Ttulo1"/>` (localized Word) or any
/// custom style ID is unresolvable. See [`STYLES_XML_PATH`].
/// What: best-effort. A missing part yields an empty map; an oversized or
/// malformed one is logged and yields an empty map — the body is still
/// extracted and heading depth falls back to [`heading_level_from_name`],
/// because refusing to index a document over its stylesheet would be a worse
/// outcome than indexing it with less structure.
/// Test: `test_heading_level_resolved_via_styles_xml`,
/// `test_missing_styles_xml_falls_back_to_style_id`.
fn heading_styles(
    archive: &mut zip::ZipArchive<std::fs::File>,
    path: &Path,
) -> HashMap<String, u8> {
    let Ok(entry) = archive.by_name(STYLES_XML_PATH) else {
        return HashMap::new();
    };
    let declared = entry.size();
    match read_entry_bounded(entry, declared, STYLES_XML_PATH, MAX_DOCUMENT_XML_BYTES)
        .and_then(|xml| heading_styles_from_xml(&xml))
    {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "docx: {STYLES_XML_PATH} unreadable; heading depth falls back to style-ID names"
            );
            HashMap::new()
        }
    }
}

/// Read a zip entry to a `String`, refusing to decompress past `cap` bytes.
///
/// Why: the zip-bomb defence must hold even when the entry's central-directory
/// size field lies, so the declared-size check alone is not enough — the
/// actual decompressed byte stream is also hard-capped via `Read::take`.
/// What: rejects when the entry DECLARES (`declared`) more than `cap`
/// uncompressed bytes; otherwise reads at most `cap + 1` bytes and rejects if
/// the stream exceeds `cap` (i.e. the declared size was false). `name` is the
/// zip entry path, used only to name the offending part in the error. Content
/// must be valid UTF-8. Generic over `Read` so the lying-size path is
/// unit-testable without crafting a malicious zip.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
fn read_entry_bounded<R: Read>(
    entry: R,
    declared: u64,
    name: &str,
    cap: u64,
) -> Result<String, ExtractError> {
    if declared > cap {
        return Err(ExtractError::Docx(format!(
            "{name} declares {declared} uncompressed bytes, over the {cap} byte cap"
        )));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    entry
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ExtractError::Docx(e.to_string()))?;
    if bytes.len() as u64 > cap {
        return Err(ExtractError::Docx(format!(
            "{name} decompressed past the {cap} byte cap (declared {declared})"
        )));
    }
    String::from_utf8(bytes).map_err(|e| ExtractError::Docx(e.to_string()))
}

/// Read the `w:val` attribute of a start/empty element.
fn attr_val(e: &BytesStart<'_>) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.local_name().as_ref() == b"val")
            .then(|| String::from_utf8_lossy(a.value.as_ref()).into_owned())
    })
}

/// Map a style's human-readable name (or its ID) to a markdown heading depth.
///
/// Why: Word's built-in heading styles are named `heading 1`..`heading 9` and
/// their en-US IDs read `Heading1`..`Heading9`, so one normalization covers
/// both the styles.xml name and the raw style ID fallback.
/// What: case-insensitive `heading<sep><n>` where `<sep>` is any run of space,
/// hyphen, or underscore. Returns `None` for anything else — notably the
/// `Heading 1 Char` RUN styles, which must not mark a paragraph as a heading.
/// Test: `test_heading_level_from_name`.
fn heading_level_from_name(name: &str) -> Option<u8> {
    let lower = name.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("heading")?;
    let digits = rest.trim_matches(|c: char| c == ' ' || c == '-' || c == '_');
    let level: u8 = digits.parse().ok()?;
    (1..=9)
        .contains(&level)
        .then(|| level.min(MAX_HEADING_DEPTH))
}

/// Convert a `w:outlineLvl` value to a markdown heading depth.
///
/// Why/What: OOXML outline levels are 0-based and reserve
/// [`OUTLINE_LEVEL_BODY_TEXT`] for "not a heading"; markdown depths are
/// 1-based and stop at [`MAX_HEADING_DEPTH`].
/// Test: `test_outline_level_nine_is_not_a_heading`.
fn heading_level_from_outline(val: &str) -> Option<u8> {
    let level: u8 = val.trim().parse().ok()?;
    (level < OUTLINE_LEVEL_BODY_TEXT).then(|| (level + 1).min(MAX_HEADING_DEPTH))
}

/// Parse `word/styles.xml` into a `styleId -> heading depth` map.
///
/// Why: isolated from [`heading_styles`] so the parse is unit-testable
/// against literal XML without a zip container.
/// What: for each `<w:style>`, an explicit `<w:outlineLvl>` wins over the
/// `<w:name>` heuristic (a custom style named "Body" but carrying outline
/// level 0 really is a heading). Styles resolving to neither are omitted.
/// Test: `test_heading_styles_from_xml`, `test_outline_level_nine_is_not_a_heading`.
fn heading_styles_from_xml(xml: &str) -> Result<HashMap<String, u8>, ExtractError> {
    let mut reader = Reader::from_str(xml);
    let mut styles = HashMap::new();
    let mut id: Option<String> = None;
    let mut name_level: Option<u8> = None;
    let mut outline: Option<Option<u8>> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"style" => {
                id = e.attributes().flatten().find_map(|a| {
                    (a.key.local_name().as_ref() == b"styleId")
                        .then(|| String::from_utf8_lossy(a.value.as_ref()).into_owned())
                });
                name_level = None;
                outline = None;
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if id.is_some() => {
                match e.local_name().as_ref() {
                    b"name" => {
                        name_level = attr_val(&e).as_deref().and_then(heading_level_from_name)
                    }
                    b"outlineLvl" => {
                        outline = Some(attr_val(&e).as_deref().and_then(heading_level_from_outline))
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"style" => {
                // An explicit outlineLvl is authoritative even when it says
                // "body text" (9), so a demoted style named "heading 1" is
                // NOT reported as a heading.
                let level = match outline.take() {
                    Some(explicit) => explicit,
                    None => name_level,
                };
                if let (Some(style_id), Some(level)) = (id.take(), level) {
                    styles.insert(style_id, level);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ExtractError::Docx(e.to_string())),
            _ => {}
        }
    }
    Ok(styles)
}

/// One `<w:tbl>` currently being streamed.
///
/// Why: tables nest (a table may sit inside a cell of another), so the parse
/// keeps a stack of these rather than a single current-table slot.
/// What: `row` accumulates the closed cells of the row in document order;
/// `cell` is `Some` only between `<w:tc>` and `</w:tc>`; `rows_emitted`
/// distinguishes the header row (which is followed by the markdown delimiter)
/// from the rest.
/// Test: `test_nested_table_rows_stay_inside_the_outer_cell`.
#[derive(Default)]
struct TableCtx {
    rows_emitted: usize,
    row: Vec<String>,
    cell: Option<String>,
}

/// Append a finished block to whichever sink is currently open.
///
/// Why: a paragraph or table row inside a `<w:tc>` belongs to that cell, not
/// to the document body — without this the cell's own text would be flushed
/// to the output as an anonymous paragraph, which is the #4879 defect.
/// What: appends to the innermost open cell in `enclosing`, or to `out`
/// followed by `trailing` when no cell is open.
/// Test: `test_table_rows_become_pipe_delimited_lines`,
/// `test_nested_table_rows_stay_inside_the_outer_cell`.
fn sink(out: &mut String, enclosing: &mut [TableCtx], block: &str, trailing: &str) {
    match enclosing.iter_mut().rev().find_map(|t| t.cell.as_mut()) {
        Some(cell) => {
            if !cell.is_empty() && !block.is_empty() {
                cell.push(' ');
            }
            cell.push_str(block);
        }
        None => {
            out.push_str(block);
            out.push_str(trailing);
        }
    }
}

/// Close the open cell of the innermost table, pushing it onto that table's
/// pending row.
///
/// Why: `</w:tc>` and the EOF unwind must close a cell identically, so both
/// call this rather than each writing the step out. See [`close_table`].
/// Test: `test_unterminated_row_matches_closed_row`.
fn close_cell(tables: &mut [TableCtx]) {
    if let Some(table) = tables.last_mut() {
        let text = table.cell.take().unwrap_or_default();
        table.row.push(normalize_cell(&text));
    }
}

/// Emit the innermost table's pending row as a pipe-delimited line.
///
/// Why: shared by `</w:tr>` and the EOF unwind — see [`close_table`].
/// What: emits nothing for an empty row; the first row of each table is
/// followed by the markdown delimiter that makes it read as a table.
/// Test: `test_table_rows_become_pipe_delimited_lines`,
/// `test_unterminated_row_matches_closed_row`.
fn close_row(out: &mut String, tables: &mut [TableCtx]) {
    let Some((table, enclosing)) = tables.split_last_mut() else {
        return;
    };
    if !table.row.is_empty() {
        let mut block = format!("| {} |", table.row.join(" | "));
        table.rows_emitted += 1;
        if table.rows_emitted == 1 {
            block.push_str("\n|");
            for _ in 0..table.row.len() {
                block.push_str(" --- |");
            }
        }
        sink(out, enclosing, &block, "\n");
    }
    table.row.clear();
}

/// Pop the innermost table and close its block.
///
/// Why: `</w:tbl>` and the EOF unwind must agree byte for byte. A second,
/// simpler EOF path that only approximated this is exactly what the paragraph
/// flush already avoids, so the table flush routes through the same helpers.
/// What: a table that emitted rows gets a blank line after it so the next
/// paragraph does not read as one more row.
/// Test: `test_paragraph_after_table_is_separated`,
/// `test_unterminated_table_still_emits_its_content`.
fn close_table(out: &mut String, tables: &mut Vec<TableCtx>) {
    if tables.pop().is_some_and(|t| t.rows_emitted > 0) {
        sink(out, tables, "", "\n");
    }
}

/// Close every still-open table at EOF, innermost first.
///
/// Why: `.docx` files are read mid-write by the file watcher and arrive
/// truncated over the network, and both end inside an open `<w:tbl>`. Without
/// this the document indexes with no error and no warning while every table
/// cell is missing (#4879). The paragraph path has had the equivalent
/// trailing-flush guard all along.
/// What: replays the same `</w:tc>` / `</w:tr>` / `</w:tbl>` sequence the
/// well-formed path would have run, through the same three helpers, so the
/// output is identical to the closed document's.
/// Test: `test_unterminated_table_still_emits_its_content`,
/// `test_unterminated_row_matches_closed_row`.
fn close_open_tables(out: &mut String, tables: &mut Vec<TableCtx>) {
    while !tables.is_empty() {
        if tables.last().is_some_and(|t| t.cell.is_some()) {
            close_cell(tables);
        }
        close_row(out, tables);
        close_table(out, tables);
    }
}

/// Normalize one cell's accumulated text for a pipe-delimited row.
///
/// Why: the row delimiter is only meaningful if it cannot be confused with
/// cell content, and a cell holding several paragraphs must still occupy one
/// column.
/// What: collapses all whitespace runs to single spaces and escapes literal
/// `|` as `\|`.
/// Test: `test_cell_text_with_pipe_is_escaped`.
fn normalize_cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Parse `word/document.xml` content into text that preserves the document's
/// table and heading structure, with a blank line between paragraphs
/// (matching the plaintext/document chunker's paragraph-break convention).
///
/// Why: isolated from `extract` so it can be unit tested against literal XML
/// fixtures without a real zip container.
/// What: streams `<w:t>` run text into the current paragraph buffer, flushing
/// on `</w:p>`. #4879 adds two structure-preserving behaviours on top of
/// that flush:
///
/// - a `<w:tbl>` emits one `| a | b |` line per `<w:tr>`, with a markdown
///   `| --- |` delimiter after the first row, so cell boundaries survive into
///   `chunk_text` (which sees the extracted text as plain lines);
/// - a paragraph whose `<w:pPr>` names a heading style, or carries an
///   explicit `<w:outlineLvl>`, is prefixed with that many `#` markers.
///
/// `styles` maps style IDs to heading depth; see [`heading_styles`].
/// Test: `test_paragraphs_from_document_xml_basic`,
/// `test_table_rows_become_pipe_delimited_lines`,
/// `test_heading_paragraphs_get_markdown_prefix`,
/// `test_paragraphs_from_document_xml_unescapes_entities`.
fn paragraphs_from_document_xml(
    xml: &str,
    styles: &HashMap<String, u8>,
) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;
    let mut in_ppr = false;
    let mut heading: Option<u8> = None;
    let mut tables: Vec<TableCtx> = Vec::new();
    // Depth of `<w:tbl>` nesting beyond MAX_TABLE_NESTING_DEPTH, tracked so
    // the matching `</w:tbl>` does not pop a table that was never pushed.
    let mut over_depth: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => in_text = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::Text(t)) if in_text => {
                // quick-xml 0.41 removed `BytesText::unescape()` (dependency
                // bump for RUSTSEC-2026-0194/0195, issue #3367): decode the
                // raw bytes first, then unescape XML entities via the
                // free-function equivalent. In 0.41 a `Text` event itself
                // never contains an escaped entity (see the `GeneralRef` arm
                // below), so `unescape` is a no-op here in practice — kept
                // for defence-in-depth in case that reader behavior changes.
                let decoded = t.decode().map_err(|e| ExtractError::Docx(e.to_string()))?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|e| ExtractError::Docx(e.to_string()))?;
                para.push_str(&unescaped);
            }
            // quick-xml 0.41 stopped inlining entity/character references
            // (`&amp;`, `&#233;`, ...) into surrounding `Text` events; each
            // reference is now its own `GeneralRef` event. Without this arm,
            // entity references inside `<w:t>` were silently dropped
            // (`Tom &amp; Jerry` extracted as `Tom  Jerry`) rather than
            // resolved — caught by
            // `test_paragraphs_from_document_xml_unescapes_entities`.
            Ok(Event::GeneralRef(r)) if in_text => {
                if let Some(c) = r
                    .resolve_char_ref()
                    .map_err(|e| ExtractError::Docx(e.to_string()))?
                {
                    para.push(c);
                } else {
                    let name = r.decode().map_err(|e| ExtractError::Docx(e.to_string()))?;
                    match quick_xml::escape::resolve_predefined_entity(&name) {
                        Some(resolved) => para.push_str(resolved),
                        None => {
                            return Err(ExtractError::Docx(format!(
                                "unresolvable XML entity reference: &{name};"
                            )));
                        }
                    }
                }
            }
            // #4879: a paragraph's heading depth is declared in <w:pPr>,
            // either by naming a style or by an explicit outline level.
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"pPr" => in_ppr = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"pPr" => in_ppr = false,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if in_ppr => {
                match e.local_name().as_ref() {
                    b"pStyle" => {
                        heading = attr_val(&e).and_then(|id| {
                            styles
                                .get(&id)
                                .copied()
                                .or_else(|| heading_level_from_name(&id))
                        })
                    }
                    // Direct formatting on the paragraph outranks its style,
                    // and appears after <w:pStyle> in document order.
                    b"outlineLvl" => {
                        heading = attr_val(&e).as_deref().and_then(heading_level_from_outline)
                    }
                    _ => {}
                }
            }
            // #4879: past MAX_TABLE_NESTING_DEPTH a table is not tracked at
            // all — its rows and cells are ignored so their text falls
            // through to the innermost tracked cell as plain prose.
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"tbl" => {
                if over_depth > 0 || tables.len() >= MAX_TABLE_NESTING_DEPTH as usize {
                    over_depth += 1;
                } else {
                    tables.push(TableCtx::default());
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"tbl" => {
                if over_depth > 0 {
                    over_depth -= 1;
                } else {
                    close_table(&mut out, &mut tables);
                }
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"tc" && over_depth == 0 => {
                if let Some(table) = tables.last_mut() {
                    table.cell = Some(String::new());
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"tc" && over_depth == 0 => {
                close_cell(&mut tables)
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"tr" && over_depth == 0 => {
                close_row(&mut out, &mut tables)
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                let text = para.trim_end();
                // A heading marker inside a table cell would be noise, so
                // only the text carries over there.
                let in_cell = tables.iter().any(|t| t.cell.is_some());
                let block = match heading {
                    Some(level) if !text.is_empty() && !in_cell => {
                        format!("{} {text}", "#".repeat(level as usize))
                    }
                    _ => text.to_string(),
                };
                sink(&mut out, &mut tables, &block, "\n\n");
                para.clear();
                heading = None;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ExtractError::Docx(e.to_string())),
            _ => {}
        }
    }
    // A trailing run outside any `</w:p>` (malformed/truncated input) is
    // still surfaced rather than silently dropped. It routes through `sink`
    // so a run truncated inside a cell lands in that cell, which the unwind
    // below then flushes.
    if !para.trim().is_empty() {
        sink(&mut out, &mut tables, para.trim_end(), "\n");
    }
    // #4879: same guarantee for a document truncated inside a `<w:tbl>`.
    close_open_tables(&mut out, &mut tables);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    /// Zip up a minimal but structurally valid `.docx` containing the given
    /// `word/document.xml` body XML (the `<w:body>...</w:body>` inner
    /// content only — this helper wraps it in the document envelope).
    fn build_minimal_docx(body_xml: &str) -> Vec<u8> {
        build_docx(body_xml, None)
    }

    /// Body XML for a `rows` x `cols` table whose cell text is `r{r}c{c}`.
    fn table_xml(rows: usize, cols: usize) -> String {
        let mut out = String::from("<w:tbl><w:tblPr/><w:tblGrid/>");
        for r in 0..rows {
            out.push_str("<w:tr>");
            for c in 0..cols {
                out.push_str(&format!(
                    "<w:tc><w:tcPr/><w:p><w:r><w:t>r{r}c{c}</w:t></w:r></w:p></w:tc>"
                ));
            }
            out.push_str("</w:tr>");
        }
        out.push_str("</w:tbl>");
        out
    }

    /// A paragraph carrying `<w:pStyle w:val="{style}"/>`.
    fn styled_para(style: &str, text: &str) -> String {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
        )
    }

    /// `word/styles.xml` defining `Heading1..Heading3` the way Word does:
    /// an opaque style ID whose depth is carried by `<w:outlineLvl>`.
    fn heading_styles_xml() -> String {
        let styles: String = (1..=3)
            .map(|n| {
                format!(
                    r#"<w:style w:type="paragraph" w:styleId="Custom{n}"><w:name w:val="My Heading {n}"/><w:pPr><w:outlineLvl w:val="{}"/></w:pPr></w:style>"#,
                    n - 1
                )
            })
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {NS}>{styles}</w:styles>"#
        )
    }

    /// Zip up a `.docx`, optionally including a `word/styles.xml` part.
    fn build_docx(body_xml: &str, styles_xml: Option<&str>) -> Vec<u8> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS}><w:body>{body_xml}</w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file(DOCUMENT_XML_PATH, opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            if let Some(styles) = styles_xml {
                zip.start_file(STYLES_XML_PATH, opts).unwrap();
                zip.write_all(styles.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn test_extracts_paragraphs_preserving_breaks() {
        let body = "<w:p><w:r><w:t>Hello world.</w:t></w:r></w:p>\
                     <w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>";
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("doc.docx");
        std::fs::write(&path, build_minimal_docx(body)).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("Hello world."));
        assert!(extracted.text.contains("Second paragraph."));
        // Paragraph break: the two paragraphs are separated by a blank line.
        assert!(extracted.text.contains("Hello world.\n\nSecond paragraph."));
        assert!(extracted.warning.is_none());
    }

    /// #4879: a `<w:tbl>` cell arrived as an anonymous paragraph
    /// indistinguishable from body text — a 5x4 table extracted as 20 loose
    /// paragraphs with no row or column boundary anywhere in the output. Each
    /// row must now be one line whose cells are pipe-delimited.
    #[test]
    fn test_table_rows_become_pipe_delimited_lines() {
        let text = paragraphs_from_document_xml(&table_xml(5, 4), &HashMap::new()).unwrap();

        let rows: Vec<&str> = text.lines().filter(|l| l.starts_with('|')).collect();
        // 5 content rows + the markdown delimiter row after the first.
        assert_eq!(rows.len(), 6, "expected 5 rows + 1 delimiter, got: {text}");
        assert_eq!(rows[0], "| r0c0 | r0c1 | r0c2 | r0c3 |");
        assert_eq!(rows[1], "| --- | --- | --- | --- |");
        assert_eq!(rows[5], "| r4c0 | r4c1 | r4c2 | r4c3 |");
        // Cells must NOT also appear as standalone paragraphs.
        assert!(
            !text.lines().any(|l| l.trim() == "r0c0"),
            "cell text leaked as a bare paragraph: {text}"
        );
    }

    /// #4879: heading level was never read, so a heading was indistinguishable
    /// from body text. `Heading1`/`Heading2` are Word's own en-US style IDs —
    /// the exact ones in this repo's `code_search_analysis.docx`.
    #[test]
    fn test_heading_paragraphs_get_markdown_prefix() {
        let body = format!(
            "{}{}{}",
            styled_para("Heading1", "Top level"),
            styled_para("Heading2", "Second level"),
            "<w:p><w:r><w:t>Body text.</w:t></w:r></w:p>"
        );
        let text = paragraphs_from_document_xml(&body, &HashMap::new()).unwrap();

        assert!(text.contains("# Top level"), "{text}");
        assert!(text.contains("## Second level"), "{text}");
        assert!(
            text.contains("\nBody text.") || text.starts_with("Body text."),
            "unstyled paragraphs must stay unprefixed: {text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with('#')).count(),
            2,
            "{text}"
        );
    }

    /// The end-to-end path: heading depth resolved through `word/styles.xml`
    /// for an opaque style ID (a localized or custom Word style), which the
    /// style-ID-name heuristic alone cannot read. The #4875 spike measured 0
    /// headings on a fixture omitting this part.
    #[test]
    fn test_heading_level_resolved_via_styles_xml() {
        let body = format!(
            "{}{}",
            styled_para("Custom1", "Chapter"),
            styled_para("Custom3", "Sub-sub-section")
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("styled.docx");
        std::fs::write(&path, build_docx(&body, Some(&heading_styles_xml()))).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("# Chapter"), "{}", extracted.text);
        assert!(
            extracted.text.contains("### Sub-sub-section"),
            "{}",
            extracted.text
        );
    }

    /// A document with no `word/styles.xml` must still resolve Word's
    /// self-describing built-in IDs rather than losing every heading.
    #[test]
    fn test_missing_styles_xml_falls_back_to_style_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nostyles.docx");
        std::fs::write(
            &path,
            build_docx(&styled_para("Heading2", "Fallback"), None),
        )
        .unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("## Fallback"), "{}", extracted.text);
    }

    #[test]
    fn test_heading_level_from_name() {
        assert_eq!(heading_level_from_name("Heading1"), Some(1));
        assert_eq!(heading_level_from_name("heading 3"), Some(3));
        assert_eq!(heading_level_from_name("Heading-2"), Some(2));
        // Word clamps deeper outline levels onto markdown's six.
        assert_eq!(heading_level_from_name("heading 9"), Some(6));
        // Run styles must never mark a paragraph as a heading.
        assert_eq!(heading_level_from_name("Heading 1 Char"), None);
        assert_eq!(heading_level_from_name("Normal"), None);
        assert_eq!(heading_level_from_name("TOC Heading"), None);
    }

    /// `w:outlineLvl` 9 means "body text", not "heading 10" — Word's built-in
    /// `TOC Heading` carries it, and treating it as a heading would mark the
    /// table of contents as document structure.
    #[test]
    fn test_outline_level_nine_is_not_a_heading() {
        assert_eq!(heading_level_from_outline("0"), Some(1));
        assert_eq!(heading_level_from_outline("8"), Some(6));
        assert_eq!(heading_level_from_outline("9"), None);

        let xml = format!(
            r#"<w:styles {NS}><w:style w:type="paragraph" w:styleId="TOCHeading"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="9"/></w:pPr></w:style></w:styles>"#
        );
        let styles = heading_styles_from_xml(&xml).unwrap();
        assert!(
            !styles.contains_key("TOCHeading"),
            "an explicit body-text outline level must override the style name: {styles:?}"
        );
    }

    #[test]
    fn test_heading_styles_from_xml() {
        let styles = heading_styles_from_xml(&heading_styles_xml()).unwrap();
        assert_eq!(styles.get("Custom1"), Some(&1));
        assert_eq!(styles.get("Custom3"), Some(&3));
        assert_eq!(styles.len(), 3, "{styles:?}");
    }

    /// A literal `|` in cell text would otherwise be indistinguishable from
    /// the column delimiter the row emits.
    #[test]
    fn test_cell_text_with_pipe_is_escaped() {
        let body = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a|b</w:t></w:r></w:p></w:tc>\
                    <w:tc><w:p><w:r><w:t>plain</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
        let text = paragraphs_from_document_xml(body, &HashMap::new()).unwrap();
        assert!(text.contains(r"| a\|b | plain |"), "{text}");
    }

    /// A cell holding several paragraphs must stay ONE column, not spill into
    /// extra rows.
    #[test]
    fn test_multi_paragraph_cell_stays_one_column() {
        let body = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>first</w:t></w:r></w:p>\
                    <w:p><w:r><w:t>second</w:t></w:r></w:p></w:tc>\
                    <w:tc><w:p><w:r><w:t>other</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
        let text = paragraphs_from_document_xml(body, &HashMap::new()).unwrap();
        assert!(text.contains("| first second | other |"), "{text}");
    }

    /// A table nested inside a cell must render inside that cell rather than
    /// escaping to the document body and desynchronising the outer row.
    #[test]
    fn test_nested_table_rows_stay_inside_the_outer_cell() {
        let inner = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>in</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
        let body = format!(
            "<w:tbl><w:tr><w:tc>{inner}</w:tc>\
             <w:tc><w:p><w:r><w:t>out</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
        );
        let text = paragraphs_from_document_xml(&body, &HashMap::new()).unwrap();

        let rows: Vec<&str> = text.lines().filter(|l| l.starts_with('|')).collect();
        assert_eq!(rows.len(), 2, "expected one outer row + delimiter: {text}");
        assert!(rows[0].contains("out"), "{text}");
        assert!(
            rows[0].contains("in"),
            "inner cell text must survive: {text}"
        );
    }

    /// Body text following a table must not read as one more row.
    #[test]
    fn test_paragraph_after_table_is_separated() {
        let body = format!(
            "{}<w:p><w:r><w:t>After the table.</w:t></w:r></w:p>",
            table_xml(2, 2)
        );
        let text = paragraphs_from_document_xml(&body, &HashMap::new()).unwrap();
        assert!(text.contains("|\n\nAfter the table."), "{text}");
    }

    /// A `.docx` read mid-write (Word/LibreOffice still saving) or truncated
    /// in transit ends inside its `<w:tbl>`. `</w:tr>` and `</w:tbl>` are the
    /// only paths that flush table content, so without an EOF unwind the
    /// document indexes "successfully" with every table cell missing — no
    /// error, no warning. The live file watcher reaches exactly this shape.
    #[test]
    fn test_unterminated_table_still_emits_its_content() {
        let truncated = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>important data</w:t></w:r></w:p>";
        let text = paragraphs_from_document_xml(truncated, &HashMap::new()).unwrap();
        assert!(
            text.contains("important data"),
            "truncated table content was silently dropped: {text:?}"
        );

        // The EOF unwind is a second exit from the parse loop, so it must
        // produce byte-identical output to the well-formed close path rather
        // than an approximation of it.
        let closed = format!("{truncated}</w:tc></w:tr></w:tbl>");
        assert_eq!(
            text,
            paragraphs_from_document_xml(&closed, &HashMap::new()).unwrap(),
            "EOF unwind must match the normal close path exactly"
        );
    }

    /// A partially-written cell (EOF before `</w:tc>`) is the same failure one
    /// element deeper.
    #[test]
    fn test_unterminated_row_matches_closed_row() {
        let truncated = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc>\
                         <w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p>";
        let text = paragraphs_from_document_xml(truncated, &HashMap::new()).unwrap();
        assert_eq!(
            text,
            paragraphs_from_document_xml(
                &format!("{truncated}</w:tc></w:tr></w:tbl>"),
                &HashMap::new()
            )
            .unwrap()
        );
        assert!(text.contains("| a | b |"), "{text}");
    }

    /// `tables` grows one entry per `<w:tbl>`, so a crafted `document.xml`
    /// sitting under the 50 MiB cap could otherwise push it to millions of
    /// allocated contexts. Past the cap the table degrades to plain text —
    /// content is preserved, the stack is not.
    #[test]
    fn test_table_nesting_depth_is_capped() {
        let depth = MAX_TABLE_NESTING_DEPTH as usize + 10;
        let mut body = String::new();
        for _ in 0..depth {
            body.push_str("<w:tbl><w:tr><w:tc>");
        }
        body.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
        for _ in 0..depth {
            body.push_str("</w:tc></w:tr></w:tbl>");
        }
        let text = paragraphs_from_document_xml(&body, &HashMap::new()).unwrap();

        assert!(
            text.contains("deep"),
            "content past the depth cap must degrade to text, not vanish: {text}"
        );
        // One delimiter per TRACKED table, and the cap is what bounds that
        // count. Counting occurrences rather than LINES matters: a nested
        // table renders inside its enclosing cell, where `normalize_cell`
        // collapses it onto one line.
        let delimiters = text.matches("---").count();
        assert!(
            delimiters <= MAX_TABLE_NESTING_DEPTH as usize,
            "nesting past the cap still allocated a table context: {delimiters} delimiters for depth {depth}"
        );
    }

    /// A malformed nested `<w:p>` must not discard the outer paragraph's
    /// accumulated text.
    #[test]
    fn test_malformed_nested_paragraph_keeps_outer_text() {
        let body = "<w:p><w:r><w:t>outer </w:t></w:r>\
                    <w:p><w:r><w:t>inner</w:t></w:r></w:p></w:p>";
        let text = paragraphs_from_document_xml(body, &HashMap::new()).unwrap();
        assert!(
            text.contains("outer"),
            "outer paragraph text was discarded: {text:?}"
        );
        assert!(text.contains("inner"), "{text:?}");
    }

    #[test]
    fn test_paragraphs_from_document_xml_basic() {
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t>Just one paragraph.</w:t></w:r></w:p></w:body></w:document>"#
        );
        let text = paragraphs_from_document_xml(&xml, &HashMap::new()).unwrap();
        assert_eq!(text.trim(), "Just one paragraph.");
    }

    #[test]
    fn test_oversized_document_xml_rejected_by_declared_size() {
        // declared size over the cap must be rejected BEFORE any decompression.
        let data = b"whatever";
        let result = read_entry_bounded(
            std::io::Cursor::new(&data[..]),
            1000,
            DOCUMENT_XML_PATH,
            100,
        );
        let err = result.expect_err("declared size over cap must error");
        assert!(err.to_string().contains("over the 100 byte cap"), "{err}");
    }

    #[test]
    fn test_bounded_read_rejects_underdeclared_entry() {
        // A lying size field (declares under the cap, actually decompresses
        // past it) must still be stopped by the Read::take hard bound.
        let data = vec![b'x'; 200];
        let result = read_entry_bounded(std::io::Cursor::new(data), 50, DOCUMENT_XML_PATH, 100);
        let err = result.expect_err("stream past cap must error");
        assert!(err.to_string().contains("decompressed past"), "{err}");
    }

    #[test]
    fn test_bounded_read_accepts_within_cap() {
        let data = b"hello world";
        let text = read_entry_bounded(
            std::io::Cursor::new(&data[..]),
            data.len() as u64,
            DOCUMENT_XML_PATH,
            100,
        )
        .unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_paragraphs_from_document_xml_multiple_runs_per_paragraph() {
        // A single paragraph split across multiple <w:r> runs (e.g. bold +
        // plain text) must be concatenated without an artificial break.
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p></w:body></w:document>"#
        );
        let text = paragraphs_from_document_xml(&xml, &HashMap::new()).unwrap();
        assert_eq!(text.trim(), "Hello world");
    }

    /// Regression test for the `BytesText::unescape()` → `.decode()` +
    /// `escape::unescape()` replacement (issue #3367, quick-xml 0.41 bump):
    /// the two-step decode-then-unescape path must still resolve XML
    /// entities (named and numeric) exactly as the removed single-call API
    /// did — an equivalence that was verified by hand against vendored
    /// sources but had no direct test coverage.
    #[test]
    fn test_paragraphs_from_document_xml_unescapes_entities() {
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t>Tom &amp; Jerry: 1 &lt; 2 &gt; 0, caf&#233;</w:t></w:r></w:p></w:body></w:document>"#
        );
        let text = paragraphs_from_document_xml(&xml, &HashMap::new()).unwrap();
        assert_eq!(text.trim(), "Tom & Jerry: 1 < 2 > 0, café");
    }

    #[test]
    fn test_missing_document_xml_errors() {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("readme.txt", opts).unwrap();
            zip.write_all(b"not a real docx").unwrap();
            zip.finish().unwrap();
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bad.docx");
        std::fs::write(&path, buf).unwrap();

        let result = extract(&path);
        assert!(
            result.is_err(),
            "a zip without word/document.xml must error"
        );
    }

    /// Regression test for RUSTSEC-2026-0194 (issue #3367): pre-patch
    /// quick-xml checked start-tag attributes for duplicate names in
    /// quadratic time, so a tag with a large number of attributes (duplicate
    /// or not) could pin CPU well within the existing size caps. Bounded
    /// join turns a reintroduced quadratic-time regression into a
    /// deterministic test failure instead of a slow/hung CI job.
    #[test]
    fn test_pathological_attribute_count_does_not_hang() {
        let n = 20_000;
        let attrs: String = (0..n).map(|i| format!(r#" a{i}="v""#)).collect();
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t{attrs}>hi</w:t></w:r></w:p></w:body></w:document>"#
        );

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = paragraphs_from_document_xml(&xml, &HashMap::new());
            let _ = tx.send(result.is_ok());
        });
        let completed = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert!(
            completed.is_ok(),
            "a tag with many attributes must parse (Ok or Err) in bounded time, not hang"
        );
    }

    #[test]
    fn test_not_a_zip_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-zip.docx");
        std::fs::write(&path, b"this is definitely not a zip file").unwrap();

        let result = extract(&path);
        assert!(result.is_err(), "non-zip input must error");
    }
}

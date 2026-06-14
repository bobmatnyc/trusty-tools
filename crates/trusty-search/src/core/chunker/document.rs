//! Structured-document chunkers for non-code file formats.
//!
//! Why: code-aware AST chunking is the wrong tool for prose, config, and log
//! formats; sliding-window chunking shreds heading structure. Format-aware
//! chunking yields semantically coherent BM25/vector candidates.
//!
//! What: `chunk_document(file, content) -> Option<Vec<RawChunk>>` dispatches
//! on file extension to per-format chunkers:
//!   - md/mdx  → section-per-heading (`chunk_markdown`)
//!   - yaml/yml → top-level key sections (`chunk_yaml`)
//!   - toml    → `[section]` blocks (`chunk_toml`)
//!   - json    → whole file if < 500 lines, otherwise skip (`chunk_json`)
//!   - txt/log → blank-line paragraphs, capped at 50 lines/chunk (`chunk_plaintext`)
//!   - xml     → top-level child elements (`chunk_xml`)
//!
//! Returns `None` for unknown extensions so the caller can fall back to the
//! sliding-window chunker.
//!
//! Test: see `test_chunk_markdown_*`, `test_chunk_yaml_*`, etc. in
//! `core/chunker/tests.rs`.

use super::types::{ChunkType, RawChunk};

/// Maximum lines for a JSON file to be indexed as a single chunk. Files
/// larger than this are skipped (JSON is hard to chunk meaningfully).
const JSON_MAX_LINES: usize = 500;

/// Maximum lines per plaintext / log chunk. Long paragraphs are split.
const PLAINTEXT_MAX_LINES: usize = 50;

/// Structured document chunker dispatcher.
///
/// Why: code-aware chunking is the wrong tool for prose, config, and log
/// formats; sliding-window chunking shreds heading structure. Format-aware
/// chunking yields semantically coherent BM25/vector candidates.
/// What: dispatches on extension to per-format chunkers; returns `None` for
/// unknown extensions so the caller can fall back to the sliding-window chunker.
/// Test: `test_chunk_document_dispatch` in `core/chunker/tests.rs`.
pub fn chunk_document(file: &str, content: &str) -> Option<Vec<RawChunk>> {
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let chunks = match ext.as_str() {
        "md" | "mdx" => chunk_markdown(file, content),
        "yaml" | "yml" => chunk_yaml(file, content),
        "toml" => chunk_toml(file, content),
        "json" => chunk_json(file, content)?,
        "txt" | "log" => chunk_plaintext(file, content),
        "xml" => chunk_xml(file, content),
        _ => return None,
    };
    Some(chunks)
}

/// Build a generic document chunk with a specific language tag and chunk type.
///
/// Why: all document chunkers produce the same struct shape; this helper
/// avoids repeating the struct literal and guarantees consistent ID derivation.
/// What: constructs a `RawChunk` whose `id` embeds the chunk type and name
/// when available, falling back to `{file}:{start}:{end}`.
/// Test: covered transitively by every document-format test.
pub(super) fn document_chunk(
    file: &str,
    start_line: usize,
    end_line: usize,
    content: String,
    function_name: Option<String>,
    language: &str,
    chunk_type: ChunkType,
) -> RawChunk {
    let id = match &function_name {
        Some(name) if !name.is_empty() => {
            format!("{file}::{}::{name}::{start_line}", chunk_type.as_str())
        }
        _ => format!("{file}:{start_line}:{end_line}"),
    };
    RawChunk {
        id,
        file: file.to_string(),
        start_line,
        end_line,
        content,
        function_name,
        language: Some(language.to_string()),
        chunk_type,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// Markdown: split on `^#+ ` headings. Each heading + its body becomes one
/// chunk. Content before the first heading becomes a leading chunk.
///
/// Why: markdown files have strong natural section boundaries; splitting at
/// headings keeps each chunk topically coherent.
/// What: walks lines tracking fenced code blocks (so `#` inside a fence is
/// not treated as a heading), flushes a chunk at each heading boundary.
/// Test: `test_chunk_markdown_sections` and
/// `test_chunk_markdown_ignores_hash_in_code_fence`.
pub(super) fn chunk_markdown(file: &str, content: &str) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RawChunk> = Vec::new();
    let mut section_start = 0usize;
    let mut section_heading: Option<String> = None;
    let mut in_code_fence = false;

    let flush = |out: &mut Vec<RawChunk>,
                 start: usize,
                 end: usize,
                 heading: &Option<String>,
                 lines: &[&str]| {
        if start >= end {
            return;
        }
        let text = lines[start..end].join("\n");
        if text.trim().is_empty() {
            return;
        }
        out.push(document_chunk(
            file,
            start + 1,
            end,
            text,
            heading.clone(),
            "markdown",
            ChunkType::Docstring,
        ));
    };

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        // Track fenced code blocks so we don't treat `#` inside code as headings.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }
        if trimmed.starts_with('#') {
            // Heading line — flush previous section.
            flush(&mut out, section_start, i, &section_heading, &lines);
            // Extract heading text (strip leading #'s and whitespace).
            let heading = trimmed.trim_start_matches('#').trim().to_string();
            section_heading = if heading.is_empty() {
                None
            } else {
                Some(heading)
            };
            section_start = i;
        }
    }
    // Final section.
    flush(
        &mut out,
        section_start,
        lines.len(),
        &section_heading,
        &lines,
    );

    if out.is_empty() {
        // No content matched: fall back to a single whole-file chunk.
        out.push(document_chunk(
            file,
            1,
            lines.len(),
            content.to_string(),
            None,
            "markdown",
            ChunkType::Docstring,
        ));
    }
    out
}

/// YAML: split on top-level keys (lines starting at column 0 with `key:`).
/// Comments and blank lines are bundled with the following key's section.
///
/// Why: YAML files organise config under top-level keys; splitting there
/// gives BM25 a coherent key+value block per chunk.
/// What: delegates to `chunk_by_top_level_key` with a YAML key detector.
/// Test: `test_chunk_yaml_top_level_keys`.
pub(super) fn chunk_yaml(file: &str, content: &str) -> Vec<RawChunk> {
    chunk_by_top_level_key(file, content, "yaml", |line| {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        // Top-level YAML key: starts at col 0, not indented, contains ':'.
        if !line.starts_with(|c: char| c.is_whitespace() || c == '-') {
            if let Some(idx) = trimmed.find(':') {
                let key = trimmed[..idx].trim();
                if !key.is_empty() && !key.contains(' ') {
                    return Some(key.to_string());
                }
            }
        }
        None
    })
}

/// TOML: split on `[section]` and `[[array.section]]` headers at column 0.
///
/// Why: TOML files have explicit section headers; splitting there keeps each
/// chunk to one configuration section.
/// What: delegates to `chunk_by_top_level_key` with a TOML header detector.
/// Test: `test_chunk_toml_sections`.
pub(super) fn chunk_toml(file: &str, content: &str) -> Vec<RawChunk> {
    chunk_by_top_level_key(file, content, "toml", |line| {
        let trimmed = line.trim_end();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let inner = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            if !inner.is_empty() {
                return Some(inner);
            }
        }
        None
    })
}

/// Generic top-level-key chunker.
///
/// Why: YAML and TOML share the same "flush on header" structure; a shared
/// helper avoids code duplication.
/// What: `header_of(line)` returns `Some(name)` when the line starts a new
/// section. Content before the first header is emitted as a leading "preamble"
/// chunk. Emits one chunk per section with the section name as `function_name`.
/// Test: `test_chunk_yaml_top_level_keys` and `test_chunk_toml_sections`.
fn chunk_by_top_level_key(
    file: &str,
    content: &str,
    language: &str,
    header_of: impl Fn(&str) -> Option<String>,
) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RawChunk> = Vec::new();
    let mut section_start = 0usize;
    let mut section_name: Option<String> = None;

    let flush = |out: &mut Vec<RawChunk>,
                 start: usize,
                 end: usize,
                 name: &Option<String>,
                 lines: &[&str]| {
        if start >= end {
            return;
        }
        let text = lines[start..end].join("\n");
        if text.trim().is_empty() {
            return;
        }
        out.push(document_chunk(
            file,
            start + 1,
            end,
            text,
            name.clone(),
            language,
            ChunkType::Constant,
        ));
    };

    for (i, line) in lines.iter().enumerate() {
        if let Some(name) = header_of(line) {
            flush(&mut out, section_start, i, &section_name, &lines);
            section_name = Some(name);
            section_start = i;
        }
    }
    flush(&mut out, section_start, lines.len(), &section_name, &lines);

    if out.is_empty() {
        out.push(document_chunk(
            file,
            1,
            lines.len(),
            content.to_string(),
            None,
            language,
            ChunkType::Constant,
        ));
    }
    out
}

/// JSON: if the file has fewer than `JSON_MAX_LINES` lines, emit a single
/// whole-file chunk. Otherwise return `Some(empty)` to signal "skip indexing".
///
/// Why: large JSON files dominate BM25 with structural punctuation noise;
/// small ones are genuinely useful to index as a single chunk.
/// What: counts lines; returns `Some(vec![one_chunk])` if small,
/// `Some(vec![])` if large (skip), and never returns `None`.
/// Test: `test_chunk_json_small_file_single_chunk` and
/// `test_chunk_json_large_file_skipped`.
pub(super) fn chunk_json(file: &str, content: &str) -> Option<Vec<RawChunk>> {
    let line_count = content.lines().count();
    if line_count == 0 {
        return Some(Vec::new());
    }
    if line_count >= JSON_MAX_LINES {
        // Skip large JSON: it's effectively un-chunkable and dominates BM25
        // with structural punctuation noise.
        return Some(Vec::new());
    }
    Some(vec![document_chunk(
        file,
        1,
        line_count,
        content.to_string(),
        None,
        "json",
        ChunkType::Constant,
    )])
}

/// Plaintext / logs: split on blank-line paragraphs, cap at
/// `PLAINTEXT_MAX_LINES` per chunk. Paragraphs longer than the cap are split
/// into successive fixed-size sub-chunks.
///
/// Why: log files have natural paragraph boundaries at blank lines; splitting
/// there keeps each chunk to one log entry or paragraph.
/// What: walks lines accumulating a "buffer" until a blank line is hit, then
/// flushes sub-chunks of up to `PLAINTEXT_MAX_LINES` lines.
/// Test: `test_chunk_plaintext_paragraphs` and
/// `test_chunk_plaintext_caps_at_50_lines`.
pub(super) fn chunk_plaintext(file: &str, content: &str) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let lang = match std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "log" => "log",
        _ => "text",
    };
    let mut out: Vec<RawChunk> = Vec::new();
    let mut buf_start: Option<usize> = None;

    let push_buf =
        |out: &mut Vec<RawChunk>, start: usize, end: usize, lines: &[&str], lang: &str| {
            // Split into fixed PLAINTEXT_MAX_LINES windows (no overlap).
            let mut s = start;
            while s < end {
                let e = (s + PLAINTEXT_MAX_LINES).min(end);
                let text = lines[s..e].join("\n");
                if !text.trim().is_empty() {
                    out.push(document_chunk(
                        file,
                        s + 1,
                        e,
                        text,
                        None,
                        lang,
                        ChunkType::Code,
                    ));
                }
                s = e;
            }
        };

    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            if let Some(start) = buf_start.take() {
                push_buf(&mut out, start, i, &lines, lang);
            }
        } else if buf_start.is_none() {
            buf_start = Some(i);
        }
    }
    if let Some(start) = buf_start {
        push_buf(&mut out, start, lines.len(), &lines, lang);
    }

    if out.is_empty() {
        out.push(document_chunk(
            file,
            1,
            lines.len(),
            content.to_string(),
            None,
            lang,
            ChunkType::Code,
        ));
    }
    out
}

/// XML: split on top-level child elements via a minimal depth-tracking parser.
/// Each direct child of the root becomes one chunk; the XML prolog and root
/// open/close tags are emitted as separate trivial chunks if present.
///
/// Why: XML files have strong element-level structure; chunking at top-level
/// children keeps each chunk to one entity (e.g. one `<book>`).
/// What: walks lines tracking open/close depth; at depth 1 a new opening tag
/// starts a child, which is flushed when it closes back to depth 1. Depth is
/// clamped at 0 so malformed input (orphan closing tags, more closes than
/// opens) never produces a negative depth and the emit guard never fires
/// spuriously on garbage input (closes #1181).
/// Test: `test_chunk_xml_top_level_children`,
/// `test_chunk_xml_malformed_leading_close`,
/// `test_chunk_xml_malformed_extra_closes`,
/// `test_chunk_xml_well_formed_unchanged`.
pub(super) fn chunk_xml(file: &str, content: &str) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    // Walk lines tracking element-open depth (excluding self-closing and
    // closing tags). Depth==1 is "inside root, at top of children".
    let mut out: Vec<RawChunk> = Vec::new();
    let mut depth: i32 = 0;
    let mut child_start: Option<usize> = None;
    let mut child_name: Option<String> = None;

    for (i, line) in lines.iter().enumerate() {
        let opens = count_xml_opens(line);
        let closes = count_xml_closes(line);

        // If we're at depth 1 with no active child and this line opens a new
        // element, start tracking.
        if depth == 1 && child_start.is_none() && opens > closes {
            child_start = Some(i);
            child_name = first_xml_tag_name(line);
        }

        let prev_depth = depth;
        depth += opens as i32;
        depth -= closes as i32;
        // Clamp to 0: malformed input (orphan closing tags, more closes than
        // opens) must not drive depth negative and trigger spurious emits.
        depth = depth.max(0);

        // Closed a top-level child: emit chunk.
        if let Some(start) = child_start {
            if depth <= 1 && prev_depth >= 1 && i >= start {
                let text = lines[start..=i].join("\n");
                if !text.trim().is_empty() {
                    out.push(document_chunk(
                        file,
                        start + 1,
                        i + 1,
                        text,
                        child_name.clone(),
                        "xml",
                        ChunkType::Class,
                    ));
                }
                child_start = None;
                child_name = None;
            }
        }
    }

    if out.is_empty() {
        out.push(document_chunk(
            file,
            1,
            lines.len(),
            content.to_string(),
            None,
            "xml",
            ChunkType::Class,
        ));
    }
    out
}

/// Count element-opening tags on a line, excluding self-closing (`<foo/>`),
/// closing tags (`</foo>`), prolog (`<?xml ... ?>`), comments, and DOCTYPE.
///
/// Why: the XML chunker tracks open/close depth per-line to find top-level
/// children without a full XML parser.
/// What: walks bytes counting `<tag>` occurrences that are not self-closing,
/// closing, prolog, comment, or doctype.
/// Test: covered transitively by `test_chunk_xml_top_level_children`.
fn count_xml_opens(line: &str) -> usize {
    let mut count = 0usize;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Skip prolog/comment/doctype/closing.
            let rest = &line[i..];
            if rest.starts_with("<?")
                || rest.starts_with("<!--")
                || rest.starts_with("<!")
                || rest.starts_with("</")
            {
                i += 1;
                continue;
            }
            // Find the matching `>` and check if it's self-closing.
            if let Some(close) = rest.find('>') {
                let tag = &rest[..=close];
                if !tag.ends_with("/>") {
                    count += 1;
                }
                i += close + 1;
                continue;
            }
        }
        i += 1;
    }
    count
}

/// Count element-closing tags (`</foo>`) on a line.
///
/// Why: paired with `count_xml_opens` to track depth changes per line.
/// What: counts occurrences of `</` in the line string.
/// Test: covered transitively by `test_chunk_xml_top_level_children`.
fn count_xml_closes(line: &str) -> usize {
    line.matches("</").count()
}

/// Extract the first opening tag name from a line, e.g. `<book id="1">` → `book`.
///
/// Why: the XML chunker uses the first top-level child tag name as the chunk's
/// `function_name` so it appears in search results.
/// What: finds `<`, skips special tag types (`?`, `!`, `/`), then extracts
/// the tag name up to the first whitespace, `>`, or `/`.
/// Test: covered transitively by `test_chunk_xml_top_level_children`.
fn first_xml_tag_name(line: &str) -> Option<String> {
    let start = line.find('<')?;
    let rest = &line[start + 1..];
    if rest.starts_with('?') || rest.starts_with('!') || rest.starts_with('/') {
        return None;
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

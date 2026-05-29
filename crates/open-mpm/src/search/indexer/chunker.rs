//! Language detection and AST/markdown/fallback chunk extraction.
//!
//! Why: Separating extraction from persistence keeps the AST-walking code
//! pure and easy to unit-test without touching disk or the embedder.
//! What: Maps a file to a language tag, dispatches to the per-language
//! tree-sitter extractor (or markdown heading split, or sliding-window
//! fallback), and produces [`RawChunk`]s before embedding.
//! Test: See the `tests` submodule of the parent `indexer` module —
//! per-language chunking, markdown heading split, and fallback windows.

use std::path::Path;

use tree_sitter::{Language, Node, Parser};

/// Line-count target for the fallback chunker.
///
/// Why: When tree-sitter finds no function nodes (config files, pure-data
/// modules, markdown), we still want the file to be searchable. Larger
/// 150-line windows preserve more surrounding context per chunk so the
/// embedding captures relationships across a wider span (#376).
pub(crate) const FALLBACK_LINES_PER_CHUNK: usize = 150;

/// Stride between successive sliding-window chunks (~67% overlap with the
/// 150-line window).
///
/// Why: Hard windows risk splitting a logical block (loop, struct literal,
/// long match arm) at exactly the wrong line and losing it from semantic
/// matches. With a 150-line window and a 50-line stride every line lands
/// in three consecutive chunks so boundary context is preserved (#376).
/// What: Every chunk after the first starts `FALLBACK_STRIDE` lines after
/// the previous chunk's start. Last chunk is clipped to file length.
/// Test: `fallback_uses_overlapping_windows`.
pub(crate) const FALLBACK_STRIDE: usize = 50;

/// Raw chunk produced by the language-specific extractors before embedding.
///
/// Why: Separating extraction from persistence keeps the AST-walking code
/// pure and easy to unit-test without touching disk or the embedder.
#[derive(Debug, Clone)]
pub(crate) struct RawChunk {
    pub(crate) function_name: Option<String>,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) text: String,
}

/// Compute the manifest key used to track which chunk ids belong to a file.
///
/// Why: `remove_file` needs an O(1) lookup of the per-file chunk list so it
/// can delete precisely the entries that were inserted by `index_file`.
/// What: Returns `"manifest:{canonical_path}"`. Centralised here so the
/// write path and the delete path can't drift out of sync.
pub(crate) fn manifest_key(path: &Path) -> String {
    format!("manifest:{}", path.display())
}

/// Map a file to one of the supported language tags.
///
/// Why: Central switchboard keeps language detection consistent across
/// the indexer, the filter API, and the extractor. The optional `root`
/// argument lets us promote `AGENTS.md`/`CLAUDE.md` sitting directly at
/// the project root to the special `"agentconfig"` language so agent-
/// facing search queries surface them first.
/// What: If `root` is provided and `path`'s parent equals `root`, and the
/// (case-insensitive) filename is `AGENTS.md` or `CLAUDE.md`, returns
/// `"agentconfig"`. Otherwise maps the file extension to one of
/// `"rust"`/`"python"`/`"typescript"`/`"javascript"`/`"go"`/`"markdown"`,
/// or returns `None` for unknown extensions.
/// Test: `root_agents_md_gets_agentconfig_language`,
/// `subdir_agents_md_stays_markdown`, `claude_md_at_root_gets_agentconfig`,
/// plus implicit coverage from per-language chunking tests.
pub(crate) fn detect_language(path: &Path, root: Option<&Path>) -> Option<&'static str> {
    // Root-level AGENTS.md / CLAUDE.md promotion.
    if let Some(root) = root
        && let Some(parent) = path.parent()
        && parent == root
        && let Some(name) = path.file_name().and_then(|n| n.to_str())
    {
        let lower = name.to_ascii_lowercase();
        if lower == "agents.md" || lower == "claude.md" {
            return Some("agentconfig");
        }
    }
    match path.extension()?.to_str()? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some("cpp"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

/// Skip hidden directories and known build/vendor folders during walks.
///
/// Why: Indexing `.git`, `target/`, or `node_modules/` wastes time and
/// pollutes results with generated code.
/// What: Returns true for dotted names (except `.`/`..`) and a short
/// denylist of directory names.
pub(crate) fn is_hidden_or_skipped(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == "." || name == ".." {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    matches!(name, "target" | "node_modules" | "dist" | "build")
}

/// Truncate a string to at most `max` characters (by byte index of the
/// `max`-th char, not byte count), returning an owned `String`.
///
/// Why: Chunk text must stay under [`crate::search::indexer::MAX_CHUNK_CHARS`]
/// before embedding and before being written to redb. Using char indices
/// avoids slicing through a UTF-8 multibyte sequence.
/// What: If the string has fewer than `max` chars, returns it unchanged;
/// otherwise truncates and returns a new `String`.
/// Test: Implicit — large source files in indexing tests exercise it.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Dispatch to the per-language chunk extractor.
///
/// Why: Language-specific node kinds differ enough that sharing a single
/// traversal is awkward. Per-language functions keep each implementation
/// self-contained and readable.
/// What: Delegates to a language-specific helper; falls back to fixed
/// line windows if the helper returns zero chunks.
/// Test: `rust_function_chunking`, `python_function_chunking`,
/// `go_function_chunking`, `markdown_heading_chunking`,
/// `fallback_to_line_chunks_when_no_functions`.
pub(crate) fn extract_chunks_from_source(source: &str, language: &str) -> Vec<RawChunk> {
    let chunks = match language {
        "rust" => extract_tree_sitter(
            source,
            tree_sitter_rust::LANGUAGE.into(),
            &["function_item"],
        ),
        "python" => extract_tree_sitter(
            source,
            tree_sitter_python::LANGUAGE.into(),
            &["function_definition", "async_function_definition"],
        ),
        "javascript" => extract_tree_sitter(
            source,
            tree_sitter_javascript::LANGUAGE.into(),
            &[
                "function_declaration",
                "function_expression",
                "arrow_function",
                "method_definition",
            ],
        ),
        "typescript" => extract_tree_sitter(
            source,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &[
                "function_declaration",
                "function_expression",
                "arrow_function",
                "method_definition",
                "method_signature",
            ],
        ),
        "go" => extract_tree_sitter(
            source,
            tree_sitter_go::LANGUAGE.into(),
            &["function_declaration", "method_declaration"],
        ),
        "java" => extract_tree_sitter(
            source,
            tree_sitter_java::LANGUAGE.into(),
            &["method_declaration", "constructor_declaration"],
        ),
        "c" => extract_tree_sitter(
            source,
            tree_sitter_c::LANGUAGE.into(),
            &["function_definition"],
        ),
        "cpp" => extract_tree_sitter(
            source,
            tree_sitter_cpp::LANGUAGE.into(),
            &["function_definition"],
        ),
        "markdown" | "agentconfig" => return extract_markdown_headings(source),
        _ => Vec::new(),
    };
    if chunks.is_empty() {
        fallback_line_chunks(source)
    } else {
        chunks
    }
}

/// Generic tree-sitter extractor keyed by a list of target node kinds.
///
/// Why: All four code languages share the same walk-and-capture pattern;
/// only the set of "interesting" node kinds differs.
/// What: Parses `source`, then does a preorder walk; whenever a node's
/// `kind()` matches one of `target_kinds`, emits a `RawChunk` with the
/// node's byte range and `name`-field text (if present).
/// Test: Covered by per-language tests.
fn extract_tree_sitter(source: &str, language: Language, target_kinds: &[&str]) -> Vec<RawChunk> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        tracing::warn!("tree-sitter set_language failed");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_for_kinds(tree.root_node(), source, target_kinds, &mut out);
    out
}

/// Preorder walk that captures nodes whose `kind()` is in `targets`.
///
/// Why: tree-sitter cursors are awkward to use recursively; a plain
/// function is easier to read. Once we hit a captured node we still
/// descend — nested functions inside methods (e.g., JS closures in
/// methods) should also be indexed.
/// What: Recursive DFS. For each match, extract the node's text and its
/// `name` child's text; push a [`RawChunk`].
fn walk_for_kinds(node: Node, source: &str, targets: &[&str], out: &mut Vec<RawChunk>) {
    if targets.contains(&node.kind())
        && let Some(chunk) = node_to_chunk(node, source)
    {
        out.push(chunk);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_kinds(child, source, targets, out);
    }
}

/// Convert a tree-sitter node into a [`RawChunk`].
///
/// Why: The body of the captured-node handling is identical across
/// languages; factoring it out keeps `walk_for_kinds` small.
/// What: Extracts UTF-8 text via byte range, reads the `name` field if
/// present, and converts 0-indexed rows to 1-indexed line numbers.
fn node_to_chunk(node: Node, source: &str) -> Option<RawChunk> {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let text = source.get(start_byte..end_byte)?.to_string();
    let name = node
        .child_by_field_name("name")
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string());
    Some(RawChunk {
        function_name: name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        text,
    })
}

/// Split a markdown document at `##` headings.
///
/// Why: Tree-sitter for markdown is overkill here; heading-based splits
/// are exactly what most docs want anyway, and the regex fallback keeps
/// the dependency graph smaller.
/// What: Iterates lines; every line starting with `## ` (exactly two `#`
/// and a space) closes the previous chunk and opens a new one.
/// Test: `markdown_heading_chunking`.
fn extract_markdown_headings(source: &str) -> Vec<RawChunk> {
    let mut chunks = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    let flush = |start: usize,
                 end: usize,
                 name: Option<String>,
                 lines: &[&str],
                 out: &mut Vec<RawChunk>| {
        if lines.is_empty() {
            return;
        }
        out.push(RawChunk {
            function_name: name,
            start_line: start,
            end_line: end,
            text: lines.join("\n"),
        });
    };

    for (idx, line) in source.lines().enumerate() {
        let one_indexed = idx + 1;
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(start) = current_start {
                flush(
                    start,
                    one_indexed - 1,
                    current_name.take(),
                    &current_lines,
                    &mut chunks,
                );
            }
            current_start = Some(one_indexed);
            current_name = Some(rest.trim().to_string());
            current_lines.clear();
            current_lines.push(line);
        } else if current_start.is_some() {
            current_lines.push(line);
        }
    }
    if let Some(start) = current_start {
        let end = source.lines().count().max(start);
        flush(start, end, current_name, &current_lines, &mut chunks);
    }
    chunks
}

/// Sliding-window line chunker used when AST extraction finds nothing.
///
/// Why: Config files, data-only modules, and unsupported languages still
/// deserve to be searchable. Overlapping windows (50 lines, 25-line stride)
/// preserve cross-boundary context — every line appears in two consecutive
/// chunks so a tight code block split across a window boundary still scores
/// well in semantic search.
/// What: Emits one [`RawChunk`] per stride step, each
/// [`FALLBACK_LINES_PER_CHUNK`] lines wide (or shorter at end-of-file), with
/// no function name and 1-indexed start/end lines.
/// Test: `fallback_uses_overlapping_windows`.
pub(crate) fn fallback_line_chunks(source: &str) -> Vec<RawChunk> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    // For files smaller than one window, emit a single chunk covering the
    // whole file rather than producing zero strides.
    if lines.len() <= FALLBACK_LINES_PER_CHUNK {
        return vec![RawChunk {
            function_name: None,
            start_line: 1,
            end_line: lines.len(),
            text: lines.join("\n"),
        }];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let end = (i + FALLBACK_LINES_PER_CHUNK).min(lines.len());
        out.push(RawChunk {
            function_name: None,
            start_line: i + 1,
            end_line: end,
            text: lines[i..end].join("\n"),
        });
        // Stop once the window has reached EOF; otherwise advance by stride.
        if end == lines.len() {
            break;
        }
        i += FALLBACK_STRIDE;
    }
    out
}
